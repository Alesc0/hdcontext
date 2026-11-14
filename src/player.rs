use crate::{ai::Moment, report::Clip, segments::timestamp};
use anyhow::{Context, Result, ensure};
use eframe::egui;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_video::prelude::*;
use std::path::{Path, PathBuf};

pub struct Player {
    duration: f64,
    pending_seek: Option<f64>,
    frame_seconds: f64,
    frame_duration: f64,
    video_height: f32,
    fill_player: bool,
    overlay: OverlayState,
    controls_rect: Option<egui::Rect>,
    editor_controls_height: f32,
    viewing_mode: bool,
    timeline_view: Option<(f64, f64)>,
    edits: EditHistory,
    history_action: i8,
    source: PathBuf,
    selected: usize,
    loop_recording: bool,
    stop_at: Option<f64>,
    pipeline: gst::Element,
    sink: gstreamer_app::AppSink,
    texture: Option<egui::TextureHandle>,
    playing: bool,
    volume: f64,
    error: Option<String>,
}

impl Player {
    pub fn open_source(source: PathBuf, duration: f64, selected: usize) -> Result<Self> {
        let mut player = Self::open_with_audio_sink(source, duration, None)?;
        player.selected = selected;
        Ok(player)
    }

    pub fn focus_range(&mut self, start: f64, end: f64) {
        let padding = ((end - start) * 0.3).max(2.0);
        self.timeline_view = Some((
            (start - padding).max(0.0),
            (end + padding).min(self.duration),
        ));
    }
    pub fn source(&self) -> &Path {
        &self.source
    }

    fn open_with_audio_sink(
        source: PathBuf,
        duration: f64,
        audio_sink: Option<&gst::Element>,
    ) -> Result<Self> {
        gst::init().context("Could not initialize GStreamer for video playback")?;
        ensure!(
            gst::ElementFactory::find("qtdemux").is_some(),
            "MP4 playback plugin is missing. Install GStreamer's Good plugins, including qtdemux"
        );
        let sink = gstreamer_app::AppSink::builder()
            .caps(
                &gst::Caps::builder("video/x-raw")
                    .field("format", "RGBA")
                    .field("width", gst::IntRange::<i32>::new(1, 1280))
                    .field("height", gst::IntRange::<i32>::new(1, 720))
                    .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
                    .build(),
            )
            .max_buffers(2)
            .drop(true)
            .sync(true)
            .wait_on_eos(false)
            .build();
        // Scale decoded preview frames before copying RGBA pixels to the UI. The source
        // file, analysis media and full-resolution exports are unaffected.
        let scaler = gst::ElementFactory::make("videoscale")
            .property("add-borders", true)
            .build()
            .context("GStreamer videoscale is unavailable")?;
        let uri = gst::glib::filename_to_uri(&source, None)?;
        let pipeline = gst::ElementFactory::make("playbin")
            .property("uri", uri.as_str())
            .property("video-sink", &sink)
            .property("video-filter", &scaler)
            .build()
            .context("GStreamer playbin is unavailable; install the playback plugins")?;
        if let Some(sink) = audio_sink {
            pipeline.set_property("audio-sink", sink);
        }
        pipeline
            .set_state(gst::State::Playing)
            .context("Could not start preview playback")?;
        Ok(Self {
            duration,
            pending_seek: None,
            frame_seconds: 0.0,
            frame_duration: 1.0 / 60.0,
            video_height: 320.0,
            fill_player: true,
            overlay: OverlayState::default(),
            controls_rect: None,
            editor_controls_height: 390.0,
            viewing_mode: false,
            timeline_view: None,
            edits: EditHistory::default(),
            history_action: 0,
            source,
            selected: 0,
            loop_recording: false,
            stop_at: None,
            pipeline,
            sink,
            texture: None,
            playing: true,
            volume: 1.0,
            error: None,
        })
    }

