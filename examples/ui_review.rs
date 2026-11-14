#[cfg(debug_assertions)]
mod preview {
    use eframe::egui;
    use std::path::PathBuf;
    struct Review {
        app: Box<dyn eframe::App>,
        output: PathBuf,
        frames: usize,
        capture_delay: f64,
        captured: bool,
        idle_pointer: bool,
    }
    impl eframe::App for Review {
        fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
            if self.idle_pointer {
                ctx.input_mut(|input| {
                    input.pointer = Default::default();
                    input
                        .events
                        .retain(|event| matches!(event, egui::Event::Screenshot { .. }));
                });
            }
            self.app.update(ctx, frame);
            self.frames += 1;
            if self.frames >= 20
                && !self.captured
                && ctx.input(|input| input.time) >= self.capture_delay
            {
                self.captured = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
            for event in ctx.input(|input| input.events.clone()) {
                if let egui::Event::Screenshot { image, .. } = event {
                    let pixels: Vec<u8> = image
                        .pixels
                        .iter()
                        .flat_map(|pixel| pixel.to_array())
                        .collect();
                    image::save_buffer(
                        &self.output,
                        &pixels,
                        image.width() as u32,
                        image.height() as u32,
                        image::ColorType::Rgba8,
                    )
                    .unwrap();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
    }
    pub fn run() -> eframe::Result {
        let args: Vec<_> = std::env::args().collect();
        let source = PathBuf::from(&args[1]);
        let output = PathBuf::from(&args[2]);
        let state = args.get(3).map_or("editor", String::as_str);
        let width = args
            .get(4)
            .and_then(|arg| arg.parse().ok())
            .unwrap_or(1440.0);
        let height = args
            .get(5)
            .and_then(|arg| arg.parse().ok())
            .unwrap_or(960.0);
        eframe::run_native(
            "HDContext UI review",
            eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default().with_inner_size([width, height]),
                ..Default::default()
            },
            Box::new(move |cc| {
                Ok(Box::new(Review {
                    app: hdcontext::desktop::review_app(cc, source, state),
                    output,
                    frames: 0,
                    capture_delay: if state == "hidden" { 3.2 } else { 0.0 },
                    captured: false,
                    idle_pointer: state == "hidden",
                }))
            }),
        )
    }
}
#[cfg(debug_assertions)]
fn main() -> eframe::Result {
    preview::run()
}
#[cfg(not(debug_assertions))]
fn main() {
    eprintln!("Run this visual review example without --release.");
}