    fn seek(&self, seconds: f64) -> Result<()> {
        self.pipeline.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::ClockTime::from_seconds_f64(
                seconds.clamp(0.0, (self.duration - 0.000001).max(0.0)),
            ),
        )?;
        Ok(())
    }

    fn frame(&mut self, ctx: &egui::Context) -> Result<()> {
        if self.pending_seek.is_some() && self.pipeline.current_state() >= gst::State::Paused {
            let seconds = self.pending_seek.take().unwrap();
            self.seek(seconds)?;
        }
        if let Some(bus) = self.pipeline.bus() {
            for message in bus.iter_filtered(&[gst::MessageType::Error, gst::MessageType::Eos]) {
                match message.view() {
                    gst::MessageView::Error(error) => {
                        self.error = Some(error.error().to_string());
                        self.playing = false;
                    }
                    gst::MessageView::Eos(_) => {
                        if self.loop_recording && self.stop_at.is_none() {
                            self.seek(0.0)?;
                            self.pipeline.set_state(gst::State::Playing)?;
                        } else {
                            self.pipeline.set_state(gst::State::Paused)?;
                            self.playing = false;
                        }
                    }
                    _ => {}
                }
            }
        }
        // Pulling a normal sample clears appsink's preroll slot, even when paused.
        // Prefer preroll after a paused seek so its newly decoded frame is not discarded.
        let sample = if self.playing {
            self.sink
                .try_pull_sample(gst::ClockTime::ZERO)
                .or_else(|| self.sink.try_pull_preroll(gst::ClockTime::ZERO))
        } else {
            self.sink
                .try_pull_preroll(gst::ClockTime::ZERO)
                .or_else(|| self.sink.try_pull_sample(gst::ClockTime::ZERO))
        };
        if let Some(sample) = sample {
            self.update_texture(ctx, &sample)?;
        }
        Ok(())
    }

    fn update_texture(&mut self, ctx: &egui::Context, sample: &gst::Sample) -> Result<()> {
        let info = gstreamer_video::VideoInfo::from_caps(
            sample.caps().context("Preview frame has no format")?,
        )?;
        let frame = gstreamer_video::VideoFrameRef::from_buffer_ref_readable(
            sample.buffer().context("Preview frame has no pixels")?,
            &info,
        )?;
        let fps = info.fps();
        if fps.numer() > 0 && fps.denom() > 0 {
            self.frame_duration = f64::from(fps.denom()) / f64::from(fps.numer());
        }
        if let Some(pts) = sample.buffer().and_then(|buffer| buffer.pts()) {
            self.frame_seconds = sample
                .segment()
                .and_then(|segment| segment.downcast_ref::<gst::ClockTime>())
                .and_then(|segment| segment.to_stream_time(pts))
                .unwrap_or(pts)
                .seconds_f64();
        }
        let width = info.width() as usize;
        let height = info.height() as usize;
        let stride = frame.plane_stride()[0] as usize;
        let data = frame.plane_data(0)?;
        let mut rgba = Vec::with_capacity(width * height * 4);
        for row in 0..height {
            rgba.extend_from_slice(&data[row * stride..row * stride + width * 4]);
        }
        let image = egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba);
        if let Some(texture) = &mut self.texture {
            texture.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.texture =
                Some(ctx.load_texture("moment-preview", image, egui::TextureOptions::LINEAR));
        }
        Ok(())
    }

    fn toggle_playback(&mut self, position: f64) {
        self.stop_at = None;
        let result = if self.playing {
            self.pipeline.set_state(gst::State::Paused)
        } else {
            if position >= self.duration - 0.05 {
                self.pending_seek = Some(0.0);
            }
            self.pipeline.set_state(gst::State::Playing)
        };
        match result {
            Ok(_) => self.playing = !self.playing,
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn step_frame(&mut self, forward: bool) {
        self.stop_at = None;
        if let Err(error) = self.pipeline.set_state(gst::State::Paused) {
            self.error = Some(error.to_string());
            return;
        }
        self.playing = false;
        if forward {
            if !self.sink.send_event(gst::event::Step::new(
                gst::format::Buffers::from_u64(1),
                1.0,
                true,
                false,
            )) {
                self.error = Some("Decoder could not advance a frame".into());
            }
        } else if let Err(error) =
            self.seek((self.frame_seconds - self.frame_duration + 0.000001).max(0.0))
        {
            self.error = Some(error.to_string());
        }
    }

    fn keyboard(&mut self, ctx: &egui::Context, position: f64) {
        if text_input_active(ctx) || egui::Popup::is_any_open(ctx) {
            return;
        }
        if ctx.input(|input| {
            input.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Key { pressed: true, .. }
                        | egui::Event::Text(_)
                        | egui::Event::Paste(_)
                )
            })
        }) {
            self.overlay.last_activity = Some(ctx.input(|input| input.time));
        }
        ctx.input_mut(|input| {
            let control = egui::Modifiers::COMMAND;
            if input.consume_key(control | egui::Modifiers::SHIFT, egui::Key::Z)
                || input.consume_key(control, egui::Key::Y)
            {
                self.history_action = 1;
            } else if input.consume_key(control, egui::Key::Z) {
                self.history_action = -1;
            }
        });
        let pressed = ctx.input_mut(|input| {
            [
                egui::Key::Space,
                egui::Key::Comma,
                egui::Key::Period,
                egui::Key::ArrowLeft,
                egui::Key::ArrowRight,
                egui::Key::Home,
                egui::Key::End,
            ]
            .map(|key| input.consume_key(egui::Modifiers::NONE, key))
        });
        if pressed[0] {
            self.toggle_playback(position);
        }
        if pressed[1] {
            self.step_frame(false);
        }
        if pressed[2] {
            self.step_frame(true);
        }
        let target = if pressed[3] {
            Some((position - 5.0).max(0.0))
        } else if pressed[4] {
            Some((position + 5.0).min(self.duration))
        } else if pressed[5] {
            Some(0.0)
        } else if pressed[6] {
            Some((self.duration - self.frame_duration).max(0.0))
        } else {
            None
        };
        if let Some(seconds) = target {
            self.stop_at = None;
            self.pending_seek = Some(seconds);
        }
    }

    pub fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let position = self
            .pipeline
            .query_position::<gst::ClockTime>()
            .map(|p| p.seconds_f64())
            .unwrap_or(self.frame_seconds);
        self.keyboard(ctx, position);
    }

    pub fn set_viewing_mode(&mut self, viewing: bool) {
        if self.viewing_mode != viewing {
            self.viewing_mode = viewing;
            self.overlay.last_activity = None;
            self.controls_rect = None;
            if viewing {
                self.timeline_view = None;
            }
        }
    }

    fn volume_controls(&mut self, ui: &mut egui::Ui) -> egui::Rect {
        let muted = self.pipeline.property::<bool>("mute");
        if ui.button(if muted { "Unmute" } else { "Mute" }).clicked() {
            self.pipeline.set_property("mute", !muted);
        }
        let slider = ui.add(
            egui::Slider::new(&mut self.volume, 0.0..=1.0)
                .text("Volume")
                .show_value(false),
        );
        if slider.changed() {
            self.pipeline.set_property("volume", self.volume);
            if self.volume > 0.0 {
                self.pipeline.set_property("mute", false);
            }
        }
        slider.rect
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        clip: &mut Clip,
        editable: bool,
        export_ready: bool,
        viewport_height: f32,
        scan_ready: bool,
    ) -> PlayerResponse {
        let ctx = ui.ctx().clone();
        let editable = editable && !self.viewing_mode;
        let mut response = PlayerResponse::default();
        if let Err(error) = self.frame(&ctx) {
            self.error = Some(format!("{error:#}"));
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
        let duration = clip.media.duration_seconds;
        let mut position = self
            .pipeline
            .query_position::<gst::ClockTime>()
            .map(|p| p.seconds_f64())
            .unwrap_or(0.0)
            .clamp(0.0, duration);
        if !self.playing && self.texture.is_some() {
            position = self.frame_seconds;
        }
        if self.playing && self.stop_at.is_some_and(|end| position >= end) {
            let _ = self.pipeline.set_state(gst::State::Paused);
            self.playing = false;
            self.stop_at = None;
        }
        self.selected = self
            .selected
            .min(clip.analysis.moments.len().saturating_sub(1));
        let before = clip.analysis.moments.clone();
        self.edits.synchronize(&before);
        let mut seek_to = None;
        let mut history_action = std::mem::take(&mut self.history_action);
        let mut remove = false;
        let mut fit_selection = false;
        ui.scope(|ui| {
            let compact=ui.available_width()<600.0||viewport_height<650.0;
            let total=viewport_height.min(ui.available_height()).max(120.0);
            let reserved=if self.viewing_mode {0.0} else {self.editor_controls_height.min((total-128.0).max(80.0))+ui.spacing().item_spacing.y};
            let available=(total-reserved).max(120.0);
            let display_height=if self.fill_player || self.viewing_mode {available} else {self.video_height.clamp(120.0,available)};
            let controls_space=if self.viewing_mode {display_height-16.0} else {total-display_height-ui.spacing().item_spacing.y};
            let (plane,video_response)=ui.allocate_exact_size(egui::vec2(ui.available_width(),display_height),egui::Sense::click());
            #[cfg(test)] {response.video_plane=Some(plane);}
            ui.painter().rect_filled(plane,8.0,egui::Color32::from_rgb(7,12,18));
            if let Some(texture)=&self.texture {
                let size=texture.size_vec2();let scale=(plane.width()/size.x).min(plane.height()/size.y);let picture=egui::Rect::from_center_size(plane.center(),size*scale);
                ui.painter().image(texture.id(),picture,egui::Rect::from_min_max(egui::Pos2::ZERO,egui::pos2(1.0,1.0)),egui::Color32::WHITE);
            } else {ui.painter().text(plane.center(),egui::Align2::CENTER_CENTER,if self.error.is_some(){"Playback unavailable"}else{"Opening video..."},egui::FontId::proportional(18.0),egui::Color32::GRAY);}
            if video_response.double_clicked(){response.toggle_expand=true;}
            else if video_response.clicked(){if let Some(focus)=ctx.memory(|memory|memory.focused()){ctx.memory_mut(|memory|memory.surrender_focus(focus));}self.toggle_playback(position);}
            let pointer_over_controls = ctx.input(|input| input.pointer.hover_pos().is_some_and(|point| self.controls_rect.is_some_and(|rect| rect.contains(point))));
            let active = ctx.input(|input| input.pointer.hover_pos().is_some_and(|point| plane.contains(point)) && (input.pointer.hover_pos()!=self.overlay.last_pointer || input.pointer.delta()!=egui::Vec2::ZERO || input.pointer.any_down() || input.raw_scroll_delta!=egui::Vec2::ZERO));
            self.overlay.last_pointer=ctx.input(|input| input.pointer.hover_pos());
            let controls_visible = !self.viewing_mode || self.overlay.visible(ctx.input(|input| input.time), self.playing, active, pointer_over_controls || text_input_active(&ctx) || egui::Popup::is_any_open(&ctx) || self.error.is_some());
            #[cfg(test)] {response.controls_visible=controls_visible;}
            if !controls_visible {self.controls_rect=None; if video_response.hovered(){ctx.set_cursor_icon(egui::CursorIcon::None);}}
            if controls_visible {
            let viewing=self.viewing_mode;
            let controls_id=ui.id().with(if viewing {"player_overlay"} else {"player_controls"});
            let mut draw_controls = |ui: &mut egui::Ui| {
                let panel=egui::Frame::new().fill(if viewing {egui::Color32::from_rgba_unmultiplied(13,21,31,240)} else {egui::Color32::from_rgb(13,21,31)}).corner_radius(8.0).inner_margin(10.0).show(ui, |ui| {
                ui.set_width((plane.width()-if viewing {36.0} else {20.0}).max(100.0));
                egui::ScrollArea::vertical().id_salt("player_controls_scroll").max_height((controls_space-20.0).max(60.0)).show(ui, |ui| {
            if !self.viewing_mode {
            let(divider,resize)=ui.allocate_exact_size(egui::vec2(ui.available_width(),8.0),egui::Sense::click_and_drag());
            #[cfg(test)] {response.divider=Some(divider);}
            ui.painter().hline(egui::Rangef::new(divider.center().x-28.0,divider.center().x+28.0),divider.center().y,egui::Stroke::new(3.0_f32,if resize.hovered()||resize.dragged(){egui::Color32::LIGHT_GREEN}else{egui::Color32::from_rgb(64,82,101)}));
            resize.clone().on_hover_cursor(egui::CursorIcon::ResizeVertical).on_hover_text("Drag to resize the video. Double-click to fill the player.");
            if resize.dragged(){self.fill_player=false;self.video_height=(display_height+ctx.input(|input|input.pointer.delta().y)).clamp(120.0,available);}
            if resize.double_clicked(){self.fill_player=true;}
            }
            ui.horizontal_wrapped(|ui| {
                if ui.add(egui::Button::new(if self.playing{"Pause"}else{"Play"}).min_size(egui::vec2(58.0,30.0))).on_hover_text("Space").clicked(){self.toggle_playback(position);}
                if (!compact || self.viewing_mode) && ui.button("-5s").on_hover_text("Back five seconds / Left arrow").clicked(){seek_to=Some((position-5.0).max(0.0));}
                if !self.viewing_mode && ui.button("-1f").on_hover_text("Previous frame / comma").clicked(){self.step_frame(false);}
                if !self.viewing_mode && ui.button("+1f").on_hover_text("Next frame / period").clicked(){self.step_frame(true);}
                if (!compact || self.viewing_mode) && ui.button("+5s").on_hover_text("Forward five seconds / Right arrow").clicked(){seek_to=Some((position+5.0).min(duration));}
                ui.monospace(format!("{} / {}",edit_time(position),timestamp(duration)));
                if self.viewing_mode {
                    let back=ui.button("Back to editor").on_hover_text("Escape also restores the editor");
                    #[cfg(test)] {response.mode_button=Some(back.rect);}
                    if back.clicked(){response.toggle_expand=true;}
                } else {
                ui.menu_button("Playback",|ui| {
                    ui.horizontal(|ui|{if ui.button("Back 5 seconds").clicked(){seek_to=Some((position-5.0).max(0.0));}
if ui.button("Forward 5 seconds").clicked(){seek_to=Some((position+5.0).min(duration));}});
                    ui.checkbox(&mut self.loop_recording,"Loop recording");
                    let tracks=self.pipeline.property::<i32>("n-audio");
                    if tracks>1 {let mut track=self.pipeline.property::<i32>("current-audio");let old=track;for index in 0..tracks{ui.radio_value(&mut track,index,format!("Audio track {}",index+1));}
if track!=old{self.pipeline.set_property("current-audio",track);}}
                    ui.checkbox(&mut self.fill_player,"Fill player space");
                    let mut height=display_height;
if ui.add(egui::Slider::new(&mut height,120.0..=available).text("Video height").show_value(false)).changed(){self.fill_player=false;self.video_height=height;}
                    ui.separator();ui.small("Space  Play / pause");ui.small(", / .  Previous / next frame");ui.small("Left / Right  Skip five seconds");ui.small("Home / End  Beginning / end");ui.small("Ctrl+Z / Ctrl+Shift+Z  Undo / redo edit");
                });
                }
            });
            ui.horizontal_wrapped(|ui| {
                let volume = self.volume_controls(ui);
                #[cfg(test)] {response.volume_slider=Some(volume);}
                #[cfg(not(test))] let _=volume;
                if !self.viewing_mode {
                let viewing=ui.button("Viewing mode").on_hover_text("Watch with only timelines, playback and volume controls. Double-click the video to toggle; Escape returns to editing.");
                #[cfg(test)] {response.mode_button=Some(viewing.rect);}
                if viewing.clicked(){response.toggle_expand=true;}
                }
            });
            if !self.viewing_mode {
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui|{
                ui.menu_button(format!("Highlights ({})",clip.analysis.moments.len()),|ui|{
                    for(index,moment)in clip.analysis.moments.iter().enumerate(){if ui.selectable_label(self.selected==index,format!("{}  {}",timestamp(moment.start_seconds),moment.description)).clicked(){self.selected=index;seek_to=Some(moment.start_seconds);fit_selection=self.timeline_view.is_some();ui.close();}}
                });
                if ui.selectable_label(self.timeline_view.is_none(),"Full clip").clicked(){self.timeline_view=None;}
                if ui.add_enabled(!clip.analysis.moments.is_empty(),egui::Button::new("Fit selection").selected(self.timeline_view.is_some())).on_hover_text("Zoom in for precise trimming").clicked(){fit_selection=true;}
                if ui.add_enabled(editable&&self.edits.can_undo(),egui::Button::new("Undo")).on_hover_text("Ctrl+Z").clicked(){history_action = -1;}
                if ui.add_enabled(editable&&self.edits.can_redo(),egui::Button::new("Redo")).on_hover_text("Ctrl+Shift+Z").clicked(){history_action=1;}
            });
            }
            let old_selection=self.selected;
            let view=self.timeline_view.unwrap_or((0.0,duration));
            let timeline=timeline_view(ui,duration,&mut position,&mut clip.analysis.moments,&mut self.selected,editable,view);
            response.changed|=timeline.changed;
if let Some(seconds)=timeline.seek{seek_to=Some(seconds);}
            fit_selection|=old_selection!=self.selected&&self.timeline_view.is_some()&&!timeline.changed;
            if self.viewing_mode {
                if let Some(moment)=clip.analysis.moments.get(self.selected) {
                    ui.add(egui::Label::new(egui::RichText::new(&moment.description).color(egui::Color32::from_rgb(173,241,214))).truncate());
                }
            } else {
            if let Some(moment)=clip.analysis.moments.get_mut(self.selected){
                egui::Frame::new().fill(egui::Color32::from_rgb(24,34,46)).corner_radius(7.0).inner_margin(10.0).show(ui,|ui|{
                    ui.add_enabled_ui(editable,|ui|{
                        ui.horizontal(|ui|{
                            ui.colored_label(egui::Color32::from_rgb(120,224,186),format!("Highlight {}",self.selected+1)).on_hover_text(format!("Selection duration: {:.2} seconds",moment.end_seconds-moment.start_seconds));
                            let name_width=(ui.available_width()-80.0).max(60.0);
                            if ui.add(egui::TextEdit::singleline(&mut moment.description).id_salt("player_description").desired_width(name_width).hint_text("Name this highlight")).changed(){moment.exported_to=None;response.changed=true;}
                            response.changed|=ui.checkbox(&mut moment.kept,"Keep").changed();
                        });
                        ui.horizontal_wrapped(|ui|{
                            ui.label("Start");let start=ui.add(egui::DragValue::new(&mut moment.start_seconds).speed(self.frame_duration).range(0.0..=(moment.end_seconds-minimum_range(duration)).max(0.0)).custom_formatter(|value,_|edit_time(value)).custom_parser(parse_edit_time)).changed();
                            ui.label("End");let end=ui.add(egui::DragValue::new(&mut moment.end_seconds).speed(self.frame_duration).range((moment.start_seconds+minimum_range(duration)).min(duration)..=duration).custom_formatter(|value,_|edit_time(value)).custom_parser(parse_edit_time)).changed();
                            if start||end{moment.exported_to=None;response.changed=true;}
                            if !compact && ui.small_button("Start here").on_hover_text("Set selection start to playhead").clicked(){resize_boundary(moment,true,position,duration);response.changed=true;}
                            if !compact && ui.small_button("End here").on_hover_text("Set selection end to playhead").clicked(){resize_boundary(moment,false,position,duration);response.changed=true;}
                        });
                    });
                    if moment.description.trim().is_empty(){ui.colored_label(egui::Color32::from_rgb(240,202,128),"Add a short label before exporting.");}
                    ui.horizontal_wrapped(|ui|{
                        if ui.button("Play selection").clicked(){seek_to=Some(moment.start_seconds);self.stop_at=Some(moment.end_seconds);
if self.pipeline.set_state(gst::State::Playing).is_ok(){self.playing=true;}}
                        else if seek_to.is_some(){self.stop_at=None;}
                        let export=ui.add_enabled(export_ready&&!moment.description.trim().is_empty()&&crate::segments::validate_range(moment.start_seconds,moment.end_seconds,duration).is_ok(),egui::Button::new(egui::RichText::new("Export clip...").color(egui::Color32::from_rgb(12,37,28))).fill(egui::Color32::from_rgb(120,224,186))).on_disabled_hover_text("Waiting for source verification or the current background task to finish");
                        #[cfg(test)] {response.export_button=Some(export.rect);}
                        if export.clicked(){response.export=Some(self.selected);}
                        ui.menu_button("More",|ui|{if ui.add_enabled(editable,egui::Button::new("Set start at playhead")).clicked(){resize_boundary(moment,true,position,duration);response.changed=true;ui.close();}
if ui.add_enabled(editable,egui::Button::new("Set end at playhead")).clicked(){resize_boundary(moment,false,position,duration);response.changed=true;ui.close();}
if ui.add_enabled(editable,egui::Button::new("Remove highlight")).clicked(){remove=true;ui.close();}
if ui.button("Jump to start").clicked(){seek_to=Some(moment.start_seconds);ui.close();}
if let Some(path)=&moment.exported_to && ui.button("Copy exported file path").clicked(){ctx.copy_text(path.display().to_string());ui.close();}});
                        if moment.exported_to.is_some(){ui.colored_label(egui::Color32::LIGHT_GREEN,"Exported");}
                    });
                });
            }else{ui.small("No highlights yet. Add a marker at the playhead, or run a scan.");}
            ui.horizontal_wrapped(|ui|{
                if ui.add_enabled(editable,egui::Button::new("+ Add highlight")).on_hover_text("Create a five-second selection at the playhead").clicked(){let start=position.min((duration-minimum_range(duration)).max(0.0));clip.analysis.moments.push(Moment{start_seconds:start,end_seconds:(start+5.0).min(duration),description:"New highlight".into(),kept:false,exported_to:None});self.selected=clip.analysis.moments.len()-1;response.changed=true;}
                let rescan=ui.add_enabled(scan_ready,egui::Button::new("Rescan highlights")).on_hover_text("Analyze this recording again with this folder's scan settings");
                #[cfg(test)] {response.rescan_button=Some(rescan.rect);}
                if rescan.clicked(){response.rescan=true;}
                if !clip.analysis.moments.is_empty()&&!compact{ui.small("Drag blocks to move; drag edges to trim.");}
            });
            }
            if let Some(error)=&self.error{ui.colored_label(egui::Color32::LIGHT_RED,"Playback needs attention");egui::CollapsingHeader::new("Playback details").show(ui,|ui|{ui.add(egui::Label::new(error).wrap());});
if ui.button("Retry video").clicked(){response.reload=true;}}
                })
                });
                (panel.response.rect, panel.inner.content_size.y+20.0)
            };
            let (rect,natural_height)=if viewing {
                let overlay=egui::Area::new(controls_id)
                    .order(egui::Order::Foreground)
                    .pivot(egui::Align2::LEFT_BOTTOM)
                    .fixed_pos(egui::pos2(plane.left()+8.0,plane.bottom()-8.0))
                    .constrain_to(plane)
                    .show(&ctx,&mut draw_controls);
                (overlay.response.rect, overlay.inner.1)
            } else {
                ui.push_id(controls_id, &mut draw_controls).inner
            };
            self.controls_rect=Some(rect);
            if !viewing {self.editor_controls_height=natural_height;}
            #[cfg(test)] {response.controls_bounds=Some(rect);}
            }
        });
        if remove {
            clip.analysis.moments.remove(self.selected);
            self.selected = self.selected.saturating_sub(1);
            response.changed = true;
        }
        if fit_selection && let Some(moment) = clip.analysis.moments.get(self.selected) {
            let padding = ((moment.end_seconds - moment.start_seconds) * 0.3).max(2.0);
            self.timeline_view = Some((
                (moment.start_seconds - padding).max(0.0),
                (moment.end_seconds + padding).min(duration),
            ));
        }
        if editable && history_action != 0 {
            response.changed |= self.edits.apply(&mut clip.analysis.moments, history_action);
        } else if response.changed {
            self.edits.record(
                before,
                &clip.analysis.moments,
                ctx.input(|input| input.pointer.any_down()) || text_input_active(&ctx),
            );
        } else if !ctx.input(|input| input.pointer.any_down()) && !text_input_active(&ctx) {
            self.edits.transaction = false;
        }
        self.edits.observed = Some(clip.analysis.moments.clone());

        if response.changed {
            self.stop_at = None;
        }
        if let Some(seconds) = seek_to
            && let Err(error) = self.seek(seconds)
        {
            self.error = Some(error.to_string());
        }
        response
    }
}

#[derive(Default)]
struct OverlayState {
    last_pointer: Option<egui::Pos2>,
    last_activity: Option<f64>,
}
impl OverlayState {
    fn visible(&mut self, now: f64, playing: bool, activity: bool, interacting: bool) -> bool {
        if self.last_activity.is_none() || activity || interacting || !playing {
            self.last_activity = Some(now);
        }
        !playing || interacting || now - self.last_activity.unwrap_or(now) < 2.5
    }
}

#[derive(Default)]
struct EditHistory {
    undo: Vec<Vec<Moment>>,
    redo: Vec<Vec<Moment>>,
    observed: Option<Vec<Moment>>,
    transaction: bool,
}
impl EditHistory {
    fn synchronize(&mut self, current: &[Moment]) {
        if self.observed.as_ref().is_some_and(|last| last != current) {
            self.undo.clear();
            self.redo.clear();
            self.transaction = false;
        }
    }
    fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    fn record(&mut self, before: Vec<Moment>, after: &[Moment], continuing: bool) {
        if before == after {
            return;
        }
        if !self.transaction {
            self.undo.push(before);
            if self.undo.len() > 50 {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.transaction = continuing;
        self.observed = Some(after.to_vec());
    }
    fn apply(&mut self, current: &mut Vec<Moment>, direction: i8) -> bool {
        let (from, to) = if direction < 0 {
            (&mut self.undo, &mut self.redo)
        } else {
            (&mut self.redo, &mut self.undo)
        };
        if let Some(snapshot) = from.pop() {
            to.push(std::mem::replace(current, snapshot));
            self.transaction = false;
            self.observed = Some(current.clone());
            true
        } else {
            false
        }
    }
}
fn edit_time(seconds: f64) -> String {
    let full = crate::media::clip_timecode(seconds);
    if seconds < 3600.0 {
        full[3..].into()
    } else {
        full
    }
}
fn parse_edit_time(value: &str) -> Option<f64> {
    let parts: Vec<_> = value.trim().split(':').collect();
    let result = match parts.as_slice() {
        [seconds] => seconds.parse::<f64>().ok()?,
        [minutes, seconds] => {
            let seconds = seconds.parse::<f64>().ok()?;
            if !(0.0..60.0).contains(&seconds) {
                return None;
            }
            minutes.parse::<u32>().ok()? as f64 * 60.0 + seconds
        }
        [hours, minutes, seconds] => {
            let minutes = minutes.parse::<u32>().ok()?;
            let seconds = seconds.parse::<f64>().ok()?;
            if minutes >= 60 || !(0.0..60.0).contains(&seconds) {
                return None;
            }
            hours.parse::<u32>().ok()? as f64 * 3600.0 + minutes as f64 * 60.0 + seconds
        }
        _ => return None,
    };
    (result.is_finite() && result >= 0.0).then_some(result)
}

fn text_input_active(ctx: &egui::Context) -> bool {
    ctx.memory(|memory| memory.focused())
        .is_some_and(|id| egui::TextEdit::load_state(ctx, id).is_some())
}

#[derive(Default)]
pub struct PlayerResponse {
    pub changed: bool,
    pub export: Option<usize>,
    pub rescan: bool,
    pub reload: bool,
    pub toggle_expand: bool,
    #[cfg(test)]
    controls_visible: bool,
    #[cfg(test)]
    controls_bounds: Option<egui::Rect>,
    #[cfg(test)]
    volume_slider: Option<egui::Rect>,
    #[cfg(test)]
    mode_button: Option<egui::Rect>,
    #[cfg(test)]
    export_button: Option<egui::Rect>,
    #[cfg(test)]
    rescan_button: Option<egui::Rect>,
    #[cfg(test)]
    video_plane: Option<egui::Rect>,
    #[cfg(test)]
    divider: Option<egui::Rect>,
}

#[derive(Default)]
struct TimelineResponse {
    changed: bool,
    seek: Option<f64>,
    #[cfg(test)]
    geometry: Vec<egui::Rect>,
}

fn minimum_range(duration: f64) -> f64 {
    0.05_f64.min(duration)
}

fn resize_boundary(moment: &mut Moment, start: bool, seconds: f64, duration: f64) {
    let minimum = minimum_range(duration);
    if start {
        moment.start_seconds = seconds.clamp(0.0, (moment.end_seconds - minimum).max(0.0));
    } else {
        moment.end_seconds =
            seconds.clamp((moment.start_seconds + minimum).min(duration), duration);
    }
    moment.exported_to = None;
}

fn time_x(rect: egui::Rect, seconds: f64, duration: f64) -> f32 {
    egui::lerp(rect.x_range(), (seconds / duration).clamp(0.0, 1.0) as f32)
}
fn x_time(rect: egui::Rect, x: f32, duration: f64) -> f64 {
    f64::from(((x - rect.left()) / rect.width()).clamp(0.0, 1.0)) * duration
}

#[cfg(test)]
fn timeline(
    ui: &mut egui::Ui,
    duration: f64,
    position: &mut f64,
    moments: &mut [Moment],
    selected: &mut usize,
    editable: bool,
) -> TimelineResponse {
    timeline_view(
        ui,
        duration,
        position,
        moments,
        selected,
        editable,
        (0.0, duration),
    )
}

fn timeline_view(
    ui: &mut egui::Ui,
    duration: f64,
    position: &mut f64,
    moments: &mut [Moment],
    selected: &mut usize,
    editable: bool,
    view: (f64, f64),
) -> TimelineResponse {
    let mut result = TimelineResponse::default();
    let (view_start, view_end) = view;
    let span = (view_end - view_start).max(0.001);
    let width = ui.available_width().max(100.0);
    let label_width = (width * 0.25).clamp(90.0, 200.0);
    let (row, _) = ui.allocate_exact_size(egui::vec2(width, 27.0), egui::Sense::hover());
    let rect = egui::Rect::from_min_max(egui::pos2(row.left() + label_width, row.top()), row.max);
    ui.painter().text(
        row.left_center(),
        egui::Align2::LEFT_CENTER,
        if view_start > 0.0 || view_end < duration {
            "Zoomed selection"
        } else {
            "Recording time"
        },
        egui::FontId::proportional(11.0),
        egui::Color32::from_rgb(162, 177, 195),
    );
    ui.painter()
        .rect_filled(rect, 4.0, egui::Color32::from_rgb(11, 17, 25));
    let map_time = |seconds: f64| time_x(rect, seconds - view_start, span);
    let ticks = ((rect.width() / 90.0) as usize).clamp(2, 8);
    for i in 0..=ticks {
        let fraction = i as f32 / ticks as f32;
        let at = view_start + span * fraction as f64;
        let text = crate::media::clip_timecode(at);
        let label = if at < 3600.0 { &text[3..8] } else { &text[..8] };
        ui.painter().text(
            egui::pos2(egui::lerp(rect.x_range(), fraction), rect.center().y),
            if i == 0 {
                egui::Align2::LEFT_CENTER
            } else if i == ticks {
                egui::Align2::RIGHT_CENTER
            } else {
                egui::Align2::CENTER_CENTER
            },
            label,
            egui::FontId::monospace(11.0),
            egui::Color32::from_rgb(192, 206, 222),
        );
    }
    let track = ui.interact(
        rect,
        ui.id().with("seek_track"),
        egui::Sense::click_and_drag(),
    );
    if (track.clicked() || track.dragged())
        && let Some(pointer) = track.interact_pointer_pos()
    {
        *position = view_start + x_time(rect, pointer.x, span);
        if track.clicked()
            && let Some(index) = moments.iter().position(|moment| {
                moment.start_seconds <= *position && moment.end_seconds >= *position
            })
        {
            *selected = index;
        }
        result.seek = Some(*position);
    }
    #[cfg(test)]
    result.geometry.push(rect);
    if (view_start..=view_end).contains(position) {
        ui.painter().vline(
            map_time(*position),
            rect.y_range(),
            egui::Stroke::new(2.0_f32, egui::Color32::WHITE),
        );
    }
    let (row, _) = ui.allocate_exact_size(egui::vec2(width, 38.0), egui::Sense::hover());
    let lane = egui::Rect::from_min_max(
        egui::pos2(row.left() + label_width, row.top() + 2.0),
        egui::pos2(row.right(), row.bottom() - 2.0),
    );
    #[cfg(test)]
    result.geometry.push(lane);
    ui.painter().text(
        row.left_center(),
        egui::Align2::LEFT_CENTER,
        "Highlights",
        egui::FontId::proportional(11.0),
        egui::Color32::from_rgb(162, 177, 195),
    );
    ui.painter()
        .rect_filled(lane, 4.0, egui::Color32::from_rgb(11, 17, 25));
    let range_rect = |moment: &Moment| {
        egui::Rect::from_min_max(
            egui::pos2(
                time_x(lane, moment.start_seconds - view_start, span),
                lane.top(),
            ),
            egui::pos2(
                time_x(lane, moment.end_seconds - view_start, span),
                lane.bottom(),
            ),
        )
    };
    let visible =
        |moment: &Moment| moment.end_seconds >= view_start && moment.start_seconds <= view_end;
    // Paint the selected range last so its trim handles stay visible over overlapping highlights.
    for index in (0..moments.len())
        .filter(|index| *index != *selected)
        .chain(std::iter::once(*selected))
    {
        let Some(moment) = moments.get(index).filter(|moment| visible(moment)) else {
            continue;
        };
        let bar = range_rect(moment);
        let chosen = index == *selected;
        let painter = ui.painter().with_clip_rect(lane);
        painter.rect_filled(
            bar,
            3.0,
            if chosen {
                egui::Color32::from_rgb(62, 150, 112)
            } else {
                egui::Color32::from_rgb(53, 108, 91)
            },
        );
        if chosen {
            for x in [bar.left(), bar.right()] {
                painter.vline(
                    x,
                    lane.y_range(),
                    egui::Stroke::new(3.0_f32, egui::Color32::from_rgb(155, 240, 201)),
                );
            }
        }
    }
    // One interaction for the whole track avoids hidden blocks stealing drag gestures.
    let track_id = ui.id().with("highlight_track");
    let track = ui.interact(lane, track_id, egui::Sense::click_and_drag());
    let hit = |pointer: egui::Pos2| -> Option<(usize, u8)> {
        if let Some(moment) = moments.get(*selected).filter(|moment| visible(moment)) {
            let bar = range_rect(moment);
            let inset = (bar.width() * 0.25).min(6.0);
            if moment.start_seconds >= view_start
                && (bar.left() - 6.0..=bar.left() + inset).contains(&pointer.x)
            {
                return Some((*selected, 1));
            }
            if moment.end_seconds <= view_end
                && (bar.right() - inset..=bar.right() + 6.0).contains(&pointer.x)
            {
                return Some((*selected, 2));
            }
            if bar.contains(pointer) {
                return Some((*selected, 0));
            }
        }
        moments
            .iter()
            .enumerate()
            .rev()
            .find(|(_, moment)| visible(moment) && range_rect(moment).contains(pointer))
            .map(|(index, _)| (index, 0))
    };
    if let Some(pointer) = track.hover_pos() {
        if let Some((index, edge)) = hit(pointer) {
            ui.ctx().set_cursor_icon(if editable && edge != 0 {
                egui::CursorIcon::ResizeHorizontal
            } else if editable {
                egui::CursorIcon::Grab
            } else {
                egui::CursorIcon::PointingHand
            });
            let moment = &moments[index];
            track.clone().on_hover_text(format!("{}\n{} to {}\nClick to select. Drag to move; drag selected edges to trim. Click overlapping blocks again to cycle.", moment.description, edit_time(moment.start_seconds), edit_time(moment.end_seconds)));
        } else {
            track
                .clone()
                .on_hover_cursor(egui::CursorIcon::PointingHand);
        }
    }
    let clicked = track
        .clicked()
        .then(|| track.interact_pointer_pos())
        .flatten()
        .map(|pointer| {
            // Repeated clicks cycle coincident ranges without creating extra timeline rows.
            let matches: Vec<_> = moments
                .iter()
                .enumerate()
                .filter(|(_, moment)| visible(moment) && range_rect(moment).contains(pointer))
                .map(|(index, _)| index)
                .collect();
            let index = if matches.len() > 1 {
                let next = matches
                    .iter()
                    .position(|index| index == selected)
                    .map_or(0, |index| (index + 1) % matches.len());
                Some(matches[next])
            } else {
                hit(pointer).map(|(index, _)| index)
            };
            (pointer, index)
        });
    let drag_start = if editable && track.drag_started() {
        ui.input(|input| input.pointer.press_origin()).and_then(hit)
    } else {
        None
    };
    if let Some((_, Some(index))) = clicked {
        *selected = index;
        result.seek = Some(moments[index].start_seconds);
    } else if let Some((pointer, None)) = clicked {
        *position = view_start + x_time(lane, pointer.x, span);
        result.seek = Some(*position);
    }
    if let Some((index, edge)) = drag_start {
        *selected = index;
        if let Some(origin) = ui.input(|input| input.pointer.press_origin()) {
            let moment = &moments[index];
            ui.data_mut(|data| {
                data.insert_temp(
                    track_id,
                    (
                        index,
                        edge,
                        moment.start_seconds,
                        moment.end_seconds,
                        origin.x,
                        span / f64::from(lane.width()),
                    ),
                )
            });
        }
    }
    if editable
        && track.dragged()
        && let Some(pointer) = track.interact_pointer_pos()
        && let Some((index, edge, start, end, origin, seconds_per_pixel)) =
            ui.data(|data| data.get_temp::<(usize, u8, f64, f64, f32, f64)>(track_id))
        && let Some(moment) = moments.get_mut(index)
    {
        *selected = index;
        if edge == 0 {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            let length = end - start;
            let new_start = (start + f64::from(pointer.x - origin) * seconds_per_pixel)
                .clamp(0.0, (duration - length).max(0.0));
            if moment.start_seconds != new_start
                || moment.end_seconds != (new_start + length).min(duration)
            {
                moment.start_seconds = new_start;
                moment.end_seconds = (new_start + length).min(duration);
                moment.exported_to = None;
                result.changed = true;
            }
        } else {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            resize_boundary(
                moment,
                edge == 1,
                view_start + x_time(lane, pointer.x, span),
                duration,
            );
            result.changed = true;
        }
    }
    if track.drag_stopped() {
        ui.data_mut(|data| {
            data.remove::<(usize, u8, f64, f64, f32, f64)>(track_id);
        });
    }
    if (view_start..=view_end).contains(position) {
        ui.painter().vline(
            time_x(lane, *position - view_start, span),
            lane.y_range(),
            egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
        );
    }

    result
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::Moment;

    fn example_moment(start: f64, end: f64) -> Moment {
        Moment {
            start_seconds: start,
            end_seconds: end,
            description: "Test highlight".into(),
            kept: true,
            exported_to: Some("previous.mp4".into()),
        }
    }

    #[test]
    fn overlay_hides_when_idle_and_stays_visible_during_interaction() {
        let mut overlay = OverlayState::default();
        assert!(overlay.visible(0.0, true, false, false));
        assert!(overlay.visible(2.0, true, false, false));
        assert!(!overlay.visible(3.0, true, false, false));
        assert!(
            overlay.visible(4.0, true, true, false),
            "Mouse movement reveals controls"
        );
        assert!(!overlay.visible(7.0, true, false, false));
        assert!(
            overlay.visible(8.0, false, false, false),
            "Pause reveals controls"
        );
        assert!(overlay.visible(30.0, false, false, false));
        assert!(
            overlay.visible(40.0, true, false, true),
            "Hover, editing, drags, menus and pinning prevent hiding"
        );
        assert!(overlay.visible(42.0, true, false, false));
        assert!(!overlay.visible(43.0, true, false, false));
    }

    #[test]
    fn highlights_share_one_track_and_overlapping_blocks_cycle_selection() {
        let ctx = egui::Context::default();
        let mut moments = vec![
            example_moment(20.0, 60.0),
            example_moment(30.0, 50.0),
            example_moment(35.0, 55.0),
        ];
        let mut selected = 0;
        let mut position = 0.0;
        let mut frame = |events: Vec<egui::Event>| {
            let mut output = TimelineResponse::default();
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 500.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        output =
                            timeline(ui, 100.0, &mut position, &mut moments, &mut selected, true);
                    });
                },
            );
            (output, selected)
        };
        let (output, _) = frame(vec![]);
        assert_eq!(
            output.geometry.len(),
            2,
            "Only the recording ruler and one shared highlight track should exist"
        );
        let lane = output.geometry[1];
        let point = egui::pos2(time_x(lane, 42.0, 100.0), lane.center().y);
        for (index, start) in [(1, 30.0), (2, 35.0), (0, 20.0)] {
            for pressed in [true, false] {
                let (output, selected) = frame(vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ]);
                assert_eq!(output.geometry, vec![output.geometry[0], lane]);
                if !pressed {
                    assert_eq!(selected, index);
                    assert_eq!(output.seek, Some(start));
                }
            }
        }
        drop(frame);
        moments.extend((0..50).map(|index| example_moment(index as f64, index as f64 + 1.0)));
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let output = timeline(ui, 100.0, &mut position, &mut moments, &mut selected, true);
                assert_eq!(
                    output.geometry.len(),
                    2,
                    "Adding highlights must not add rows or scrolling"
                );
                assert_eq!(output.geometry[1].height(), lane.height());
            });
        });
    }

    #[test]
    fn timeline_scrubs_selects_overlapping_highlights_and_drags_edges() {
        let ctx = egui::Context::default();
        let mut moments = vec![example_moment(20.0, 40.0), example_moment(30.0, 60.0)];
        let mut position = 0.0;
        let mut selected = 0;
        let mut frame = |events: Vec<egui::Event>, editable: bool| {
            let mut output = TimelineResponse::default();
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 500.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        output = timeline(
                            ui,
                            100.0,
                            &mut position,
                            &mut moments,
                            &mut selected,
                            editable,
                        );
                    });
                },
            );
            output
        };
        fn pointer(pos: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        }
        let geometry = frame(vec![], true).geometry;
        let track = geometry[0];
        let seek = egui::pos2(time_x(track, 75.0, 100.0), track.center().y);
        frame(pointer(seek, true), true);
        let output = frame(pointer(seek, false), true);
        assert!((output.seek.unwrap() - 75.0).abs() < 0.01);
        let lane = geometry[1];
        let body = egui::pos2(time_x(lane, 50.0, 100.0), lane.center().y);
        frame(pointer(body, true), true);
        let output = frame(pointer(body, false), true);
        assert_eq!(output.seek, Some(30.0));
        let start = egui::pos2(time_x(lane, 30.0, 100.0), lane.center().y);
        frame(pointer(start, true), true);
        let moved = egui::pos2(time_x(lane, 25.0, 100.0), lane.center().y);
        assert!(frame(vec![egui::Event::PointerMoved(moved)], true).changed);
        frame(pointer(moved, false), true);
        let end = egui::pos2(time_x(lane, 60.0, 100.0), lane.center().y);
        frame(pointer(end, true), true);
        let moved = egui::pos2(time_x(lane, 80.0, 100.0), lane.center().y);
        assert!(frame(vec![egui::Event::PointerMoved(moved)], true).changed);
        frame(pointer(moved, false), true);
        // A background export must prevent further edits to its range.
        frame(pointer(moved, true), false);
        assert!(!frame(vec![egui::Event::PointerMoved(end)], false).changed);
        frame(pointer(end, false), false);
        assert_eq!(selected, 1);
        assert!((moments[1].start_seconds - 25.0).abs() < 0.01);
        assert!((moments[1].end_seconds - 80.0).abs() < 0.01);
        assert!(moments[1].exported_to.is_none());
        assert_eq!(moments[0].start_seconds, 20.0);
    }

    #[test]
    fn dragging_highlight_blocks_moves_both_bounds_and_preserves_duration() {
        struct Harness {
            ctx: egui::Context,
            moments: Vec<Moment>,
            selected: usize,
            position: f64,
            view: (f64, f64),
        }
        impl Harness {
            fn frame(&mut self, events: Vec<egui::Event>, editable: bool) -> TimelineResponse {
                let mut result = TimelineResponse::default();
                let _ = self.ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(800.0, 500.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            result = timeline_view(
                                ui,
                                100.0,
                                &mut self.position,
                                &mut self.moments,
                                &mut self.selected,
                                editable,
                                self.view,
                            );
                        });
                    },
                );
                result
            }
        }
        fn button(pos: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        }
        for view in [(0.0, 100.0), (10.0, 60.0)] {
            let mut h = Harness {
                ctx: egui::Context::default(),
                moments: vec![example_moment(2.0, 8.0), example_moment(20.0, 30.0)],
                selected: 0,
                position: 0.0,
                view,
            };
            let lane = h.frame(vec![], true).geometry[1];
            let at = |seconds: f64| {
                egui::pos2(
                    lane.left() + ((seconds - view.0) / (view.1 - view.0)) as f32 * lane.width(),
                    lane.center().y,
                )
            };
            // Grab away from the midpoint to verify the original grab offset is retained.
            h.frame(button(at(24.0), true), true);
            assert!(
                h.frame(vec![egui::Event::PointerMoved(at(29.0))], true)
                    .changed
            );
            assert!((h.moments[1].start_seconds - 25.0).abs() < 0.001);
            assert!((h.moments[1].end_seconds - 35.0).abs() < 0.001);
            h.frame(vec![egui::Event::PointerMoved(at(34.0))], true);
            assert!(
                (h.moments[1].start_seconds - 30.0).abs() < 0.001,
                "Drag updates must not accumulate the previous delta"
            );
            assert_eq!(h.selected, 1);
            assert!(h.moments[1].kept && h.moments[1].exported_to.is_none());
            h.frame(vec![egui::Event::PointerMoved(at(-50.0))], true);
            assert_eq!(h.moments[1].start_seconds, 0.0);
            assert_eq!(h.moments[1].end_seconds, 10.0);
            h.frame(vec![egui::Event::PointerMoved(at(200.0))], true);
            assert_eq!(h.moments[1].start_seconds, 90.0);
            assert_eq!(h.moments[1].end_seconds, 100.0);
            h.frame(vec![egui::Event::PointerMoved(at(29.0))], true);
            assert!(
                (h.moments[1].start_seconds - 25.0).abs() < 0.001,
                "Reversing at a boundary must follow the original drag anchor: view {view:?}, got {:?}",
                (h.moments[1].start_seconds, h.moments[1].end_seconds)
            );
            h.frame(button(at(29.0), false), true);
            assert!((h.moments[1].end_seconds - h.moments[1].start_seconds - 10.0).abs() < 0.001);
            assert_eq!(
                (h.moments[0].start_seconds, h.moments[0].end_seconds),
                (2.0, 8.0)
            );
            let before = h.moments.clone();
            h.frame(button(at(29.0), true), false);
            assert!(
                !h.frame(vec![egui::Event::PointerMoved(at(35.0))], false)
                    .changed
            );
            h.frame(button(at(35.0), false), false);
            assert_eq!(h.moments, before, "Export locks must prevent moving ranges");
        }
        let mut h = Harness {
            ctx: egui::Context::default(),
            moments: vec![example_moment(40.0, 41.0)],
            selected: 0,
            position: 0.0,
            view: (0.0, 100.0),
        };
        let lane = h.frame(vec![], true).geometry[1];
        let at = |seconds: f64| egui::pos2(time_x(lane, seconds, 100.0), lane.center().y);
        h.frame(button(at(40.5), true), true);
        assert!(
            h.frame(vec![egui::Event::PointerMoved(at(45.5))], true)
                .changed
        );
        h.frame(button(at(45.5), false), true);
        assert!((h.moments[0].start_seconds - 45.0).abs() < 0.001);
        assert!(
            (h.moments[0].end_seconds - 46.0).abs() < 0.001,
            "Short blocks still have a draggable center"
        );
    }

    #[test]
    fn edit_history_coalesces_drags_undoes_removal_and_resets_for_new_scan_results() {
        let original = vec![example_moment(20.0, 40.0)];
        let mut current = original.clone();
        let mut edits = EditHistory::default();
        current[0].start_seconds = 21.0;
        edits.record(original.clone(), &current, true);
        let before = current.clone();
        current[0].start_seconds = 22.0;
        edits.record(before, &current, false);
        assert_eq!(edits.undo.len(), 1, "One drag should be one undo step");
        assert!(edits.apply(&mut current, -1));
        assert_eq!(current, original);
        assert!(edits.apply(&mut current, 1));
        assert_eq!(current[0].start_seconds, 22.0);
        let before = current.clone();
        current.clear();
        edits.record(before.clone(), &current, false);
        assert!(edits.apply(&mut current, -1));
        assert_eq!(current, before, "Removing a marker is reversible");
        current.push(example_moment(50.0, 60.0));
        edits.synchronize(&current);
        assert!(
            !edits.can_undo() && !edits.can_redo(),
            "Undo must not remove newly arrived AI highlights"
        );
    }

    #[test]
    fn trim_time_inputs_accept_precise_timecodes_and_seconds() {
        assert_eq!(parse_edit_time("01:42.125"), Some(102.125));
        assert_eq!(parse_edit_time("42.5"), Some(42.5));
        assert_eq!(parse_edit_time("01:02:03.500"), Some(3723.5));
        assert_eq!(edit_time(59.9996), "01:00.000");
        for invalid in ["-1", "NaN", "00:60", "01:70:02", "nope"] {
            assert_eq!(parse_edit_time(invalid), None);
        }
    }

    #[test]
    fn zoomed_timeline_uses_absolute_recording_times() {
        let ctx = egui::Context::default();
        let mut moments = vec![example_moment(40.0, 50.0)];
        let mut selected = 0;
        let mut position = 0.0;
        let mut frame = |events| {
            let mut result = TimelineResponse::default();
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 400.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        result = timeline_view(
                            ui,
                            180.0,
                            &mut position,
                            &mut moments,
                            &mut selected,
                            true,
                            (35.0, 55.0),
                        );
                    });
                },
            );
            result
        };
        let rect = frame(vec![]).geometry[0];
        let pos = rect.center();
        let pointer = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![egui::Event::PointerMoved(pos), pointer(true)]);
        assert!((frame(vec![pointer(false)]).seek.unwrap() - 45.0).abs() < 0.01);
    }

    #[test]
    fn trimming_cannot_cross_the_other_edge_or_leave_the_recording() {
        let mut moment = example_moment(20.0, 40.0);
        resize_boundary(&mut moment, true, 100.0, 100.0);
        assert!(moment.start_seconds < moment.end_seconds);
        assert!((moment.start_seconds - 39.95).abs() < 0.001);
        resize_boundary(&mut moment, true, -10.0, 100.0);
        assert_eq!(moment.start_seconds, 0.0);
        resize_boundary(&mut moment, false, 120.0, 100.0);
        assert_eq!(moment.end_seconds, 100.0);
        resize_boundary(&mut moment, false, -1.0, 100.0);
        assert_eq!(moment.end_seconds, 0.05);
        assert!(moment.exported_to.is_none());
    }

    #[test]
    fn direct_source_decoder_displays_frames_seeks_and_preserves_the_original() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preview.mp4");
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-nostdin",
                "-n",
                "-f",
                "lavfi",
                "-i",
                "color=c=blue:s=1920x1080:r=24:d=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-c:v",
                "libx264",
                "-c:a",
                "aac",
                "-t",
                "1",
            ])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        gst::init().unwrap();
        let silent_test_sink = gst::ElementFactory::make("fakesink")
            .property("sync", true)
            .build()
            .unwrap();
        let mut player =
            Player::open_with_audio_sink(path.clone(), 1.0, Some(&silent_test_sink)).unwrap();
        let (result, _, _) = player.pipeline.state(gst::ClockTime::from_seconds(5));
        result.unwrap();
        let ctx = egui::Context::default();
        player.frame(&ctx).unwrap();
        assert!(player.error.is_none(), "{:?}", player.error);
        assert!(
            player.texture.is_some(),
            "Embedded player must deliver a decoded video frame"
        );
        let preview_size = player.texture.as_ref().unwrap().size();
        assert!(
            preview_size[0] <= 1280 && preview_size[1] <= 720,
            "UI frames should be downscaled, got {preview_size:?}"
        );
        player.pipeline.set_state(gst::State::Paused).unwrap();
        player
            .pipeline
            .state(gst::ClockTime::from_seconds(5))
            .0
            .unwrap();
        player.playing = false;
        player.seek(0.25).unwrap();
        player
            .pipeline
            .state(gst::ClockTime::from_seconds(5))
            .0
            .unwrap();
        player.frame(&ctx).unwrap();
        let initial = player.frame_seconds;
        assert!(
            (initial - 0.25).abs() < 0.001,
            "Actual seek frame: {initial}"
        );
        fn key(key: egui::Key) -> egui::Event {
            egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }
        }
        let _ = ctx.run(
            egui::RawInput {
                events: vec![key(egui::Key::Period)],
                ..Default::default()
            },
            |ctx| player.handle_shortcuts(ctx),
        );
        assert!(
            player
                .pipeline
                .bus()
                .unwrap()
                .timed_pop_filtered(
                    gst::ClockTime::from_seconds(2),
                    &[gst::MessageType::StepDone]
                )
                .is_some(),
            "Forward step must finish"
        );
        player.frame(&ctx).unwrap();
        assert!(!player.playing);
        assert!(
            (player.frame_seconds - initial - 1.0 / 24.0).abs() < 0.001,
            "Next frame: {} from {initial}",
            player.frame_seconds
        );
        let _ = ctx.run(
            egui::RawInput {
                events: vec![key(egui::Key::Comma)],
                ..Default::default()
            },
            |ctx| player.handle_shortcuts(ctx),
        );
        player
            .pipeline
            .state(gst::ClockTime::from_seconds(5))
            .0
            .unwrap();
        player.frame(&ctx).unwrap();
        assert!(
            (player.frame_seconds - initial).abs() < 0.001,
            "Previous frame: {}",
            player.frame_seconds
        );

        // Text entry takes precedence over playback, including spaces and punctuation.
        let id = egui::Id::new("typing_test");
        let mut text = String::new();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.add(egui::TextEdit::singleline(&mut text).id(id))
                    .request_focus();
            });
        });
        for (pressed, typed) in [
            (egui::Key::Space, " "),
            (egui::Key::Period, "."),
            (egui::Key::Comma, ","),
        ] {
            let _ = ctx.run(
                egui::RawInput {
                    events: vec![key(pressed), egui::Event::Text(typed.into())],
                    ..Default::default()
                },
                |ctx| {
                    player.handle_shortcuts(ctx);
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.add(egui::TextEdit::singleline(&mut text).id(id));
                    });
                },
            );
            assert!(!player.playing);
        }
        assert_eq!(text, " .,");
        ctx.memory_mut(|memory| memory.surrender_focus(id));
        let _ = ctx.run(
            egui::RawInput {
                events: vec![key(egui::Key::Space)],
                ..Default::default()
            },
            |ctx| player.handle_shortcuts(ctx),
        );
        assert!(player.playing, "Space resumes playback");
        let _ = ctx.run(
            egui::RawInput {
                events: vec![key(egui::Key::Space)],
                ..Default::default()
            },
            |ctx| player.handle_shortcuts(ctx),
        );
        assert!(!player.playing, "Space pauses playback");

        let mut clip = Clip {
            source: path.clone(),
            sha256: crate::media::fingerprint(&path).unwrap(),
            provider: crate::provider::Provider::default(),
            model: "manual".into(),
            audio_model: None,
            proposed_name: String::new(),
            media: crate::media::probe(&path).unwrap(),
            analysis: crate::ai::Analysis::default(),
        };
        let mut render = |events| {
            let mut response = PlayerResponse::default();
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 1000.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        response = player.show(ui, &mut clip, true, true, 1000.0, true);
                    });
                },
            );
            response
        };
        render(vec![]);
        let before = render(vec![]);
        assert!(
            before.video_plane.unwrap().bottom() <= before.controls_bounds.unwrap().top(),
            "Normal controls must sit below the video"
        );
        assert!(before.controls_visible);
        let grip = before.divider.unwrap().center();
        let moved = grip + egui::vec2(0.0, -80.0);
        let pointer = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        render(vec![egui::Event::PointerMoved(grip), pointer(grip, true)]);
        render(vec![egui::Event::PointerMoved(moved)]);
        let after = render(vec![pointer(moved, false)]);
        assert!(
            after.video_plane.unwrap().height() < before.video_plane.unwrap().height() - 60.0,
            "Divider must resize the video independently of the app window"
        );
        let button = after.rescan_button.unwrap().center();
        render(vec![
            egui::Event::PointerMoved(button),
            pointer(button, true),
        ]);
        assert!(
            render(vec![pointer(button, false)]).rescan,
            "Rescan belongs to the embedded player"
        );
        clip.analysis.moments = vec![
            example_moment(0.1, 0.4),
            example_moment(0.4, 0.7),
            example_moment(0.7, 0.9),
        ];
        let mut small = PlayerResponse::default();
        for _ in 0..3 {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(470.0, 550.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        small = player.show(ui, &mut clip, true, true, 530.0, true);
                    });
                },
            );
        }
        assert!(
            small.export_button.is_some() && small.rescan_button.is_some(),
            "Compact controls remain available below the video"
        );
        let controls = player.controls_rect.unwrap();
        let plane = small.video_plane.unwrap();
        assert!(
            plane.bottom() <= controls.top() && controls.bottom() <= 550.0,
            "Normal controls stay below the video and inside the window: plane {plane:?}, controls {controls:?}"
        );
        player.playing = true;
        player.overlay.last_activity = Some(0.0);
        let mut normal_idle = PlayerResponse::default();
        let _ = ctx.run(
            egui::RawInput {
                time: Some(90.0),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(470.0, 550.0),
                )),
                ..Default::default()
            },
            |ctx| {
                ctx.input_mut(|input| input.pointer = Default::default());
                egui::CentralPanel::default().show(ctx, |ui| {
                    normal_idle = player.show(ui, &mut clip, true, true, 530.0, true);
                });
            },
        );
        assert!(
            normal_idle.controls_visible,
            "Normal controls remain visible during idle playback"
        );
        assert!(
            normal_idle.video_plane.unwrap().bottom() <= normal_idle.controls_bounds.unwrap().top()
        );
        player.set_viewing_mode(true);
        player.overlay.last_activity = Some(0.0);
        player.controls_rect = None;
        let mut hidden = PlayerResponse::default();
        let _ = ctx.run(
            egui::RawInput {
                time: Some(100.0),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(470.0, 550.0),
                )),
                ..Default::default()
            },
            |ctx| {
                ctx.input_mut(|input| input.pointer = Default::default());
                egui::CentralPanel::default().show(ctx, |ui| {
                    hidden = player.show(ui, &mut clip, true, true, 530.0, true);
                });
            },
        );
        assert!(
            !hidden.controls_visible,
            "Idle playback must render without an overlay"
        );
        let mut revealed = PlayerResponse::default();
        let _ = ctx.run(
            egui::RawInput {
                time: Some(101.0),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(470.0, 550.0),
                )),
                events: vec![egui::Event::PointerMoved(
                    hidden.video_plane.unwrap().center(),
                )],
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    revealed = player.show(ui, &mut clip, true, true, 530.0, true);
                });
            },
        );
        assert!(
            revealed.controls_visible,
            "Moving over the video reveals the timeline"
        );
        assert_eq!(
            hidden.video_plane, revealed.video_plane,
            "Hiding the overlay must not resize the video"
        );
        player.playing = false;
        player.set_viewing_mode(false);
        let moments_before_viewing = clip.analysis.moments.clone();
        player.focus_range(0.1, 0.4);
        player.set_viewing_mode(true);
        assert!(player.timeline_view.is_none());
        let mut view_response = PlayerResponse::default();
        let mut draw_view = |events: Vec<egui::Event>| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 700.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        view_response = player.show(ui, &mut clip, true, true, 680.0, true);
                    });
                },
            );
            std::mem::take(&mut view_response)
        };
        draw_view(vec![]);
        draw_view(vec![]);
        let basic = draw_view(vec![]);
        assert!(basic.volume_slider.is_some() && basic.mode_button.is_some());
        assert!(
            basic.export_button.is_none()
                && basic.rescan_button.is_none()
                && basic.divider.is_none(),
            "Viewing mode only exposes playback controls and timelines"
        );
        let slider = basic.volume_slider.unwrap();
        let volume_point = egui::pos2(slider.left() + 25.0, slider.center().y);
        for pressed in [true, false] {
            draw_view(vec![
                egui::Event::PointerMoved(volume_point),
                pointer(volume_point, pressed),
            ]);
        }
        let back = basic.mode_button.unwrap().center();
        draw_view(vec![egui::Event::PointerMoved(back), pointer(back, true)]);
        assert!(
            draw_view(vec![pointer(back, false)]).toggle_expand,
            "Back to editor restores the full workspace"
        );
        drop(draw_view);
        assert!(
            player.volume < 0.8,
            "The visible volume slider changes playback volume"
        );
        assert!((player.pipeline.property::<f64>("volume") - player.volume).abs() < 0.001);
        assert_eq!(clip.analysis.moments, moments_before_viewing);
        assert!(
            player.controls_rect.unwrap().height() < 230.0,
            "The viewing overlay stays compact"
        );
        player.set_viewing_mode(false);
        // The main shortcut handler must recognize the platform's command modifier.
        let _ = ctx.run(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Z,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers {
                        ctrl: true,
                        command: true,
                        ..Default::default()
                    },
                }],
                ..Default::default()
            },
            |ctx| player.handle_shortcuts(ctx),
        );
        assert_eq!(player.history_action, -1);
        drop(player);
        assert!(path.exists());
        drop(directory);
    }
}
