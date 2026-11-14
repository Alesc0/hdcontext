use crate::{
    ai::Analyzer,
    player::Player,
    preferences::{self, ModelSettings, Preferences},
    provider::{Provider, configured_audio_model},
    report::{self, Clip, Failure, Report},
    scan_cache::{Cache, FileStamp, Record, State},
    segments, watch, watch_service,
    workflow::discover,
};
use anyhow::{Context, Result, ensure};
use eframe::egui::{self, Color32, RichText};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
};

type CachedRecords = Result<Vec<(Record, bool)>, String>;

const ACCENT: Color32 = Color32::from_rgb(120, 224, 186);
const MUTED: Color32 = Color32::from_rgb(162, 177, 195);

pub fn launch(free_tier: bool) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 960.0])
            .with_min_inner_size([1000.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native(
        "HDContext",
        options,
        Box::new(move |cc| {
            workspace_style(&cc.egui_ctx);
            let mut app = Desktop::default();
            app.enable_remembering();
            if free_tier {
                app.use_gemini_free_tier();
            }
            if app.watch_in_background {
                app.configure_background(true, &cc.egui_ctx);
            }
            Ok(Box::new(app))
        }),
    )
}

fn workspace_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = Color32::from_rgb(18, 24, 33);
    style.visuals.window_fill = Color32::from_rgb(24, 33, 45);
    style.visuals.extreme_bg_color = Color32::from_rgb(11, 17, 25);
    style.visuals.override_text_color = Some(Color32::from_rgb(224, 232, 242));
    style.visuals.selection.bg_fill = Color32::from_rgb(37, 83, 70);
    style.visuals.selection.stroke = egui::Stroke::new(1.0_f32, Color32::from_rgb(120, 224, 186));
    style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(32, 44, 59);
    style.visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(27, 38, 52);
    style.visuals.widgets.inactive.bg_stroke =
        egui::Stroke::new(1.0_f32, Color32::from_rgb(49, 66, 85));
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(43, 62, 79);
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(42, 85, 75);
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(11.0, 7.0);
    style.spacing.interact_size.y = 30.0;
    for (kind, size) in [
        (egui::TextStyle::Body, 15.0),
        (egui::TextStyle::Button, 14.0),
        (egui::TextStyle::Small, 12.0),
        (egui::TextStyle::Heading, 22.0),
        (egui::TextStyle::Monospace, 13.0),
    ] {
        style.text_styles.insert(
            kind.clone(),
            if kind == egui::TextStyle::Monospace {
                egui::FontId::monospace(size)
            } else {
                egui::FontId::proportional(size)
            },
        );
    }
    ctx.set_style(style);
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    #[default]
    Analysis,
    Folders,
    Storage,
}

/// Isolated visual review fixture. Never loads preferences, credentials, or API clients.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn review_app(
    cc: &eframe::CreationContext<'_>,
    source: PathBuf,
    state: &str,
) -> Box<dyn eframe::App> {
    workspace_style(&cc.egui_ctx);
    let mut app = Desktop {
        auto_find: false,
        allow_close: true,
        ..Default::default()
    };
    for provider in &mut app.providers {
        provider.key.clear();
    }
    if state != "empty" {
        let media = crate::media::probe(&source).expect("review fixture video");
        let moments = [
            (42.0, 57.0, "3-kill clutch"),
            (104.0, 114.0, "Shotgun whiff"),
            (148.0, 163.0, "Last-second revive"),
        ]
        .into_iter()
        .map(|(start, end, label)| crate::ai::Moment {
            start_seconds: start,
            end_seconds: end,
            description: label.into(),
            kept: label.contains("clutch"),
            exported_to: None,
        })
        .collect();
        let clip = Clip {
            source: source.clone(),
            sha256: crate::media::fingerprint(&source).expect("review fixture hash"),
            provider: Provider::Gemini,
            model: "gemini-3.5-flash-lite".into(),
            audio_model: None,
            proposed_name: String::new(),
            media: media.clone(),
            analysis: crate::ai::Analysis {
                moments,
                ..Default::default()
            },
        };
        let mut row = Row::new(source.clone());
        row.clip = Some(clip.clone());
        app.rows.push(row);
        for (file, label) in [
            ("Replay_2026-11-14_14-50-54.mp4", "Missed shotgun shot"),
            ("Replay_2026-11-13_21-08-33.mp4", "Squad wipe"),
            ("Replay_2026-11-13_20-42-12.mp4", "Lucky wallbang"),
        ] {
            let path = source.parent().unwrap().join(file);
            let mut clip = clip.clone();
            clip.source = path.clone();
            clip.analysis.moments.truncate(1);
            clip.analysis.moments[0].description = label.into();
            let mut row = Row::new(path);
            row.clip = Some(clip);
            app.rows.push(row);
        }
        app.active_source = Some(source.clone());
        app.folder = "/Recordings/Hunt Showdown".into();
        app.status = "Highlights ready".into();
        app.player =
            Some(Player::open_source(source, media.duration_seconds, 0).expect("review player"));
        if state == "error" {
            app.rows[0].error=Some("Provider limit reached. Your completed highlights are saved. Wait for your quota to reset before trying again.".into());
        }
        if state == "settings" || state == "folders" {
            app.settings_open = true;
        }
    }
    if state == "folders" {
        app.settings_open = true;
        app.settings_page = SettingsPage::Folders;
        app.watch_in_background = true;
        app.watched_folders.push(watch::Folder {
            path: "/Recordings/Hunt Showdown".into(),
            enabled: true,
            recursive: true,
            provider: Provider::Gemini,
            model: "gemini-3.5-flash-lite".into(),
            audio_model: String::new(),
            hint: "Hunt Showdown, PvP highlights".into(),
            max_seconds: 300,
        });
    }
    if state == "expanded" || state == "hidden" {
        app.player_expanded = true;
    }
    if state == "storage" {
        app.settings_open = true;
        app.settings_page = SettingsPage::Storage;
    }
    if state == "zoom"
        && let Some(player) = &mut app.player
    {
        player.focus_range(42.0, 57.0);
    }
    if state == "export" || state == "replace" {
        app.prepare_export(app.selected_jobs(0, 0));
        if state == "replace" {
            app.export_dialog.as_mut().unwrap().options.replace_original = true;
        }
    }
    Box::new(app)
}

#[derive(Clone, Serialize, Deserialize)]
struct Row {
    source: PathBuf,
    selected: bool,
    clip: Option<Clip>,
    #[serde(default)]
    manual_only: bool,
    history: Vec<Clip>,
    error: Option<String>,
    #[serde(skip)]
    working: bool,
    #[serde(default)]
    cache_revision: u128,
    #[serde(skip)]
    edited_during_scan: bool,
    #[serde(skip)]
    queued: bool,
    #[serde(skip)]
    replaced: bool,
}

impl Row {
    fn new(source: PathBuf) -> Self {
        Self {
            source,
            selected: false,
            clip: None,
            manual_only: false,
            history: Vec::new(),
            error: None,
            working: false,
            cache_revision: 0,
            edited_during_scan: false,
            queued: false,
            replaced: false,
        }
    }

    fn generated(&mut self, result: Result<Clip, String>) {
        self.working = false;
        self.edited_during_scan = false;
        self.queued = false;
        match result {
            Ok(clip) => {
                self.manual_only = false;
                if let Some(previous) = self.clip.replace(clip) {
                    self.history.push(previous);
                }
                // A new suggestion always needs a fresh selection before renaming.
                self.selected = false;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn restore(&mut self, index: usize) {
        let previous = self.history.remove(index);
        self.manual_only = previous.model == "manual";
        if let Some(current) = self.clip.replace(previous) {
            self.history.push(current);
        }
        self.selected = false;
    }
}

type ExportJob = (usize, usize, Clip, crate::ai::Moment);
struct ExportRequest {
    jobs: Vec<ExportJob>,
    folder: PathBuf,
    options: segments::ExportOptions,
}
struct ExportGroup {
    clip: Clip,
    members: Vec<(usize, usize, crate::ai::Moment)>,
}

enum Event {
    Scanned(Vec<PathBuf>),
    Started(usize),
    Generated(usize, Box<Result<Clip, String>>),
    ProviderLimited(Provider),
    Exported(usize, usize, PathBuf),
    Replaced(PathBuf, Vec<Clip>),
    Finished(Result<String, String>),
}

enum OpenEvent {
    Ready(Box<Clip>, Box<Player>),
    Verified(String),
    Failed(String),
    Finished,
}
struct Opening {
    source: PathBuf,
    receiver: Receiver<OpenEvent>,
    cancelled: Arc<AtomicBool>,
}
impl Drop for Opening {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

struct Work {
    receiver: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    total: usize,
    completed: usize,
    cancellable: bool,
}

#[derive(Serialize, Deserialize)]
struct SavedReview {
    #[serde(flatten)]
    report: Report,
    #[serde(default)]
    review: Option<ReviewState>,
}

#[derive(Serialize, Deserialize)]
struct ReviewState {
    folder: String,
    recursive: bool,
    hint: String,
    #[serde(default)]
    folder_contexts: Option<crate::context::FolderContexts>,
    max_seconds: u32,
    rows: Vec<Row>,
    #[serde(default)]
    provider_models: Option<[ModelSettings; 2]>,
}

struct ProviderSettings {
    key: String,
    models: ModelSettings,
}

impl ProviderSettings {
    fn from_env(provider: Provider) -> Self {
        Self {
            key: std::env::var(provider.key_env()).unwrap_or_default(),
            models: ModelSettings {
                model: provider.configured_model(),
                audio_model: configured_audio_model(),
            },
        }
    }
}

#[derive(Clone, Copy)]
enum Pending {
    Scan,
    Open,
    Close,
}

struct Remember {
    path: PathBuf,
    worker: preferences::Worker,
    key_ready: [bool; 2],
    key_edited: [bool; 2],
    loaded: bool,
    observed: Option<preferences::Snapshot>,
    last_sent: Option<preferences::Snapshot>,
    changed_at: std::time::Instant,
    revision: u64,
    completed_revision: u64,
    settings_error: Option<String>,
    key_errors: [Option<String>; 2],
}

impl Remember {
    fn new(path: PathBuf, worker: preferences::Worker, key_ready: [bool; 2]) -> Self {
        Self {
            path,
            worker,
            key_ready,
            key_edited: [false, false],
            loaded: false,
            observed: None,
            last_sent: None,
            changed_at: std::time::Instant::now(),
            revision: 0,
            completed_revision: 0,
            settings_error: None,
            key_errors: [None, None],
        }
    }
}

struct Desktop {
    folder: String,
    recursive: bool,
    provider: Provider,
    providers: [ProviderSettings; 2],
    hint: String,
    folder_contexts: crate::context::FolderContexts,
    context_folder: Option<PathBuf>,
    context_inferred: bool,
    max_seconds: u32,
    rows: Vec<Row>,
    work: Option<Work>,
    status: String,
    error: Option<String>,
    dirty: bool,
    player: Option<Player>,
    opening: Option<Opening>,
    active_source: Option<PathBuf>,
    settings_open: bool,
    player_expanded: bool,
    settings_page: SettingsPage,
    show_key: bool,
    library_view: usize,
    auto_find: bool,
    auto_queue: VecDeque<PathBuf>,
    auto_attempted: HashSet<PathBuf>,
    auto_limit_blocked: bool,
    analysis_running: bool,
    analysis_baselines: HashMap<PathBuf, Vec<crate::ai::Moment>>,
    pending: Option<Pending>,
    allow_close: bool,
    filter: String,
    remember: Option<Remember>,
    remember_error: Option<String>,
    watched_folders: Vec<watch::Folder>,
    watch_in_background: bool,
    local_watch_cancel: Option<Arc<AtomicBool>>,
    cache: Option<Cache>,
    cache_poll: Option<Receiver<CachedRecords>>,
    cache_next: std::time::Instant,
    cache_error: Option<String>,
    service_job: Option<(bool, Receiver<Result<(), String>>)>,
    service_notice: Option<String>,
    service_problem: Option<(bool, String)>,
    export_dialog: Option<ExportRequest>,
    replacement_to_open: Option<PathBuf>,
    reopen_after_export: bool,
}

impl Default for Desktop {
    fn default() -> Self {
        Self {
            folder: String::new(),
            recursive: true,
            provider: if std::env::var("GEMINI_API_KEY").is_ok_and(|k| !k.trim().is_empty()) {
                Provider::Gemini
            } else if std::env::var("OPENAI_API_KEY").is_ok_and(|k| !k.trim().is_empty()) {
                Provider::OpenAi
            } else {
                Provider::Gemini
            },
            providers: Provider::ALL.map(ProviderSettings::from_env),
            hint: String::new(),
            folder_contexts: Default::default(),
            context_folder: None,
            context_inferred: false,
            max_seconds: 300,
            rows: Vec::new(),
            work: None,
            status: "Choose a folder to find your game clips.".into(),
            error: None,
            dirty: false,
            player: None,
            opening: None,
            active_source: None,
            settings_open: false,
            player_expanded: false,
            settings_page: SettingsPage::Analysis,
            show_key: false,
            library_view: 0,
            auto_find: true,
            auto_queue: VecDeque::new(),
            auto_attempted: HashSet::new(),
            auto_limit_blocked: false,
            analysis_running: false,
            analysis_baselines: HashMap::new(),
            pending: None,
            allow_close: false,
            filter: String::new(),
            remember: None,
            remember_error: None,
            watched_folders: Vec::new(),
            watch_in_background: false,
            local_watch_cancel: None,
            cache: None,
            cache_poll: None,
            cache_next: std::time::Instant::now(),
            cache_error: None,
            service_job: None,
            service_notice: None,
            service_problem: None,
            export_dialog: None,
            replacement_to_open: None,
            reopen_after_export: false,
        }
    }
}

impl Desktop {
    fn remembered_preferences(&self) -> Preferences {
        Preferences {
            version: 1,
            folder: self.folder.clone(),
            recursive: self.recursive,
            provider: self.provider,
            models: self.providers.each_ref().map(|p| p.models.clone()),
            hint: self.hint.clone(),
            folder_contexts: Some(self.folder_contexts.clone()),
            max_seconds: self.max_seconds,
            auto_find: self.auto_find,
            watched_folders: self.watched_folders.clone(),
            watch_in_background: self.watch_in_background,
        }
    }

    fn remembered_snapshot(&self, key_ready: [bool; 2]) -> preferences::Snapshot {
        preferences::Snapshot {
            preferences: self.remembered_preferences(),
            keys: Provider::ALL.map(|provider| {
                key_ready[provider.index()].then(|| self.providers[provider.index()].key.clone())
            }),
        }
    }

    fn enable_remembering(&mut self) {
        let result = (|| -> Result<Remember> {
            let path = preferences::config_path()?;
            if let Some(saved) = preferences::load(&path)? {
                self.apply_remembered_preferences(saved);
            }
            let key_ready = self.providers.each_ref().map(|p| !p.key.trim().is_empty());
            Ok(Remember::new(
                path.clone(),
                preferences::Worker::start(path),
                key_ready,
            ))
        })();
        match result {
            Ok(remember) => {
                match Cache::from_config(&remember.path) {
                    Ok(cache) => self.cache = Some(cache),
                    Err(error) => {
                        self.cache_error = Some(format!("Could not open scan storage: {error:#}"))
                    }
                }
                self.remember = Some(remember);
                self.remember_error = None;
            }
            Err(error) => {
                self.remember_error = Some(format!(
                    "Could not restore settings: {error}. Automatic saving is disabled so the existing file is preserved."
                ))
            }
        }
    }

    fn apply_remembered_preferences(&mut self, saved: Preferences) {
        self.folder_contexts = saved.effective_contexts();
        self.context_folder = None;
        self.folder = saved.folder;
        self.recursive = saved.recursive;
        self.provider = saved.provider;
        self.hint = saved.hint;
        self.max_seconds = saved.max_seconds;
        self.auto_find = saved.auto_find;
        self.watched_folders = saved.watched_folders;
        self.watch_in_background = saved.watch_in_background;
        for (settings, models) in self.providers.iter_mut().zip(saved.models) {
            settings.models = models;
        }
        self.sync_context_editor();
    }

    fn poll_remembering(&mut self, ctx: &egui::Context) {
        let Some(mut remember) = self.remember.take() else {
            return;
        };
        for event in remember.worker.events.try_iter() {
            match event {
                preferences::Event::Loaded(keys) => {
                    ctx.request_repaint();
                    for (index, result) in keys.into_iter().enumerate() {
                        match result {
                            Ok(key) => {
                                if !remember.key_edited[index]
                                    && self.providers[index].key.is_empty()
                                {
                                    self.providers[index].key = key.unwrap_or_default();
                                }
                                remember.key_ready[index] = true;
                                remember.key_errors[index] = None;
                            }
                            Err(error) => remember.key_errors[index] = Some(error),
                        }
                    }
                    remember.loaded = true;
                }
                preferences::Event::Saved {
                    revision,
                    settings,
                    keys,
                } => {
                    remember.completed_revision = remember.completed_revision.max(revision);
                    if revision == remember.revision {
                        remember.settings_error = settings.err();
                        for (index, result) in keys.into_iter().enumerate() {
                            if let Some(result) = result {
                                remember.key_errors[index] = result.err();
                            }
                        }
                    }
                }
            }
        }
        if remember.loaded {
            let snapshot = self.remembered_snapshot(remember.key_ready);
            if remember.observed.as_ref() != Some(&snapshot) {
                remember.observed = Some(snapshot.clone());
                remember.changed_at = std::time::Instant::now();
            }
            if remember.last_sent.as_ref() != Some(&snapshot) {
                let debounce = std::time::Duration::from_millis(600);
                if remember.changed_at.elapsed() >= debounce {
                    remember.revision += 1;
                    remember.worker.save(snapshot.clone(), remember.revision);
                    remember.last_sent = Some(snapshot);
                } else {
                    ctx.request_repaint_after(
                        debounce.saturating_sub(remember.changed_at.elapsed()),
                    );
                }
            }
        }
        if !remember.loaded || remember.completed_revision < remember.revision {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.remember = Some(remember);
    }

    fn save_preferences_now(&mut self) {
        let Some(mut remember) = self.remember.take() else {
            return;
        };
        let snapshot = self.remembered_snapshot(remember.key_ready);
        remember.revision += 1;
        remember.worker.save(snapshot.clone(), remember.revision);
        remember.last_sent = Some(snapshot.clone());
        remember.observed = Some(snapshot);
        self.remember = Some(remember);
    }

    fn forget_key(&mut self) {
        let index = self.provider.index();
        self.providers[index].key.clear();
        if let Some(remember) = &mut self.remember {
            remember.key_ready[index] = true;
            remember.key_edited[index] = true;
        }
        self.save_preferences_now();
    }

    fn settings(&self) -> &ProviderSettings {
        &self.providers[self.provider.index()]
    }

    fn select_provider(&mut self, provider: Provider) {
        self.provider = provider;
        self.error = None;
        if !self.rows.is_empty() {
            self.dirty = true;
        }
    }

    fn use_gemini_free_tier(&mut self) {
        self.select_provider(Provider::Gemini);
        self.providers[Provider::Gemini.index()].models.model =
            Provider::Gemini.default_model().into();
        self.status = "Gemini 3.5 Flash-Lite selected. Use a key from a Free Tier project in Google AI Studio, then try Find highlight on one recording.".into();
    }

    fn spawn(
        &mut self,
        ctx: &egui::Context,
        total: usize,
        cancellable: bool,
        task: impl FnOnce(&Sender<Event>, &AtomicBool) -> Result<String> + Send + 'static,
    ) {
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let context = ctx.clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                task(&sender, &worker_cancel)
            }))
            .map_err(|_| {
                "The background task stopped unexpectedly. Completed results are still available."
                    .to_string()
            })
            .and_then(|result| result.map_err(|error| format!("{error:#}")));
            let _ = sender.send(Event::Finished(result));
            context.request_repaint();
        });
        self.work = Some(Work {
            receiver,
            cancel,
            total,
            completed: 0,
            cancellable,
        });
        self.error = None;
    }

    fn poll(&mut self) {
        let events: Vec<_> = self
            .work
            .as_ref()
            .map(|w| w.receiver.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            match event {
                Event::Scanned(paths) => {
                    self.rows = paths.into_iter().map(Row::new).collect();
                    self.dirty = false;
                }
                Event::Started(index) => {
                    self.rows[index].working = true;
                    self.status = format!(
                        "Analyzing {}",
                        self.rows[index]
                            .source
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                    );
                }
                Event::Generated(index, result) => {
                    let mut result = *result;
                    if let Some(baseline) = self.analysis_baselines.remove(&self.rows[index].source)
                        && let (Ok(generated), Some(current)) =
                            (&mut result, &self.rows[index].clip)
                    {
                        let mut preserved: Vec<_> = current
                            .analysis
                            .moments
                            .iter()
                            .enumerate()
                            .filter(|(i, moment)| baseline.get(*i) != Some(*moment))
                            .map(|(_, moment)| moment.clone())
                            .collect();
                        for moment in &generated.analysis.moments {
                            if !preserved.iter().any(|old| {
                                old.start_seconds == moment.start_seconds
                                    && old.end_seconds == moment.end_seconds
                                    && old.description == moment.description
                            }) {
                                preserved.push(moment.clone());
                            }
                        }
                        generated.analysis.moments = preserved;
                    }
                    self.rows[index].generated(result);
                    if let Some(cache) = &self.cache
                        && let Ok(Some(record)) = cache.load(&self.rows[index].source)
                    {
                        self.rows[index].cache_revision = record.revision;
                    }
                    self.dirty |= self.cache.is_none();
                    if let Some(work) = &mut self.work {
                        work.completed += 1;
                    }
                }
                Event::ProviderLimited(provider) => {
                    self.auto_limit_blocked = true;
                    self.auto_queue.clear();
                    if let Some(cache) = &self.cache {
                        let _ = cache.set_paused(provider, true);
                    }
                }
                Event::Exported(row, moment, destination) => {
                    self.rows[row].clip.as_mut().unwrap().analysis.moments[moment].exported_to =
                        Some(destination);
                    self.dirty = true;
                    if let Some(work) = &mut self.work {
                        work.completed += 1;
                    }
                }
                Event::Replaced(source, outputs) => {
                    for row in &mut self.rows {
                        if row.source == source {
                            row.replaced = true;
                        }
                    }
                    if self.active_source.as_ref() == Some(&source) {
                        self.player = None;
                        self.opening = None;
                        self.active_source = outputs.first().map(|clip| clip.source.clone());
                        self.replacement_to_open = self.active_source.clone();
                    }
                    if let Some(work) = &mut self.work {
                        work.completed += outputs.len();
                    }
                    for clip in outputs {
                        let mut row = Row::new(clip.source.clone());
                        if let Some(cache) = &self.cache
                            && let Ok(Some(record)) = cache.load(&clip.source)
                        {
                            row.cache_revision = record.revision;
                        }
                        row.clip = Some(clip);
                        self.rows.push(row);
                    }
                    self.dirty = true;
                }
                Event::Finished(result) => {
                    for row in &mut self.rows {
                        row.working = false;
                    }
                    self.work = None;
                    self.rows.retain(|row| !row.replaced);
                    if self.reopen_after_export {
                        self.replacement_to_open = self.active_source.clone();
                        self.reopen_after_export = false;
                    }
                    self.analysis_running = false;
                    self.analysis_baselines.clear();
                    match result {
                        Ok(status) => {
                            self.status = status;
                            self.error = None;
                        }
                        Err(error) => {
                            self.status = "Task stopped. Completed changes are shown below.".into();
                            self.error = Some(error);
                        }
                    }
                }
            }
        }
    }

    fn request(&mut self, action: Pending, ctx: &egui::Context) {
        if self.dirty {
            self.pending = Some(action);
        } else {
            self.perform(action, ctx);
        }
    }

    fn perform(&mut self, action: Pending, ctx: &egui::Context) {
        self.player = None;
        self.opening = None;
        self.active_source = None;
        self.auto_queue.clear();
        self.auto_attempted.clear();
        self.auto_limit_blocked = false;
        match action {
            Pending::Scan => {
                let path = PathBuf::from(self.folder.trim());
                let recursive = self.recursive;
                self.status = "Finding video files...".into();
                self.spawn(ctx, 0, false, move |sender, _| {
                    let files = discover(&[path], recursive)?;
                    let count = files.len();
                    let _ = sender.send(Event::Scanned(files));
                    Ok(format!(
                        "Found {count} recordings. Find highlights to start reviewing."
                    ))
                });
            }
            Pending::Open => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("HDContext report", &["json"])
                    .pick_file()
                    && let Err(error) = self.open_path(&path)
                {
                    self.error = Some(format!("Cannot open report: {error:#}"));
                }
            }
            Pending::Close => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn snapshot(&self) -> SavedReview {
        SavedReview {
            report: Report {
                version: 1,
                provider: self.provider,
                model: self.settings().models.model.clone(),
                clips: self.rows.iter().filter_map(|r| r.clip.clone()).collect(),
                failures: self
                    .rows
                    .iter()
                    .filter_map(|r| {
                        r.error.as_ref().map(|error| Failure {
                            source: r.source.clone(),
                            error: error.clone(),
                        })
                    })
                    .collect(),
            },
            review: Some(ReviewState {
                folder: self.folder.clone(),
                recursive: self.recursive,
                hint: self.hint.clone(),
                folder_contexts: Some(self.folder_contexts.clone()),
                max_seconds: self.max_seconds,
                rows: self.rows.clone(),
                provider_models: Some(self.providers.each_ref().map(|p| p.models.clone())),
            }),
        }
    }

    fn open_path(&mut self, path: &std::path::Path) -> Result<()> {
        let saved: SavedReview = serde_json::from_reader(std::fs::File::open(path)?)?;
        ensure!(saved.report.version == 1, "Unsupported report version");
        self.provider = saved.report.provider;
        self.providers[self.provider.index()].models.model = saved.report.model;
        if let Some(review) = saved.review {
            if let Some(models) = review.provider_models {
                for (settings, models) in self.providers.iter_mut().zip(models) {
                    settings.models = models;
                }
            }
            if let Some(contexts) = review.folder_contexts {
                self.folder_contexts.extend(contexts);
            } else if !review.folder.is_empty() && !review.hint.is_empty() {
                self.folder_contexts.insert(
                    crate::context::folder_key(std::path::Path::new(&review.folder)),
                    review.hint.clone(),
                );
            }
            self.folder = review.folder;
            self.recursive = review.recursive;
            self.hint = review.hint;
            self.max_seconds = review.max_seconds.max(1);
            self.rows = review.rows;
        } else {
            self.rows = saved
                .report
                .clips
                .into_iter()
                .map(|clip| {
                    let mut row = Row::new(clip.source.clone());
                    row.manual_only = clip.model == "manual";
                    row.clip = Some(clip);
                    row
                })
                .collect();
            for failure in saved.report.failures {
                if let Some(row) = self.rows.iter_mut().find(|r| r.source == failure.source) {
                    row.error = Some(failure.error);
                } else {
                    let mut row = Row::new(failure.source);
                    row.error = Some(failure.error);
                    self.rows.push(row);
                }
            }
            self.folder = self
                .rows
                .first()
                .and_then(|r| r.source.parent())
                .map(|p| p.display().to_string())
                .unwrap_or_default();
        }
        self.context_folder = None;
        self.active_source = None;
        self.sync_context_editor();
        self.dirty = false;
        self.error = None;
        for row in &mut self.rows {
            if let Some(clip) = &mut row.clip {
                crate::ai::upgrade_legacy_moments(&mut clip.analysis, clip.media.duration_seconds);
            }
            for clip in &mut row.history {
                crate::ai::upgrade_legacy_moments(&mut clip.analysis, clip.media.duration_seconds);
            }
        }
        self.status = format!("Loaded {} clips from {}", self.rows.len(), path.display());
        Ok(())
    }

    fn save(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("HDContext report", &["json"])
            .set_file_name("hdcontext-report.json")
            .save_file()
        {
            match report::save_json(&self.snapshot(), &path) {
                Ok(()) => {
                    self.dirty = false;
                    self.status = format!("Saved {}", path.display());
                    self.error = None;
                }
                Err(error) => self.error = Some(format!("{error:#}")),
            }
        }
    }

    fn context_scope(&self) -> Option<PathBuf> {
        let path = if let Some(source) = &self.active_source {
            source.parent()?.to_owned()
        } else if !self.folder.trim().is_empty() {
            PathBuf::from(self.folder.trim())
        } else {
            return None;
        };
        Some(path)
    }

    fn sync_context_editor(&mut self) {
        let scope = self.context_scope();
        if scope != self.context_folder {
            if let Some(path) = &scope {
                (self.hint, self.context_inferred) =
                    crate::context::resolve(path, &self.folder_contexts, &self.watched_folders);
            }
            self.context_folder = scope;
        }
    }

    fn set_folder_context(&mut self, path: PathBuf, text: String) {
        let path = crate::context::folder_key(&path);
        self.folder_contexts.insert(path.clone(), text.clone());
        for folder in &mut self.watched_folders {
            if crate::context::folder_key(&folder.path) == path {
                folder.hint = text.clone();
            }
        }
        if let Some(scope) = &self.context_folder {
            (self.hint, self.context_inferred) =
                crate::context::resolve(scope, &self.folder_contexts, &self.watched_folders);
        }
    }

    fn scan_config(&self, source: &std::path::Path) -> watch::Folder {
        let mut config = self
            .watched_folders
            .iter()
            .filter(|folder| folder.contains(source))
            .max_by_key(|folder| folder.path.components().count())
            .cloned()
            .unwrap_or_else(|| watch::Folder {
                path: source.parent().unwrap_or(source).into(),
                enabled: true,
                recursive: false,
                provider: self.provider,
                model: self.settings().models.model.clone(),
                audio_model: self.settings().models.audio_model.clone(),
                hint: String::new(),
                max_seconds: self.max_seconds,
            });
        config.hint = crate::context::resolve(
            source.parent().unwrap_or(source),
            &self.folder_contexts,
            &self.watched_folders,
        )
        .0;
        config
    }

    fn request_rescan(&mut self, index: usize, ctx: &egui::Context) {
        let source = &self.rows[index].source;
        if self.watch_in_background
            && self
                .watched_folders
                .iter()
                .any(|folder| folder.enabled && folder.contains(source))
            && let Some(cache) = &self.cache
        {
            match cache
                .request_rescan(source)
                .and_then(|()| cache.set_paused(self.scan_config(source).provider, false))
            {
                Ok(()) => {
                    self.rows[index].queued = true;
                    self.rows[index].error = None;
                    self.auto_limit_blocked = false;
                    self.cache_next = std::time::Instant::now();
                }
                Err(error) => self.error = Some(format!("Could not queue rescan: {error:#}")),
            }
        } else {
            self.generate(vec![index], ctx);
        }
    }

    fn generate(&mut self, indices: Vec<usize>, ctx: &egui::Context) {
        self.generate_with_mode(indices, ctx, true);
    }

    fn generate_with_mode(&mut self, indices: Vec<usize>, ctx: &egui::Context, force: bool) {
        if indices.is_empty() {
            return;
        }
        let jobs: Vec<_> = indices
            .iter()
            .map(|&index| {
                let source = self.rows[index].source.clone();
                let config = self.scan_config(&source);
                let key = self.providers[config.provider.index()].key.clone();
                (
                    index,
                    source,
                    self.rows[index]
                        .clip
                        .as_ref()
                        .map(|clip| clip.proposed_name.clone()),
                    config,
                    key,
                )
            })
            .collect();
        if let Some((_, _, _, config, _)) =
            jobs.iter().find(|(_, _, _, _, key)| key.trim().is_empty())
        {
            self.error = Some(format!(
                "Enter your {} API key in Settings before finding highlights.",
                config.provider.label()
            ));
            return;
        }
        for &index in &indices {
            let row = &self.rows[index];
            self.auto_attempted.insert(row.source.clone());
            self.analysis_baselines.insert(
                row.source.clone(),
                if row.manual_only {
                    Vec::new()
                } else {
                    row.clip
                        .as_ref()
                        .map(|clip| clip.analysis.moments.clone())
                        .unwrap_or_default()
                },
            );
        }
        self.analysis_running = true;
        self.auto_limit_blocked = false;
        let cache = self.cache.clone();
        self.status = "Preparing analysis...".into();
        self.spawn(ctx, jobs.len(), true, move |sender, cancel| {
            let mut completed = 0; let mut failed = 0;
            for (index, source, previous, config, key) in jobs {
                if cancel.load(Ordering::Relaxed) { return Ok(format!("Stopped after {completed} clips. Completed suggestions are ready to review.")); }
                let analyzer = Analyzer::for_provider(config.provider, &key, &config.model, &config.audio_model)?;
                let _ = sender.send(Event::Started(index));
                let result = crate::scan_cache::analyze_cached(&source, &analyzer, Some(&config.hint), config.max_seconds, previous.as_deref(), cache.as_ref(), force);
                let stop_batch = result.as_ref().err().is_some_and(crate::ai::errors::should_stop_batch);
                let result = result.map_err(|error| format!("{error:#}"));
                let stop_reason = if stop_batch { result.as_ref().err().cloned() } else { None };
                if result.is_err() { failed += 1; } completed += 1;
                let _ = sender.send(Event::Generated(index, Box::new(result)));
                if let Some(reason) = stop_reason { let _ = sender.send(Event::ProviderLimited(config.provider)); anyhow::bail!("Generation stopped after a provider limit error. Remaining clips were not sent. {reason}"); }
            }
            Ok(format!("Scanned {completed} recordings, {failed} failed. Highlights are ready to review."))
        });
    }

    fn open_recording(&mut self, row: usize, ctx: &egui::Context) {
        let source = self.rows[row].source.clone();
        self.active_source = Some(source.clone());
        if self
            .player
            .as_ref()
            .is_some_and(|player| player.source() == source)
        {
            return;
        }
        self.player = None;
        self.opening = None;
        let existing = self.rows[row].clip.clone();
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.opening = Some(Opening {
            source: source.clone(),
            receiver,
            cancelled: cancelled.clone(),
        });
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<()> {
                let media = crate::media::probe(&source)?;
                if cancelled.load(Ordering::Relaxed) {
                    return Ok(());
                }
                let player = Player::open_source(source.clone(), media.duration_seconds, 0)?;
                let clip = existing.unwrap_or(Clip {
                    media,
                    sha256: String::new(),
                    source: source.clone(),
                    provider: Provider::default(),
                    model: "manual".into(),
                    audio_model: None,
                    proposed_name: String::new(),
                    analysis: crate::ai::Analysis::default(),
                });
                if cancelled.load(Ordering::Relaxed)
                    || sender
                        .send(OpenEvent::Ready(Box::new(clip), Box::new(player)))
                        .is_err()
                {
                    return Ok(());
                }
                ctx.request_repaint();
                // Verify source identity after handing playback to the UI, without delaying the first frame.
                let hash = crate::media::fingerprint(&source)?;
                if !cancelled.load(Ordering::Relaxed) {
                    let _ = sender.send(OpenEvent::Verified(hash));
                }
                Ok(())
            })();
            if let Err(error) = result {
                let _ = sender.send(OpenEvent::Failed(format!("{error:#}")));
            }
            let _ = sender.send(OpenEvent::Finished);
            ctx.request_repaint();
        });
    }

    fn poll_opening(&mut self) {
        let Some(opening) = &self.opening else {
            return;
        };
        let source = opening.source.clone();
        let events: Vec<_> = opening.receiver.try_iter().collect();
        for event in events {
            let Some(row) = self.rows.iter_mut().find(|row| row.source == source) else {
                self.opening = None;
                break;
            };
            match event {
                OpenEvent::Ready(clip, player) => {
                    if row.clip.is_none() {
                        row.clip = Some(*clip);
                        row.manual_only = true;
                    }
                    self.player = Some(*player);
                }
                OpenEvent::Verified(hash) => {
                    if let Some(clip) = &mut row.clip {
                        if clip.sha256.is_empty() {
                            clip.sha256 = hash;
                        } else if clip.sha256 != hash {
                            row.error = Some("Source changed since scanning. Rescan before exporting these highlights.".into());
                        }
                    }
                }
                OpenEvent::Failed(error) => {
                    row.error = Some(error);
                }
                OpenEvent::Finished => {
                    self.opening = None;
                }
            }
        }
    }

    fn poll_auto(&mut self, ctx: &egui::Context) {
        if !self.auto_find || self.auto_limit_blocked {
            return;
        }
        if let Some(source) = &self.active_source
            && self.rows.iter().any(|row| {
                &row.source == source && row.manual_only && row.clip.is_some() && !row.working
            })
            && !(self.watch_in_background
                && self
                    .watched_folders
                    .iter()
                    .any(|folder| folder.enabled && folder.contains(source)))
            && !self.auto_attempted.contains(source)
            && !self.auto_queue.contains(source)
        {
            self.auto_queue.push_back(source.clone());
        }
        if self.work.is_some() {
            return;
        }
        for _ in 0..self.auto_queue.len() {
            let source = self.auto_queue.pop_front().unwrap();
            if self.auto_attempted.contains(&source)
                || (self.watch_in_background
                    && self
                        .watched_folders
                        .iter()
                        .any(|folder| folder.enabled && folder.contains(&source)))
            {
                continue;
            }
            if let Some(index) = self.rows.iter().position(|row| {
                row.source == source && !row.working && (row.clip.is_none() || row.manual_only)
            }) {
                let config = self.scan_config(&source);
                if self.providers[config.provider.index()]
                    .key
                    .trim()
                    .is_empty()
                {
                    self.auto_queue.push_back(source);
                    continue;
                }
                self.generate_with_mode(vec![index], ctx, false);
                break;
            }
        }
    }

    fn configure_background(&mut self, enabled: bool, ctx: &egui::Context) {
        if self.service_job.is_some() {
            return;
        }
        self.watch_in_background = enabled;
        self.service_problem = None;
        let Some(remember) = &self.remember else {
            self.watch_in_background = false;
            self.service_notice =
                Some("Saved settings are required to configure background scans.".into());
            return;
        };
        let snapshot = self.remembered_snapshot(remember.key_ready);
        let saved = remember.worker.flush_async(snapshot);
        let config = remember.path.clone();
        let (sender, receiver) = mpsc::channel();
        self.service_job = Some((enabled, receiver));
        self.service_notice = Some("Updating background watcher...".into());
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<()> {
                saved.recv_timeout(std::time::Duration::from_secs(5)).map_err(|_| anyhow::anyhow!("Settings storage did not respond. Unlock the credential store and retry."))?;
                ensure!(preferences::load(&config)?.is_some_and(|saved| saved.watch_in_background == enabled), "Could not save background watcher settings");
                watch_service::configure(enabled, &std::env::current_exe()?, &config)
            })().map_err(|error| format!("{error:#}"));
            let _ = sender.send(result);
            ctx.request_repaint();
        });
    }

    fn watch_settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.separator();
        ui.heading("Automatic folder scans");
        ui.small("Scan existing and incoming videos after copying finishes. Each folder keeps its own model and game context. Results save automatically. Provider API quotas and charges apply.");
        if ui
            .add_enabled(
                self.remember.is_some(),
                egui::Button::new("Add watched folder..."),
            )
            .clicked()
            && let Some(path) = rfd::FileDialog::new().pick_folder()
        {
            match path.canonicalize() {
                Ok(path)
                    if !self
                        .watched_folders
                        .iter()
                        .any(|folder| folder.path == path) =>
                {
                    let hint = crate::context::resolve(
                        &path,
                        &self.folder_contexts,
                        &self.watched_folders,
                    )
                    .0;
                    self.watched_folders.push(watch::Folder {
                        path,
                        enabled: true,
                        recursive: self.recursive,
                        provider: self.provider,
                        model: self.settings().models.model.clone(),
                        audio_model: self.settings().models.audio_model.clone(),
                        hint,
                        max_seconds: self.max_seconds,
                    });
                    if self.watched_folders.len() == 1 && cfg!(target_os = "linux") {
                        self.configure_background(true, ctx);
                    }
                }
                Ok(_) => self.service_notice = Some("That folder is already watched.".into()),
                Err(error) => self.service_notice = Some(format!("Could not open folder: {error}")),
            }
        }
        let mut background = self.watch_in_background;
        if ui
            .add_enabled(
                self.remember.is_some() && self.service_job.is_none() && cfg!(target_os = "linux"),
                egui::Checkbox::new(&mut background, "Keep scanning when the app is closed"),
            )
            .changed()
        {
            self.configure_background(background, ctx);
        }
        ui.small(if self.watch_in_background { "Starts automatically at login and runs while you are signed in. Keys come from your credential store." } else { "Watched folders scan while this window is open." });
        if let Some(notice) = &self.service_notice {
            ui.small(notice);
        }
        if let Some((target, error)) = self.service_problem.clone() {
            ui.colored_label(Color32::LIGHT_RED, error);
            if self.service_job.is_none() && ui.button("Retry background setup").clicked() {
                self.configure_background(target, ctx);
            }
        }

        if let Some(error) = &self.cache_error {
            ui.colored_label(Color32::LIGHT_RED, error);
        }
        let mut remove = None;
        let mut context_changes = Vec::new();
        let models = self
            .providers
            .each_ref()
            .map(|settings| settings.models.clone());
        for (index, folder) in self.watched_folders.iter_mut().enumerate() {
            egui::CollapsingHeader::new(folder.path.display().to_string())
                .id_salt(("watched_folder", &folder.path))
                .show(ui, |ui| {
                    ui.checkbox(&mut folder.enabled, "Scan automatically");
                    ui.checkbox(&mut folder.recursive, "Include subfolders");
                    let previous = folder.provider;
                    egui::ComboBox::from_id_salt(("watch_provider", index))
                        .selected_text(folder.provider.label())
                        .show_ui(ui, |ui| {
                            for provider in Provider::ALL {
                                ui.selectable_value(
                                    &mut folder.provider,
                                    provider,
                                    provider.label(),
                                );
                            }
                        });
                    if folder.provider != previous {
                        folder.model = models[folder.provider.index()].model.clone();
                        folder.audio_model = models[folder.provider.index()].audio_model.clone();
                    }
                    ui.label("Model");
                    ui.add(
                        egui::TextEdit::singleline(&mut folder.model)
                            .id_salt(("watch_model", index)),
                    );
                    if folder.provider == Provider::OpenAi {
                        ui.label("Audio model");
                        ui.add(
                            egui::TextEdit::singleline(&mut folder.audio_model)
                                .id_salt(("watch_audio", index)),
                        );
                    }
                    ui.label("Game context");
                    if ui
                        .add(
                            egui::TextEdit::multiline(&mut folder.hint)
                                .id_salt(("watch_context", index))
                                .desired_rows(2),
                        )
                        .changed()
                    {
                        context_changes.push((folder.path.clone(), folder.hint.clone()));
                    }
                    ui.horizontal(|ui| {
                        ui.label("Max seconds");
                        ui.add(egui::DragValue::new(&mut folder.max_seconds).range(1..=3600));
                    });
                    if ui.small_button("Remove folder").clicked() {
                        remove = Some(index);
                    }
                });
        }
        for (path, text) in context_changes {
            self.set_folder_context(path, text);
        }
        if let Some(index) = remove {
            self.watched_folders.remove(index);
        }
        if ui.button("Resume paused automatic scans").clicked() {
            if let Some(cache) = &self.cache {
                for provider in Provider::ALL {
                    if let Err(error) = cache.set_paused(provider, false) {
                        self.cache_error = Some(format!("{error:#}"));
                    }
                }
            }
            self.auto_limit_blocked = false;
        }
    }

    fn poll_watches(&mut self, ctx: &egui::Context) {
        if let Some((enabled, receiver)) = &self.service_job {
            if let Ok(result) = receiver.try_recv() {
                let enabled = *enabled;
                self.service_job = None;
                match result {
                    Ok(()) => self.service_notice = Some(
                        if enabled {
                            "Background watcher enabled. Scans continue after closing this window."
                        } else {
                            "Background watcher stopped."
                        }
                        .into(),
                    ),
                    Err(error) => {
                        self.watch_in_background = !enabled;
                        self.service_notice = Some("Background setup could not finish.".into());
                        self.service_problem = Some((enabled, error));
                    }
                }
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        let local =
            !self.watch_in_background && self.watched_folders.iter().any(|folder| folder.enabled);
        if !local {
            if let Some(cancel) = self.local_watch_cancel.take() {
                cancel.store(true, Ordering::Relaxed);
            }
        } else if self.local_watch_cancel.is_none()
            && let Some(remember) = &self.remember
        {
            let config = remember.path.clone();
            let cancel = Arc::new(AtomicBool::new(false));
            self.local_watch_cancel = Some(cancel.clone());
            std::thread::spawn(move || {
                if let Err(error) = watch::run(&config, cancel, false) {
                    eprintln!("Folder watcher stopped: {error:#}");
                }
            });
        }
        if let Some(receiver) = &self.cache_poll
            && let Ok(result) = receiver.try_recv()
        {
            self.cache_poll = None;
            match result {
                Ok(records) => self.import_cached(records),
                Err(error) => self.cache_error = Some(error),
            }
        }
        if let Some(cache) = &self.cache {
            if cache.paused(self.provider).unwrap_or(false) {
                self.auto_limit_blocked = true;
            }
            if self.cache_poll.is_none() && std::time::Instant::now() >= self.cache_next {
                self.cache_next = std::time::Instant::now() + std::time::Duration::from_secs(2);
                let cache = cache.clone();
                let (sender, receiver) = mpsc::channel();
                self.cache_poll = Some(receiver);
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let result = (|| -> Result<Vec<(Record, bool)>> {
                        cache
                            .records()?
                            .into_iter()
                            .filter(|record| {
                                FileStamp::read(&record.source)
                                    .is_ok_and(|stamp| stamp == record.stamp)
                            })
                            .map(|record| {
                                let busy = matches!(record.state, State::Scanning)
                                    && cache.busy(&record.source)?;
                                Ok((record, busy))
                            })
                            .collect()
                    })()
                    .map_err(|error| format!("{error:#}"));
                    let _ = sender.send(result);
                    ctx.request_repaint();
                });
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
    }

    fn import_cached(&mut self, records: Vec<(Record, bool)>) {
        if self.work.is_some() && !self.analysis_running {
            return;
        }
        for (record, busy) in records {
            let index =
                if let Some(index) = self.rows.iter().position(|row| row.source == record.source) {
                    index
                } else if self
                    .watched_folders
                    .iter()
                    .any(|folder| folder.contains(&record.source))
                {
                    self.rows.push(Row::new(record.source.clone()));
                    self.rows.len() - 1
                } else {
                    continue;
                };
            let row = &mut self.rows[index];
            if self.analysis_baselines.contains_key(&record.source) {
                continue;
            }
            // A scanning record can outlive a killed process; locks distinguish active scans from interrupted ones.
            if row.cache_revision == record.revision && !matches!(record.state, State::Scanning) {
                continue;
            }
            row.cache_revision = record.revision;
            row.working = busy;
            row.queued = matches!(record.state, State::Queued);
            self.auto_attempted.insert(record.source.clone());
            self.auto_queue.retain(|source| source != &record.source);
            match record.state {
                State::Ready(mut clip) => {
                    if (row.manual_only || row.edited_during_scan)
                        && let Some(current) = &row.clip
                        && (current.sha256.is_empty() || current.sha256 == clip.sha256)
                    {
                        let mut moments = current.analysis.moments.clone();
                        for moment in &clip.analysis.moments {
                            if !moments.iter().any(|old| {
                                old.start_seconds == moment.start_seconds
                                    && old.end_seconds == moment.end_seconds
                                    && old.description == moment.description
                            }) {
                                moments.push(moment.clone());
                            }
                        }
                        clip.analysis.moments = moments;
                    }
                    row.generated(Ok(*clip));
                }
                State::Failed(error) => row.error = Some(error),
                State::Queued => row.error = None,
                State::Scanning if !busy => {
                    row.error =
                        Some("Scan interrupted. Open the video and rescan it in the player.".into())
                }
                State::Scanning => {}
            }
        }
    }

    fn kept_jobs(&self) -> Vec<ExportJob> {
        self.rows
            .iter()
            .enumerate()
            .flat_map(|(row_index, row)| {
                row.clip.iter().flat_map(move |clip| {
                    clip.analysis
                        .moments
                        .iter()
                        .enumerate()
                        .filter(|(_, moment)| moment.kept && moment.exported_to.is_none())
                        .map(move |(moment_index, moment)| {
                            (row_index, moment_index, clip.clone(), moment.clone())
                        })
                })
            })
            .collect()
    }
    fn selected_jobs(&self, row: usize, moment: usize) -> Vec<ExportJob> {
        let clip = self.rows[row].clip.as_ref().unwrap();
        vec![(
            row,
            moment,
            clip.clone(),
            clip.analysis.moments[moment].clone(),
        )]
    }
    fn prepare_export(&mut self, jobs: Vec<ExportJob>) {
        if jobs.is_empty() {
            return;
        }
        let folder = jobs[0]
            .2
            .source
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf();
        self.export_dialog = Some(ExportRequest {
            jobs,
            folder,
            options: segments::ExportOptions::default(),
        });
    }
    fn export_jobs(
        &mut self,
        jobs: Vec<ExportJob>,
        folder: PathBuf,
        options: segments::ExportOptions,
        ctx: &egui::Context,
    ) {
        if jobs.is_empty() {
            return;
        }
        // A cached result may have arrived while the export dialog was open.
        if jobs.iter().any(|(row, index, clip, moment)| {
            self.rows
                .get(*row)
                .and_then(|row| row.clip.as_ref())
                .is_none_or(|current| {
                    current.sha256 != clip.sha256
                        || current.analysis.moments.get(*index) != Some(moment)
                })
        }) {
            self.error=Some("The highlights changed while export options were open. Review the selection and export again.".into());
            return;
        }
        if options.replace_original
            && self
                .active_source
                .as_ref()
                .is_some_and(|source| jobs.iter().any(|job| &job.2.source == source))
        {
            self.player = None;
            self.opening = None;
            self.reopen_after_export = true;
        }
        let mut groups: Vec<ExportGroup> = Vec::new();
        for (row, index, clip, moment) in &jobs {
            if let Some(group) = groups
                .iter_mut()
                .find(|group| group.clip.source == clip.source)
            {
                group.members.push((*row, *index, moment.clone()));
            } else {
                groups.push(ExportGroup {
                    clip: clip.clone(),
                    members: vec![(*row, *index, moment.clone())],
                });
            }
        }
        let cache = self.cache.clone();
        self.status = "Exporting selected clips...".into();
        self.spawn(ctx,jobs.len(),true,move|sender,cancel|{
            let mut plans=Vec::new();let mut seen=HashSet::new();
            for group in &groups {
                let destination=if options.replace_original{group.clip.source.parent().context("Original recording has no folder")?}else{folder.as_path()};
                let jobs:Vec<_>=group.members.iter().map(|(row,index,moment)|(*row,*index,group.clip.clone(),moment.clone())).collect();
                let paths=segments::export_plan(&jobs,destination)?;
                for path in &paths{ensure!(seen.insert(path.to_string_lossy().to_lowercase()),"Two clips have the same export destination");}
                plans.push(paths);
            }
            let mut completed=0;let mut replaced=0;
            for(group,paths)in groups.iter().zip(plans) {
                if cancel.load(Ordering::Relaxed){return Ok(format!("Export stopped. {completed} clips saved; unfinished original recordings were kept."));}
                let moments:Vec<_>=group.members.iter().map(|(_,_,moment)|moment.clone()).collect();
                let outputs=segments::export_group(&group.clip,&moments,&paths,options,cancel,cache.as_ref())?;
                completed+=outputs.len();
                if options.replace_original{replaced+=1;let _=sender.send(Event::Replaced(group.clip.source.clone(),outputs));}
                else{for ((row,index,_),path)in group.members.iter().zip(paths){let _=sender.send(Event::Exported(*row,*index,path));}}
            }
            Ok(if options.replace_original{format!("Saved {completed} clips and replaced {replaced} original recordings.")}else{format!("Exported {completed} clips. Original recordings kept.")})
        });
    }

    fn export_options(&mut self, ctx: &egui::Context) {
        let Some(mut request) = self.export_dialog.take() else {
            return;
        };
        let mut confirm = false;
        let mut cancel = false;
        let modal=egui::Modal::new(egui::Id::new("export_options")).show(ctx,|ui|{
            ui.set_width(500.0);
            let sources:HashSet<_>=request.jobs.iter().map(|job|&job.2.source).collect();
            ui.heading("Export clips");ui.label(format!("{} selected clip(s) from {} recording(s)",request.jobs.len(),sources.len()));
            ui.separator();
            ui.radio_value(&mut request.options.replace_original,false,"Keep originals and save clips to a folder");
            ui.radio_value(&mut request.options.replace_original,true,"Replace originals with the selected clips");
            if request.options.replace_original {
                ui.colored_label(Color32::from_rgb(240,202,128),"Unselected footage will be removed.");
                ui.label("Clips are saved beside each original. Each original is deleted only after all of its selected clips are exported and verified.");
            }else{
                ui.horizontal_wrapped(|ui|{ui.label("Save to");ui.add(egui::Label::new(request.folder.display().to_string()).wrap());if ui.button("Choose folder...").clicked()&&let Some(folder)=rfd::FileDialog::new().pick_folder(){request.folder=folder;}});
            }
            ui.add_space(8.0);
            ui.checkbox(&mut request.options.preserve_times,"Preserve recording date and file modified time");
            if request.options.preserve_times&&cfg!(target_os="linux"){ui.small("Linux assigns a new filesystem creation time. The original modified time and recording-date metadata are preserved.");}
            egui::CollapsingHeader::new("Selected ranges").default_open(true).show(ui,|ui|{
                egui::ScrollArea::vertical().max_height(170.0).show(ui,|ui|{for(_,_,clip,moment)in &request.jobs {ui.strong(clip.source.file_name().unwrap_or_default().to_string_lossy());ui.label(format!("{} - {}   {}",segments::timestamp(moment.start_seconds),segments::timestamp(moment.end_seconds),moment.description));}});
            });
            ui.separator();ui.horizontal(|ui|{
                if ui.button("Cancel").clicked(){cancel=true;}
                let label=if request.options.replace_original{"Export clips and remove originals"}else{"Export clips"};
                if ui.add(egui::Button::new(label).fill(if request.options.replace_original{Color32::from_rgb(119,55,51)}else{Color32::from_rgb(40,103,81)})).clicked(){confirm=true;}
            });
        });
        if modal.should_close() {
            cancel = true;
        }
        if confirm {
            self.export_jobs(request.jobs, request.folder, request.options, ctx);
        } else if !cancel {
            self.export_dialog = Some(request);
        }
    }

    fn remembering_status(&mut self, ui: &mut egui::Ui) {
        if let Some(error) = &self.remember_error {
            ui.colored_label(Color32::LIGHT_RED, error);
        }
        if let Some(remember) = &self.remember {
            if !remember.loaded {
                ui.small("Restoring remembered API keys...");
            } else {
                let pending = remember.last_sent.as_ref()
                    != Some(&self.remembered_snapshot(remember.key_ready))
                    || remember.completed_revision < remember.revision;
                ui.small(if pending {
                    "Saving settings..."
                } else {
                    "Settings saved automatically."
                });
                let index = self.provider.index();
                if let Some(error) = &remember.key_errors[index] {
                    ui.colored_label(Color32::LIGHT_RED, error);
                } else if !self.providers[index].key.is_empty() && !pending {
                    ui.small("API key remembered.");
                }
                if let Some(error) = &remember.settings_error {
                    ui.colored_label(Color32::LIGHT_RED, error);
                }
            }
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.remember.is_some(),
                    egui::Button::new("Save settings now"),
                )
                .clicked()
            {
                self.save_preferences_now();
            }
            if ui.button("Retry storage").clicked() {
                if let Some(remember) = &mut self.remember {
                    remember.loaded = false;
                    remember.last_sent = None;
                    remember.worker.reload();
                } else {
                    self.enable_remembering();
                }
            }
        });
    }

    fn choose_folder(&mut self, ctx: &egui::Context) {
        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
            self.folder = folder.display().to_string();
            self.request(Pending::Scan, ctx);
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.heading("Settings");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    self.settings_open = false;
                }
            });
        });
        ui.horizontal(|ui| {
            for (page, label) in [
                (SettingsPage::Analysis, "Analysis"),
                (SettingsPage::Folders, "Folders"),
                (SettingsPage::Storage, "Storage"),
            ] {
                ui.selectable_value(&mut self.settings_page, page, label);
            }
        });
        ui.separator();
        match self.settings_page {
            SettingsPage::Analysis => {
                ui.label(RichText::new("Highlight detection").strong());
                ui.small("Choose how recordings are analyzed. Folder-specific settings override these defaults.");
                ui.add_enabled_ui(self.work.is_none(), |ui| {
                    ui.add_space(8.0);
                    ui.label("Provider");
                    let mut provider = self.provider;
                    egui::ComboBox::from_id_salt("provider")
                        .selected_text(provider.label())
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            for option in Provider::ALL {
                                ui.selectable_value(&mut provider, option, option.label());
                            }
                        });
                    if provider != self.provider {
                        self.select_provider(provider);
                        self.show_key = false;
                    }
                    ui.label(format!("{} API key", self.provider.label()));
                    let changed = ui
                        .add(
                            egui::TextEdit::singleline(
                                &mut self.providers[self.provider.index()].key,
                            )
                            .id(egui::Id::new((
                                "provider_api_key_input",
                                self.provider.index(),
                            )))
                            .password(!self.show_key)
                            .hint_text("Paste API key")
                            .desired_width(f32::INFINITY),
                        )
                        .changed();
                    if changed && let Some(remember) = &mut self.remember {
                        let index = self.provider.index();
                        remember.key_ready[index] = true;
                        remember.key_edited[index] = true;
                    }
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.show_key, "Show key");
                        ui.small("Stored in your desktop keyring");
                    });
                    if self.provider == Provider::Gemini {
                        ui.horizontal_wrapped(|ui| {
                            ui.hyperlink_to(
                                "Get a Gemini key",
                                "https://aistudio.google.com/api-keys",
                            );
                            if ui.small_button("Use free-tier preset").clicked() {
                                self.use_gemini_free_tier();
                            }
                        });
                    }
                    ui.add_space(8.0);
                    ui.label("Game context");
                    if ui
                        .add(
                            egui::TextEdit::multiline(&mut self.hint)
                                .id(egui::Id::new("game_context_input"))
                                .hint_text("e.g. Helldivers 2 with friends")
                                .desired_rows(3)
                                .desired_width(f32::INFINITY),
                        )
                        .changed()
                        && let Some(path) = self.context_folder.clone()
                    {
                        self.set_folder_context(path, self.hint.clone());
                    }
                    if let Some(path) = &self.context_folder {
                        ui.small(format!(
                            "{}: {}",
                            if self.context_inferred {
                                "From folder name"
                            } else {
                                "Saved for folder"
                            },
                            path.display()
                        ));
                    } else {
                        ui.small("Open a folder to save its game context.");
                    }
                    egui::CollapsingHeader::new("Models and limits")
                        .id_salt("analysis_settings")
                        .show(ui, |ui| {
                            ui.label("Visual model");
                            ui.add(
                                egui::TextEdit::singleline(
                                    &mut self.providers[self.provider.index()].models.model,
                                )
                                .id(egui::Id::new((
                                    "provider_model_input",
                                    self.provider.index(),
                                )))
                                .desired_width(f32::INFINITY),
                            );
                            if self.provider == Provider::OpenAi {
                                ui.label("Audio model");
                                ui.add(
                                    egui::TextEdit::singleline(
                                        &mut self.providers[self.provider.index()]
                                            .models
                                            .audio_model,
                                    )
                                    .id(egui::Id::new((
                                        "provider_audio_model_input",
                                        self.provider.index(),
                                    )))
                                    .desired_width(f32::INFINITY),
                                );
                            }
                            ui.horizontal_wrapped(|ui| {
                                ui.label("Maximum clip length");
                                ui.add(
                                    egui::DragValue::new(&mut self.max_seconds)
                                        .range(1..=3600)
                                        .suffix(" s"),
                                );
                            });
                            ui.small("Long recordings can exceed the upload size limit.");
                        });
                });
                ui.add_space(12.0);
                if let Some(remember) = &self.remember {
                    let pending = remember.last_sent.as_ref()
                        != Some(&self.remembered_snapshot(remember.key_ready))
                        || remember.completed_revision < remember.revision;
                    ui.colored_label(
                        MUTED,
                        if !remember.loaded {
                            "Restoring saved key..."
                        } else if pending {
                            "Saving settings..."
                        } else {
                            "Settings saved"
                        },
                    );
                    if let Some(error) = &remember.key_errors[self.provider.index()] {
                        ui.colored_label(Color32::LIGHT_RED, error);
                    }
                    if let Some(error) = &remember.settings_error {
                        ui.colored_label(Color32::LIGHT_RED, error);
                    }
                }
                egui::CollapsingHeader::new("API usage and billing").show(ui,|ui| {
                    ui.small(match self.provider { Provider::Gemini=>"Scanning sends compressed video and audio to Google Gemini. Free Tier keys have quotas; paid-project keys can incur charges.",Provider::OpenAi=>"Scanning sends frames and audio to OpenAI. API credits are billed separately from ChatGPT subscriptions." });
                    if self.provider==Provider::Gemini { ui.hyperlink_to("Check project billing","https://aistudio.google.com/projects"); }
                    ui.small("Playback and exports are local. Analyze on open and watched folders can start API requests automatically.");
                });
            }
            SettingsPage::Folders => {
                self.watch_settings(ui, ctx);
            }
            SettingsPage::Storage => {
                if let Some(error) = &self.cache_error {
                    ui.colored_label(Color32::LIGHT_RED, error);
                }
                ui.strong("Settings and credentials");
                ui.small("Settings save automatically. API keys use your operating system's credential store.");
                self.remembering_status(ui);
                if let Some(remember) = &self.remember {
                    ui.label("Settings file");
                    ui.add(
                        egui::Label::new(
                            RichText::new(remember.path.display().to_string())
                                .small()
                                .color(MUTED),
                        )
                        .wrap(),
                    );
                    if ui.button("Copy settings path").clicked() {
                        ctx.copy_text(remember.path.display().to_string());
                    }
                }
                ui.separator();
                ui.strong("Saved reviews");
                ui.small("AI scans are cached automatically. Save a review to keep your manual trims, labels, and keeper choices.");
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Open review...").clicked() {
                        self.request(Pending::Open, ctx);
                    }
                    if ui
                        .add_enabled(
                            !self.rows.is_empty(),
                            egui::Button::new("Save review as..."),
                        )
                        .clicked()
                    {
                        self.save();
                    }
                });
                ui.separator();
                ui.label(format!("Remove saved {} key", self.provider.label()));
                if ui
                    .add_enabled(
                        self.remember.is_some(),
                        egui::Button::new("Forget this provider's key"),
                    )
                    .clicked()
                {
                    self.forget_key();
                }
                ui.small("This removes the key for the provider selected on the Analysis tab.");
            }
        }
    }

    fn library(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading("Library");
            ui.label(RichText::new(self.rows.len().to_string()).color(MUTED));
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.work.is_none(), egui::Button::new("Open folder..."))
                .clicked()
            {
                self.choose_folder(ctx);
            }
            if ui
                .add_enabled(
                    self.work.is_none() && !self.folder.is_empty(),
                    egui::Button::new("Refresh"),
                )
                .on_hover_text("Find recordings added to this folder")
                .clicked()
            {
                self.request(Pending::Scan, ctx);
            }
        });
        if !self.folder.is_empty() {
            ui.add(
                egui::Label::new(
                    RichText::new(
                        std::path::Path::new(&self.folder)
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy(),
                    )
                    .small()
                    .color(MUTED),
                )
                .truncate(),
            )
            .on_hover_text(&self.folder);
        }
        ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .id(egui::Id::new("moment_filter_input"))
                .hint_text("Search recordings or highlights")
                .desired_width(f32::INFINITY),
        );
        ui.horizontal_wrapped(|ui| {
            for (index, label) in ["All", "Highlights", "Needs attention"].iter().enumerate() {
                ui.selectable_value(&mut self.library_view, index, *label);
            }
        });
        ui.add_space(4.0);
        let filter = self.filter.to_lowercase();
        let mut open = None;
        let mut count = 0;
        egui::ScrollArea::vertical()
            .id_salt("recording_library")
            .show(ui, |ui| {
                for (index, row) in self.rows.iter().enumerate() {
                    let highlights = row
                        .clip
                        .as_ref()
                        .map_or(0, |clip| clip.analysis.moments.len());
                    if !matches_filter(row, &filter)
                        || (self.library_view == 1 && highlights == 0)
                        || (self.library_view == 2
                            && row.error.is_none()
                            && !row.working
                            && !row.queued)
                    {
                        continue;
                    }
                    count += 1;
                    ui.push_id((&row.source, "library_card"), |ui| {
                        let active = self.active_source.as_ref() == Some(&row.source);
                        let title = row
                            .clip
                            .as_ref()
                            .and_then(|clip| clip.analysis.moments.first())
                            .map(|moment| moment.description.clone())
                            .unwrap_or_else(|| {
                                row.source
                                    .file_stem()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .replace('_', " ")
                            });
                        let status = if row.working {
                            "Analyzing".to_owned()
                        } else if row.queued || self.auto_queue.contains(&row.source) {
                            "Queued".into()
                        } else if row.error.is_some() {
                            "Scan failed".into()
                        } else if highlights > 0 {
                            format!(
                                "{highlights} highlight{}",
                                if highlights == 1 { "" } else { "s" }
                            )
                        } else if row.clip.is_some() && !row.manual_only {
                            "No highlight found".into()
                        } else {
                            "Not scanned".into()
                        };
                        let duration = row
                            .clip
                            .as_ref()
                            .map(|clip| segments::timestamp(clip.media.duration_seconds));
                        let mut job = egui::text::LayoutJob::default();
                        job.append(
                            &title,
                            0.0,
                            egui::TextFormat {
                                font_id: egui::FontId::proportional(15.0),
                                color: if active {
                                    Color32::WHITE
                                } else {
                                    Color32::from_rgb(222, 231, 241)
                                },
                                ..Default::default()
                            },
                        );
                        job.append(
                            &format!(
                                "\n{}",
                                row.source.file_name().unwrap_or_default().to_string_lossy()
                            ),
                            0.0,
                            egui::TextFormat {
                                font_id: egui::FontId::proportional(11.5),
                                color: MUTED,
                                ..Default::default()
                            },
                        );
                        job.append(
                            &format!(
                                "\n{}{}",
                                duration.map_or(String::new(), |value| format!("{value}   /   ")),
                                status
                            ),
                            0.0,
                            egui::TextFormat {
                                font_id: egui::FontId::proportional(12.0),
                                color: if row.error.is_some() {
                                    Color32::LIGHT_RED
                                } else if row.working || row.queued {
                                    Color32::from_rgb(240, 202, 128)
                                } else {
                                    ACCENT
                                },
                                ..Default::default()
                            },
                        );
                        let button = egui::Button::new(job)
                            .wrap()
                            .min_size(egui::vec2(ui.available_width(), 82.0))
                            .fill(if active {
                                Color32::from_rgb(29, 64, 58)
                            } else {
                                Color32::from_rgb(24, 33, 45)
                            })
                            .stroke(egui::Stroke::new(
                                1.0_f32,
                                if active {
                                    ACCENT
                                } else {
                                    Color32::from_rgb(35, 47, 63)
                                },
                            ))
                            .corner_radius(7.0);
                        if ui
                            .add(button)
                            .on_hover_text(row.source.display().to_string())
                            .clicked()
                        {
                            open = Some(index);
                        }
                        ui.add_space(3.0);
                    });
                }
                if count == 0 {
                    ui.add_space(20.0);
                    ui.label(if self.rows.is_empty() {
                        "Your recordings will appear here."
                    } else {
                        "No recordings match this filter."
                    });
                    if !self.rows.is_empty() && ui.button("Clear filters").clicked() {
                        self.filter.clear();
                        self.library_view = 0;
                    }
                }
            });
        if let Some(index) = open {
            self.open_recording(index, ctx);
        }
    }

    fn content(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let active = self
            .active_source
            .as_ref()
            .and_then(|source| self.rows.iter().position(|row| &row.source == source));
        let Some(index) = active else {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height()*0.20).max(24.0));
                ui.label(RichText::new(if self.rows.is_empty(){"Your best moments, without the rewatch."}else {"Choose a recording to start watching"}).size(27.0).strong());
                ui.add_space(10.0);
                ui.label(if self.rows.is_empty(){"Open a folder of game recordings. Find highlights, adjust the cut, and keep the good parts."}else {"Video, highlight ranges, and export controls stay together in this workspace."});
                ui.add_space(18.0);
                if self.rows.is_empty() && ui.add(egui::Button::new(RichText::new("Open recordings folder...").color(Color32::from_rgb(15,36,29))).fill(ACCENT).min_size(egui::vec2(220.0,42.0))).clicked(){self.choose_folder(ctx);}
                if self.settings().key.is_empty() && ui.button("Set up highlight analysis").clicked(){self.settings_open=true;self.settings_page=SettingsPage::Analysis;}
                ui.add_space(28.0);ui.small("1  Open a recording     2  Review the highlights     3  Export your selection");
            });
            return;
        };
        let provider = self.scan_config(&self.rows[index].source).provider;
        let key_missing = self.providers[provider.index()].key.trim().is_empty();
        let row = &mut self.rows[index];
        let mut rescan = false;
        if !self.player_expanded {
            ui.add(
                egui::Label::new(
                    RichText::new(row.source.file_name().unwrap_or_default().to_string_lossy())
                        .heading(),
                )
                .truncate(),
            )
            .on_hover_text(row.source.display().to_string());
            ui.horizontal_wrapped(|ui| {
                if row.working {
                    ui.spinner();
                    ui.colored_label(ACCENT, "Finding highlights...");
                } else if row.queued || self.auto_queue.contains(&row.source) {
                    ui.label("Scan queued. Keep watching.");
                } else if self.auto_find && row.manual_only && key_missing {
                    ui.small("Add an API key in Settings to search automatically.");
                } else {
                    ui.small("Full recording / editable highlights");
                }
                if !row.history.is_empty() {
                    egui::ComboBox::from_id_salt("active_history")
                        .selected_text("Previous scans")
                        .show_ui(ui, |ui| {
                            let mut restore = None;
                            for (i, clip) in row.history.iter().enumerate() {
                                if ui
                                    .selectable_label(
                                        false,
                                        format!(
                                            "Scan {} / {} highlights",
                                            i + 1,
                                            clip.analysis.moments.len()
                                        ),
                                    )
                                    .clicked()
                                {
                                    restore = Some(i);
                                }
                            }
                            if let Some(i) = restore
                                && self.work.is_none()
                            {
                                row.restore(i);
                                self.dirty = true;
                            }
                        });
                }
            });
            if let Some(error) = &row.error {
                egui::Frame::new()
                    .fill(Color32::from_rgb(57, 33, 39))
                    .corner_radius(6.0)
                    .inner_margin(7.0)
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.colored_label(Color32::LIGHT_RED, "Needs attention");
                            ui.menu_button("Details", |ui| {
                                ui.set_max_width(360.0);
                                ui.add(egui::Label::new(error).wrap());
                            });
                            if ui.small_button("Scan settings").clicked() {
                                self.settings_open = true;
                                self.settings_page = SettingsPage::Analysis;
                            }
                        });
                    });
            }
        }
        let mut export = None;
        let mut reload = false;
        if let Some(player) = &mut self.player
            && player.source() == row.source
        {
            if let Some(clip) = &mut row.clip {
                let editor_height = ui.available_height();
                player.set_viewing_mode(self.player_expanded);
                let result = player.show(
                    ui,
                    clip,
                    self.work.is_none() || self.analysis_running,
                    self.work.is_none() && !clip.sha256.is_empty(),
                    editor_height,
                    self.work.is_none() && !row.working && !row.queued,
                );
                if result.toggle_expand {
                    self.player_expanded = !self.player_expanded;
                }
                self.dirty |= result.changed;
                row.edited_during_scan |= result.changed && row.working;
                export = result.export;
                rescan = result.rescan;
                reload = result.reload;
                if !self.player_expanded && !clip.analysis.uncertainty.is_empty() {
                    ui.small(&clip.analysis.uncertainty);
                }
            }
        } else if self.opening.is_some() {
            ui.spinner();
            ui.label("Opening source video...");
        } else if self.reopen_after_export && self.work.is_some() {
            ui.spinner();
            ui.label("Saving replacement clips. Playback will reopen when export finishes.");
        } else {
            ui.label("Video could not be opened. Click its name to try again.");
        }
        if reload {
            self.player = None;
            self.open_recording(index, ctx);
        }
        if rescan {
            self.request_rescan(index, ctx);
        }
        if let Some(moment) = export
            && self.work.is_none()
        {
            self.prepare_export(self.selected_jobs(index, moment));
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("HDContext").size(23.0).strong().color(ACCENT));
            ui.add_space(10.0);
            ui.menu_button("File",|ui| {
                ui.add_enabled_ui(self.work.is_none(),|ui| {
                    if ui.button("Open recordings folder...").clicked(){ui.close();self.choose_folder(ctx);}
                    if ui.button("Open review...").clicked(){ui.close();self.request(Pending::Open,ctx);}
                    if ui.add_enabled(!self.rows.is_empty(),egui::Button::new("Save review as...")).clicked(){ui.close();self.save();}
                });
            });
            if ui.checkbox(&mut self.auto_find,"Analyze on open").on_hover_text("Automatically send unscanned recordings to your selected provider when opened. Watched folders use their own scan settings.").changed(){self.auto_limit_blocked=false;if self.auto_find && let Some(cache)=&self.cache{let _=cache.set_paused(self.provider,false);}}
            if ui.selectable_label(self.settings_open,"Settings").clicked(){self.settings_open = !self.settings_open;}
            if self.dirty && ui.add_enabled(self.work.is_none(),egui::Button::new("Save edits...")).on_hover_text("Save manual edits and keeper choices").clicked(){self.save();}
            let keepers=self.rows.iter().filter_map(|row|row.clip.as_ref()).flat_map(|clip|&clip.analysis.moments).filter(|moment|moment.kept&&moment.exported_to.is_none()).count();
            if keepers>0 && ui.add_enabled(self.work.is_none(),egui::Button::new(format!("Export keepers ({keepers})"))).clicked(){self.prepare_export(self.kept_jobs());}
            if let Some(work)=&self.work {ui.spinner();ui.small(format!("Processing {} / {}",work.completed,work.total));if work.cancellable && ui.small_button("Stop").on_hover_text("Stop after the current request or export finishes").clicked(){work.cancel.store(true,Ordering::Relaxed);}}
        });
        if self.error.is_some()
            || self.auto_limit_blocked
            || self.service_problem.is_some()
            || self.cache_error.is_some()
        {
            let title = if self.service_problem.is_some() {
                "Background watcher needs attention"
            } else if self.cache_error.is_some() {
                "Scan storage needs attention"
            } else if self.auto_limit_blocked {
                "Automatic scans paused"
            } else {
                "The last action could not finish"
            };
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(Color32::LIGHT_RED, title);
                if ui.small_button("Review settings").clicked() {
                    self.settings_open = true;
                    self.settings_page = if self.cache_error.is_some() {
                        SettingsPage::Storage
                    } else if self.service_problem.is_some() || self.auto_limit_blocked {
                        SettingsPage::Folders
                    } else {
                        SettingsPage::Analysis
                    };
                }
            });
            if let Some(error) = self
                .error
                .as_ref()
                .or(self.cache_error.as_ref())
                .or(self.service_problem.as_ref().map(|(_, message)| message))
            {
                egui::CollapsingHeader::new("Error details").show(ui, |ui| {
                    ui.add(egui::Label::new(error).wrap());
                });
            }
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let highlights: usize = self
                .rows
                .iter()
                .filter_map(|row| row.clip.as_ref())
                .map(|clip| clip.analysis.moments.len())
                .sum();
            ui.small(format!(
                "{} recordings   /   {highlights} highlights",
                self.rows.len()
            ));
            let scanning = self.rows.iter().filter(|row| row.working).count();
            let queued = self.rows.iter().filter(|row| row.queued).count();
            if scanning > 0 || queued > 0 {
                ui.colored_label(ACCENT, format!("{scanning} scanning / {queued} queued"));
            }
            let folders = self
                .watched_folders
                .iter()
                .filter(|folder| folder.enabled)
                .count();
            if folders > 0 {
                ui.separator();
                ui.colored_label(
                    MUTED,
                    format!(
                        "{folders} watched folder{}",
                        if folders == 1 { "" } else { "s" }
                    ),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.colored_label(
                    if self.dirty {
                        Color32::from_rgb(240, 202, 128)
                    } else {
                        MUTED
                    },
                    if self.dirty {
                        "Unsaved review edits"
                    } else {
                        "No unsaved edits"
                    },
                );
            });
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(action) = self.pending {
            egui::Modal::new(egui::Id::new("unsaved_confirmation")).show(ctx, |ui| {
                ui.set_width(420.0);
                ui.heading("Save your review first?");
                ui.label("Your moment ranges and keeper choices have not been saved. Save a review to return to them later.");
                ui.horizontal(|ui| {
                    if ui.button("Keep reviewing").clicked() { self.pending = None; }
                    if ui.button("Save review as...").clicked() {
                        self.save();
                        if !self.dirty { self.pending = None; self.perform(action, ctx); }
                    }
                    if ui.button("Continue without saving").clicked() { self.pending = None; self.perform(action, ctx); }
                });
            });
        }
    }
}

fn matches_filter(row: &Row, filter: &str) -> bool {
    filter.is_empty()
        || row.source.to_string_lossy().to_lowercase().contains(filter)
        || row.clip.as_ref().is_some_and(|c| {
            c.analysis
                .moments
                .iter()
                .any(|m| m.description.to_lowercase().contains(filter))
        })
}

impl Desktop {
    fn render(&mut self, ctx: &egui::Context) {
        self.poll();
        self.sync_context_editor();
        if let Some(source) = self.replacement_to_open.take()
            && let Some(index) = self.rows.iter().position(|row| row.source == source)
        {
            self.open_recording(index, ctx);
        }
        if ctx.input(|input| input.viewport().close_requested()) && !self.allow_close {
            if self.work.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.error = Some(if self.work.as_ref().is_some_and(|work| work.cancellable) {
                    "Analysis is still running. Use Stop after current clip, then close the window when it finishes."
                } else {
                    "Please wait for the current task to finish before closing the window."
                }.into());
            } else if self.dirty {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.pending = Some(Pending::Close);
            }
        }
        if self.work.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.poll_opening();
        self.poll_watches(ctx);
        self.poll_auto(ctx);
        if self.opening.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        if self.pending.is_none()
            && self.export_dialog.is_none()
            && let Some(player) = &mut self.player
        {
            player.handle_shortcuts(ctx);
        }
        if self.player_expanded
            && self.pending.is_none()
            && self.export_dialog.is_none()
            && !egui::Popup::is_any_open(ctx)
            && !ctx.wants_keyboard_input()
            && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.player_expanded = false;
        }
        if self.player.is_none() {
            self.player_expanded = false;
        }
        if !self.player_expanded {
            egui::TopBottomPanel::top("workspace_toolbar").show(ctx, |ui| self.toolbar(ui, ctx));
            egui::TopBottomPanel::bottom("workspace_status").show(ctx, |ui| self.status_bar(ui));
            if self.settings_open {
                egui::SidePanel::right("settings")
                    .resizable(true)
                    .default_width(300.0)
                    .width_range(280.0..=420.0)
                    .show(ctx, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("settings_scroll")
                            .show(ui, |ui| self.sidebar(ui, ctx));
                    });
            }
            egui::SidePanel::left("library")
                .resizable(true)
                .default_width(290.0)
                .width_range(
                    195.0..=if self.settings_open && ctx.content_rect().width() < 1250.0 {
                        225.0
                    } else {
                        400.0
                    },
                )
                .show(ctx, |ui| self.library(ui, ctx));
        }
        egui::CentralPanel::default().show(ctx, |ui| self.content(ui, ctx));
        self.dialogs(ctx);
        self.export_options(ctx);
        self.poll_remembering(ctx);
    }
}

impl eframe::App for Desktop {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.render(ctx);
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(cancel) = &self.local_watch_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(remember) = &self.remember {
            let snapshot = self.remembered_snapshot(remember.key_ready);
            // Preferences can still be saved if credential lookup is blocked by a locked keyring.
            if preferences::save(&snapshot.preferences, &remember.path).is_err() {
                eprintln!("Could not save HDContext app settings on exit");
            }
            remember.worker.flush(snapshot);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ai::Analysis,
        media::{self, MediaInfo},
    };

    fn clip(source: PathBuf, name: &str) -> Clip {
        Clip {
            provider: Provider::Gemini,
            model: "test".into(),
            audio_model: None,
            sha256: media::fingerprint(&source).unwrap(),
            source,
            proposed_name: name.into(),
            media: MediaInfo {
                duration_seconds: 1.0,
                has_audio: true,
                audio_tracks: 1,
            },
            analysis: Analysis {
                title: name.into(),
                game: "Portal".into(),
                summary: "A jump misses the platform.".into(),
                audio_summary: "A player laughs.".into(),
                confidence: "medium".into(),
                highlights: vec![],
                moments: vec![crate::ai::Moment {
                    start_seconds: 0.1,
                    end_seconds: 0.8,
                    description: "Shotgun whiff".into(),
                    kept: false,
                    exported_to: None,
                }],
                uncertainty: String::new(),
            },
        }
    }

    fn sample() -> (tempfile::TempDir, Desktop) {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("replay.mp4");
        std::fs::write(&source, b"video contents").unwrap();
        let mut row = Row::new(source.clone());
        row.generated(Ok(clip(source, "Portal - Missed jump.mp4")));
        let app = Desktop {
            rows: vec![row],
            ..Default::default()
        };
        (directory, app)
    }

    #[test]
    fn folder_contexts_restore_per_folder_and_scans_use_the_source_folder() {
        let temp = tempfile::tempdir().unwrap();
        let hunt = temp.path().join("Hunt_Showdown_1896");
        let divers = temp.path().join("Helldivers2_Clips");
        std::fs::create_dir_all(&hunt).unwrap();
        std::fs::create_dir_all(&divers).unwrap();
        let mut app = Desktop {
            folder: hunt.display().to_string(),
            ..Desktop::default()
        };
        app.sync_context_editor();
        assert_eq!(app.hint, "Hunt Showdown 1896");
        assert!(app.context_inferred);
        app.set_folder_context(hunt.clone(), "Hunt, duo shotgun fights".into());
        app.folder = divers.display().to_string();
        app.sync_context_editor();
        assert_eq!(app.hint, "Helldivers 2");
        assert_eq!(
            app.scan_config(&hunt.join("one.mp4")).hint,
            "Hunt, duo shotgun fights"
        );
        app.set_folder_context(divers.clone(), String::new());
        let path = temp.path().join("settings.json");
        preferences::save(&app.remembered_preferences(), &path).unwrap();
        let mut restored = Desktop::default();
        restored.apply_remembered_preferences(preferences::load(&path).unwrap().unwrap());
        assert!(restored.hint.is_empty());
        assert!(!restored.context_inferred);
        restored.folder = hunt.display().to_string();
        restored.sync_context_editor();
        assert_eq!(restored.hint, "Hunt, duo shotgun fights");
        restored.active_source = Some(divers.join("two.mp4"));
        restored.sync_context_editor();
        assert!(restored.hint.is_empty());
    }

    #[test]
    fn player_context_tracks_game_subfolders_in_a_shared_watched_library() {
        let profile = watch::Folder {
            path: PathBuf::from("/clips"),
            enabled: true,
            recursive: true,
            provider: Provider::Gemini,
            model: "test".into(),
            audio_model: String::new(),
            hint: String::new(),
            max_seconds: 300,
        };
        let mut app = Desktop {
            folder: "/clips".into(),
            watched_folders: vec![profile],
            active_source: Some(PathBuf::from("/clips/Helldivers2/a.mp4")),
            ..Desktop::default()
        };
        app.sync_context_editor();
        assert_eq!(app.hint, "Helldivers 2");
        let scope = app.context_folder.clone().unwrap();
        app.set_folder_context(scope, "Helldivers with friends".into());
        app.active_source = Some(PathBuf::from("/clips/Hunt/b.mp4"));
        app.sync_context_editor();
        assert_eq!(app.hint, "Hunt");
        assert_eq!(
            app.scan_config(std::path::Path::new("/clips/Helldivers2/a.mp4"))
                .hint,
            "Helldivers with friends"
        );
    }

    #[test]
    fn watched_and_manual_context_editors_share_saved_values() {
        let folder = watch::Folder {
            path: PathBuf::from("/clips/Hunt"),
            enabled: true,
            recursive: true,
            provider: Provider::Gemini,
            model: "test".into(),
            audio_model: String::new(),
            hint: "Old context".into(),
            max_seconds: 300,
        };
        let mut app = Desktop {
            folder: folder.path.display().to_string(),
            watched_folders: vec![folder.clone()],
            ..Desktop::default()
        };
        app.sync_context_editor();
        assert_eq!(app.hint, "Old context");
        app.set_folder_context(folder.path.clone(), "Updated context".into());
        assert_eq!(app.watched_folders[0].hint, "Updated context");
        let prefs = app.remembered_preferences();
        let (background_hint, _) = crate::context::resolve(
            std::path::Path::new("/clips/Hunt/day"),
            &prefs.effective_contexts(),
            &prefs.watched_folders,
        );
        assert_eq!(
            background_hint,
            app.scan_config(std::path::Path::new("/clips/Hunt/day/a.mp4"))
                .hint
        );
        app.set_folder_context(folder.path, String::new());
        assert!(app.hint.is_empty());
        assert!(app.watched_folders[0].hint.is_empty());
    }

    #[test]
    fn remembered_settings_restore_folder_provider_models_and_context_without_affecting_keys() {
        let (directory, mut app) = sample();
        app.folder = "/games/My clips".into();
        app.provider = Provider::OpenAi;
        app.recursive = false;
        app.sync_context_editor();
        app.set_folder_context(PathBuf::from("/games/My clips"), "Shotgun fails".into());
        app.max_seconds = 640;
        app.auto_find = false;
        app.providers[0].models.model = "saved-gemini-model".into();
        app.providers[1].models.audio_model = "saved-audio-model".into();
        app.providers[0].key = "not-part-of-settings".into();
        let path = directory.path().join("settings.json");
        preferences::save(&app.remembered_preferences(), &path).unwrap();
        let mut restored = Desktop::default();
        restored.providers[0].key = "current-session-key".into();
        restored.apply_remembered_preferences(preferences::load(&path).unwrap().unwrap());
        assert_eq!(restored.folder, "/games/My clips");
        assert_eq!(restored.provider, Provider::OpenAi);
        assert!(!restored.recursive);
        assert_eq!(restored.max_seconds, 640);
        assert!(!restored.auto_find);
        assert_eq!(restored.hint, "Shotgun fails");
        assert_eq!(restored.providers[0].models.model, "saved-gemini-model");
        assert_eq!(
            restored.providers[1].models.audio_model,
            "saved-audio-model"
        );
        assert_eq!(restored.providers[0].key, "current-session-key");
        assert!(
            !std::fs::read_to_string(path)
                .unwrap()
                .contains("not-part-of-settings")
        );
    }

    #[test]
    fn delayed_key_lookup_does_not_replace_a_typed_or_forgotten_key() {
        let (directory, mut app) = sample();
        let (worker, events) = preferences::Worker::test_events();
        let mut remember = Remember::new(
            directory.path().join("settings.json"),
            worker,
            [true, false],
        );
        remember.key_edited[0] = true;
        app.providers[0].key = "newly-typed-key".into();
        app.providers[1].key.clear();
        app.remember = Some(remember);
        events
            .send(preferences::Event::Loaded([
                Ok(Some("old-stored-gemini".into())),
                Ok(Some("stored-openai".into())),
            ]))
            .unwrap();
        app.poll_remembering(&egui::Context::default());
        assert_eq!(app.providers[0].key, "newly-typed-key");
        assert_eq!(app.providers[1].key, "stored-openai");
        app.provider = Provider::OpenAi;
        app.forget_key();
        events
            .send(preferences::Event::Loaded([
                Ok(Some("old-stored-gemini".into())),
                Ok(Some("stored-openai".into())),
            ]))
            .unwrap();
        app.poll_remembering(&egui::Context::default());
        assert!(app.providers[1].key.is_empty());
        assert!(!directory.path().join("settings.json").exists());
    }

    #[test]
    fn context_keeps_focus_and_text_while_autosave_status_changes() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = Desktop {
            folder: directory.path().display().to_string(),
            provider: Provider::Gemini,
            settings_open: true,
            ..Default::default()
        };
        app.sync_context_editor();
        app.providers[0].key = "remembered-test-value".into();
        let (worker, events) = preferences::Worker::test_events();
        let mut remember =
            Remember::new(directory.path().join("settings.json"), worker, [true, true]);
        let snapshot = app.remembered_snapshot([true, true]);
        remember.loaded = true;
        remember.observed = Some(snapshot.clone());
        remember.last_sent = Some(snapshot);
        remember.revision = 1;
        remember.completed_revision = 1;
        app.remember = Some(remember);
        let ctx = egui::Context::default();
        fn frame(
            ctx: &egui::Context,
            app: &mut Desktop,
            events: Vec<egui::Event>,
        ) -> egui::FullOutput {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1180.0, 1400.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| app.render(ctx),
            )
        }
        fn placeholder(shape: &egui::Shape) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Text(text)
                    if text
                        .galley
                        .job
                        .text
                        .contains("e.g. Helldivers 2 with friends") =>
                {
                    Some(text.pos + egui::vec2(10.0, 5.0))
                }
                egui::Shape::Vec(shapes) => shapes.iter().find_map(placeholder),
                _ => None,
            }
        }
        frame(&ctx, &mut app, vec![]);
        let output = frame(&ctx, &mut app, vec![]);
        let position = output
            .shapes
            .iter()
            .find_map(|shape| placeholder(&shape.shape))
            .expect("Context input must be visible");
        for pressed in [true, false] {
            frame(
                &ctx,
                &mut app,
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ],
            );
        }
        let focused = ctx
            .memory(|m| m.focused())
            .expect("Click should focus the context input");
        let mut expected = String::new();
        for (index, ch) in "Helldivers 2 shotgun whiffs".chars().enumerate() {
            frame(&ctx, &mut app, vec![egui::Event::Text(ch.to_string())]);
            expected.push(ch);
            assert_eq!(
                app.hint, expected,
                "Typing must continue across pending/saved status transitions"
            );
            assert_eq!(
                ctx.memory(|m| m.focused()),
                Some(focused),
                "Autosave must not move keyboard focus"
            );
            if index == 5 {
                // Advance only the debounce clock, without sleeping or touching the real keyring.
                app.remember.as_mut().unwrap().changed_at =
                    std::time::Instant::now() - std::time::Duration::from_secs(1);
                frame(&ctx, &mut app, vec![]);
                let revision = app.remember.as_ref().unwrap().revision;
                assert_eq!(
                    revision, 2,
                    "Typing should queue one save after the idle delay"
                );
                events
                    .send(preferences::Event::Saved {
                        revision,
                        settings: Ok(()),
                        keys: [Some(Ok(())), Some(Ok(()))],
                    })
                    .unwrap();
                frame(&ctx, &mut app, vec![]);
                frame(&ctx, &mut app, vec![]);
                assert_eq!(
                    ctx.memory(|m| m.focused()),
                    Some(focused),
                    "Completing a save must preserve focus"
                );
            }
        }
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
        );
        expected.push('\n');
        assert_eq!(
            app.hint, expected,
            "Enter should add a context line, not activate a settings button"
        );
        assert_eq!(
            app.folder_contexts
                .get(&crate::context::folder_key(directory.path())),
            Some(&expected)
        );
        assert_eq!(
            app.remember.as_ref().unwrap().revision,
            2,
            "Keystrokes must not activate Save settings now"
        );
    }

    #[test]
    fn regeneration_preserves_edited_choices_and_requires_fresh_selection() {
        let (_directory, mut app) = sample();
        let row = &mut app.rows[0];
        row.clip.as_mut().unwrap().proposed_name = "My edited name.mp4".into();
        row.selected = true;
        row.generated(Ok(clip(
            row.source.clone(),
            "Portal - Fall and laughter.mp4",
        )));
        assert!(!row.selected);
        assert_eq!(row.history[0].proposed_name, "My edited name.mp4");
        row.restore(0);
        assert_eq!(
            row.clip.as_ref().unwrap().proposed_name,
            "My edited name.mp4"
        );
        row.generated(Err("Quota exhausted".into()));
        assert_eq!(
            row.clip.as_ref().unwrap().proposed_name,
            "My edited name.mp4"
        );
        assert_eq!(row.error.as_deref(), Some("Quota exhausted"));
    }

    #[test]
    fn saved_reviews_preserve_pending_clips_selection_history_and_exclude_api_key() {
        let (directory, mut app) = sample();
        app.providers[0].key = "private-api-key-never-save".into();
        app.providers[1].key = "private-openai-key-never-save".into();
        let row = &mut app.rows[0];
        row.generated(Ok(clip(row.source.clone(), "Portal - Another name.mp4")));
        row.selected = true;
        app.rows
            .push(Row::new(directory.path().join("pending.mkv")));
        let saved = app.snapshot();
        assert!(
            !serde_json::to_string(&saved)
                .unwrap()
                .contains("private-api-key-never-save")
        );
        assert!(
            !serde_json::to_string(&saved)
                .unwrap()
                .contains("private-openai-key-never-save")
        );
        let path = directory.path().join("review.json");
        report::save_json(&saved, &path).unwrap();
        // The CLI can still read successful suggestions in a GUI review file.
        assert_eq!(report::load(&path).unwrap().clips.len(), 1);
        let mut restored = Desktop::default();
        restored.open_path(&path).unwrap();
        assert_eq!(restored.rows.len(), 2);
        assert!(restored.rows[0].selected);
        assert_eq!(restored.rows[0].history.len(), 1);
        assert!(restored.rows[1].clip.is_none());
        assert!(!restored.dirty);
    }

    #[test]
    fn opens_existing_cli_reports() {
        let (directory, app) = sample();
        let path = directory.path().join("cli-report.json");
        let mut legacy = serde_json::to_value(app.snapshot().report).unwrap();
        legacy.as_object_mut().unwrap().remove("provider");
        for clip in legacy["clips"].as_array_mut().unwrap() {
            for field in ["provider", "model", "audio_model"] {
                clip.as_object_mut().unwrap().remove(field);
            }
        }
        report::save_json(&legacy, &path).unwrap();
        let mut restored = Desktop::default();
        restored.open_path(&path).unwrap();
        assert_eq!(
            restored.rows[0].clip.as_ref().unwrap().proposed_name,
            "Portal - Missed jump.mp4"
        );
        assert_eq!(restored.provider, Provider::Gemini);
    }

    #[test]
    fn switching_providers_keeps_keys_and_models_separate_and_saves_only_models() {
        let (directory, mut app) = sample();
        app.providers[0].key = "gemini-secret".into();
        app.providers[0].models.model = "gemini-custom".into();
        app.providers[1].key = "openai-secret".into();
        app.providers[1].models.model = "openai-custom".into();
        app.select_provider(Provider::OpenAi);
        assert_eq!(app.settings().key, "openai-secret");
        assert_eq!(app.settings().models.model, "openai-custom");
        app.select_provider(Provider::Gemini);
        assert_eq!(app.settings().key, "gemini-secret");
        assert_eq!(app.settings().models.model, "gemini-custom");
        app.select_provider(Provider::OpenAi);
        let path = directory.path().join("multi-provider.json");
        report::save_json(&app.snapshot(), &path).unwrap();
        let json = std::fs::read_to_string(&path).unwrap();
        assert!(!json.contains("gemini-secret") && !json.contains("openai-secret"));
        let mut restored = Desktop::default();
        restored.open_path(&path).unwrap();
        assert_eq!(restored.provider, Provider::OpenAi);
        assert_eq!(restored.settings().models.model, "openai-custom");
        restored.select_provider(Provider::Gemini);
        assert_eq!(restored.settings().models.model, "gemini-custom");
        assert_eq!(
            restored.rows[0].clip.as_ref().unwrap().provider,
            Provider::Gemini
        );
    }

    fn finish_work(app: &mut Desktop) {
        // Wait on a channel rather than a polling loop so worker tests have a fixed deadline.
        while let Some(work) = &app.work {
            let event = work
                .receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("Worker must finish");
            // Deliver the received event with any remaining events through the normal UI update path.
            let (sender, receiver) = mpsc::channel();
            sender.send(event).unwrap();
            let old_receiver =
                std::mem::replace(&mut app.work.as_mut().unwrap().receiver, receiver);
            app.poll();
            if let Some(work) = &mut app.work {
                work.receiver = old_receiver;
            }
        }
    }

    fn finish_opening(app: &mut Desktop) {
        while let Some(opening) = &app.opening {
            let event = opening
                .receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("Opening should finish");
            let (sender, receiver) = mpsc::channel();
            sender.send(event).unwrap();
            let old = std::mem::replace(&mut app.opening.as_mut().unwrap().receiver, receiver);
            app.poll_opening();
            if let Some(opening) = &mut app.opening {
                opening.receiver = old;
            }
        }
    }

    fn recording_fixture(source: &std::path::Path) {
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-nostdin",
                "-n",
                "-f",
                "lavfi",
                "-i",
                "color=c=red:s=160x90:r=24:d=1",
                "-c:v",
                "libx264",
            ])
            .arg(source)
            .output()
            .unwrap();
        assert!(output.status.success());
    }

    #[test]
    fn unscanned_recordings_open_the_full_timeline_without_an_api_key() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("unscanned.mp4");
        recording_fixture(&source);
        let mut app = Desktop {
            rows: vec![Row::new(source.clone())],
            ..Default::default()
        };
        app.providers[0].key.clear();
        app.providers[1].key.clear();
        let ctx = egui::Context::default();
        app.open_recording(0, &ctx);
        finish_opening(&mut app);
        assert!(app.error.is_none(), "{:?}", app.error);
        assert_eq!(app.player.as_ref().unwrap().source(), source);
        assert!(app.rows[0].manual_only);
        assert!(
            app.rows[0]
                .clip
                .as_ref()
                .unwrap()
                .analysis
                .moments
                .is_empty()
        );
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 1100.0),
                )),
                ..Default::default()
            },
            |ctx| app.render(ctx),
        );
        assert!(!output.shapes.is_empty());
        assert!(app.player.is_some());
        assert!(
            !app.dirty,
            "Opening a video alone should not require saving a review"
        );
        let saved = directory.path().join("manual-review.json");
        report::save_json(&app.snapshot(), &saved).unwrap();
        let mut restored = Desktop::default();
        restored.open_path(&saved).unwrap();
        assert!(restored.rows[0].manual_only);
    }

    #[test]
    fn background_results_keep_playback_running_and_preserve_concurrent_edits() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("watching.mp4");
        let other = directory.path().join("next.mp4");
        recording_fixture(&source);
        recording_fixture(&other);
        let ctx = egui::Context::default();
        let mut app = Desktop {
            rows: vec![Row::new(source.clone()), Row::new(other.clone())],
            auto_find: true,
            ..Default::default()
        };
        app.providers[0].key.clear();
        app.providers[1].key.clear();
        app.open_recording(0, &ctx);
        finish_opening(&mut app);
        let (sender, receiver) = mpsc::channel();
        app.work = Some(Work {
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
            total: 1,
            completed: 0,
            cancellable: true,
        });
        app.analysis_running = true;
        app.analysis_baselines.insert(source.clone(), vec![]);
        app.rows[0]
            .clip
            .as_mut()
            .unwrap()
            .analysis
            .moments
            .push(crate::ai::Moment {
                start_seconds: 0.2,
                end_seconds: 0.8,
                description: "Manual trim during search".into(),
                kept: true,
                exported_to: None,
            });
        app.open_recording(1, &ctx);
        finish_opening(&mut app);
        assert!(
            app.work.is_some(),
            "Switching video cannot replace the running analysis worker"
        );
        app.poll_auto(&ctx);
        app.poll_auto(&ctx);
        assert_eq!(
            app.auto_queue.iter().filter(|path| **path == other).count(),
            1
        );
        sender
            .send(Event::Generated(
                0,
                Box::new(Ok(clip(source, "Found event"))),
            ))
            .unwrap();
        sender
            .send(Event::Finished(Ok("Highlights ready".into())))
            .unwrap();
        app.poll();
        assert_eq!(app.player.as_ref().unwrap().source(), other);
        let moments = &app.rows[0].clip.as_ref().unwrap().analysis.moments;
        assert_eq!(moments.len(), 2);
        assert_eq!(moments[0].description, "Manual trim during search");
        assert!(moments[0].kept);
        assert_eq!(
            (moments[0].start_seconds, moments[0].end_seconds),
            (0.2, 0.8)
        );

        // A provider limit blocks queued automatic requests, even when a key is available.
        let (sender, receiver) = mpsc::channel();
        app.work = Some(Work {
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
            total: 1,
            completed: 0,
            cancellable: true,
        });
        sender.send(Event::ProviderLimited(app.provider)).unwrap();
        sender
            .send(Event::Finished(Err("Provider limit".into())))
            .unwrap();
        app.poll();
        app.providers[app.provider.index()].key = "test-key-must-not-be-used".into();
        app.poll_auto(&ctx);
        assert!(app.auto_limit_blocked && app.auto_queue.is_empty() && app.work.is_none());
        assert!(app.player.is_some());
    }

    #[test]
    fn watched_results_restore_and_player_rescans_are_queued_without_a_gui_worker() {
        let (directory, initial) = sample();
        let original = initial.rows[0].clip.as_ref().unwrap().clone();
        let cache = Cache::new(directory.path().join("cache")).unwrap();
        let claim = cache.claim(&original.source, false).unwrap().unwrap();
        claim.finish(&Ok(original.clone())).unwrap();
        drop(claim);
        let folder = watch::Folder {
            path: directory.path().into(),
            enabled: true,
            recursive: true,
            provider: Provider::OpenAi,
            model: "folder-model".into(),
            audio_model: "folder-audio".into(),
            hint: "Folder game context".into(),
            max_seconds: 90,
        };
        let mut app = Desktop {
            watched_folders: vec![folder.clone()],
            watch_in_background: true,
            cache: Some(cache.clone()),
            ..Default::default()
        };
        app.import_cached(
            cache
                .records()
                .unwrap()
                .into_iter()
                .map(|record| (record, false))
                .collect(),
        );
        assert_eq!(app.rows.len(), 1);
        assert_eq!(
            app.rows[0].clip.as_ref().unwrap().analysis.moments,
            original.analysis.moments
        );
        assert_eq!(app.scan_config(&original.source), folder);
        assert_eq!(app.remembered_preferences().watched_folders, vec![folder]);
        app.request_rescan(0, &egui::Context::default());
        assert!(
            app.work.is_none(),
            "A watched rescan must not depend on the GUI staying open"
        );
        assert!(app.rows[0].queued, "{:?}", app.error);
        assert!(matches!(
            cache.load(&original.source).unwrap().unwrap().state,
            State::Queued
        ));
        let claim = cache.claim(&original.source, false).unwrap().unwrap();
        app.import_cached(vec![(cache.load(&original.source).unwrap().unwrap(), true)]);
        assert!(app.rows[0].working);
        let current = app.rows[0].clip.as_mut().unwrap();
        current.analysis.moments[0].description = "My manual trim".into();
        app.rows[0].edited_during_scan = true;
        claim.finish(&Ok(original.clone())).unwrap();
        drop(claim);
        app.import_cached(vec![(
            cache.load(&original.source).unwrap().unwrap(),
            false,
        )]);
        assert!(!app.rows[0].working && !app.rows[0].queued);
        assert_eq!(
            app.rows[0].clip.as_ref().unwrap().analysis.moments[0].description,
            "My manual trim"
        );

        // The next GUI instance obtains the completed scan without opening or uploading the video.
        let mut restarted = Desktop {
            watched_folders: app.watched_folders.clone(),
            ..Default::default()
        };
        restarted.import_cached(
            cache
                .records()
                .unwrap()
                .into_iter()
                .map(|record| (record, false))
                .collect(),
        );
        assert_eq!(
            restarted.rows[0].clip.as_ref().unwrap().analysis.moments,
            original.analysis.moments
        );
        assert!(!restarted.dirty);
    }

    #[test]
    fn opening_a_cached_scan_uses_the_folder_profile_and_never_resends_media() {
        let (directory, mut app) = sample();
        let original = app.rows[0].clip.as_ref().unwrap().clone();
        let cache = Cache::new(directory.path().join("cache")).unwrap();
        let claim = cache.claim(&original.source, false).unwrap().unwrap();
        claim.finish(&Ok(original.clone())).unwrap();
        drop(claim);
        app.cache = Some(cache);
        app.watched_folders.push(watch::Folder {
            path: directory.path().into(),
            enabled: true,
            recursive: true,
            provider: Provider::OpenAi,
            model: "test-visual".into(),
            audio_model: "test-audio".into(),
            hint: "Folder context".into(),
            max_seconds: 90,
        });
        app.providers[0].key.clear();
        app.providers[1].key = "dummy-local-test-key".into();
        app.generate_with_mode(vec![0], &egui::Context::default(), false);
        finish_work(&mut app);
        assert!(app.error.is_none(), "{:?}", app.error);
        assert_eq!(
            app.rows[0].clip.as_ref().unwrap().analysis.moments,
            original.analysis.moments
        );
        // This fixture deliberately contains invalid video bytes: probing/uploading it would fail.
    }

    #[test]
    fn batch_and_single_exports_preserve_edited_ranges_and_saved_review_choices() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.mp4");
        recording_fixture(&source);
        let mut scanned = clip(source.clone(), "Legacy name.mp4");
        scanned.media = media::probe(&source).unwrap();
        scanned.analysis.moments[0].kept = true;
        let mut skipped = scanned.analysis.moments[0].clone();
        skipped.description = "Not selected".into();
        skipped.kept = false;
        scanned.analysis.moments.push(skipped);
        let mut row = Row::new(source.clone());
        row.clip = Some(scanned);
        let mut app = Desktop {
            rows: vec![row],
            ..Default::default()
        };
        app.export_jobs(
            app.kept_jobs(),
            directory.path().to_path_buf(),
            segments::ExportOptions::default(),
            &egui::Context::default(),
        );
        finish_work(&mut app);
        assert!(app.error.is_none(), "{:?}", app.error);
        let moments = &app.rows[0].clip.as_ref().unwrap().analysis.moments;
        assert!(moments[0].exported_to.as_ref().unwrap().exists());
        assert!(moments[1].exported_to.is_none());
        assert!(source.exists());
        let selected = &mut app.rows[0].clip.as_mut().unwrap().analysis.moments[1];
        selected.start_seconds = 0.35;
        selected.end_seconds = 0.85;
        app.export_jobs(
            app.selected_jobs(0, 1),
            directory.path().to_path_buf(),
            segments::ExportOptions::default(),
            &egui::Context::default(),
        );
        finish_work(&mut app);
        assert!(app.error.is_none(), "{:?}", app.error);
        let selected = &app.rows[0].clip.as_ref().unwrap().analysis.moments[1];
        assert!(
            !selected.kept,
            "Direct export does not require a keeper selection"
        );
        let result = media::probe(selected.exported_to.as_ref().unwrap()).unwrap();
        assert!((result.duration_seconds - 0.5).abs() < 0.1);
        let saved = directory.path().join("review.json");
        report::save_json(&app.snapshot(), &saved).unwrap();
        let mut restored = Desktop::default();
        restored.open_path(&saved).unwrap();
        assert!(restored.rows[0].clip.as_ref().unwrap().analysis.moments[0].kept);
        let edited = &restored.rows[0].clip.as_ref().unwrap().analysis.moments[1];
        assert_eq!((edited.start_seconds, edited.end_seconds), (0.35, 0.85));
        assert!(edited.exported_to.as_ref().unwrap().exists());
        assert!(
            restored.rows[0].clip.as_ref().unwrap().analysis.moments[0]
                .exported_to
                .is_some()
        );
    }

    #[test]
    fn replacement_export_updates_library_and_rebases_new_highlights() {
        let directory = tempfile::tempdir().unwrap();
        let mut rows = Vec::new();
        let mut sources = Vec::new();
        for name in ["original.mp4", "other.mp4"] {
            let source = directory.path().join(name);
            recording_fixture(&source);
            let mut original = clip(source.clone(), "Original");
            original.media = media::probe(&source).unwrap();
            original.analysis.moments[0].kept = true;
            let mut row = Row::new(source.clone());
            row.clip = Some(original);
            rows.push(row);
            sources.push(source);
        }
        let mut app = Desktop {
            rows,
            ..Default::default()
        };
        let jobs = app.kept_jobs();
        let outputs: Vec<_> = jobs
            .iter()
            .map(|job| directory.path().join(segments::export_name(&job.2, &job.3)))
            .collect();
        app.export_jobs(
            jobs,
            directory.path().into(),
            segments::ExportOptions {
                replace_original: true,
                preserve_times: true,
            },
            &egui::Context::default(),
        );
        finish_work(&mut app);
        assert!(app.error.is_none(), "{:?}", app.error);
        assert!(sources.iter().all(|source| !source.exists()));
        assert!(outputs.iter().all(|output| output.exists()));
        assert_eq!(app.rows.len(), 2);
        assert!(app.rows.iter().all(|row| outputs.contains(&row.source)
            && row.clip.as_ref().unwrap().analysis.moments[0].start_seconds == 0.0));
    }

    #[test]
    fn export_dialog_defaults_to_keeping_originals_and_preserving_dates() {
        let (_directory, mut app) = sample();
        app.prepare_export(app.selected_jobs(0, 0));
        assert!(!app.export_dialog.as_ref().unwrap().options.replace_original);
        assert!(app.export_dialog.as_ref().unwrap().options.preserve_times);
        let ctx = egui::Context::default();
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 900.0),
                )),
                ..Default::default()
            },
            |ctx| app.render(ctx),
        );
        assert!(!output.shapes.is_empty());
        assert!(app.export_dialog.is_some());
        assert!(app.work.is_none());
    }

    #[test]
    fn draws_empty_review_populated_review_and_confirmation_dialogs() {
        let (_directory, mut app) = sample();
        let context = egui::Context::default();
        for stage in 0..4 {
            if stage == 1 {
                app.rows[0].selected = true;
                app.pending = Some(Pending::Close);
            }
            if stage == 2 {
                app.pending = Some(Pending::Close);
            }
            if stage == 3 {
                app.pending = None;
                app.rows.clear();
            }
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1180.0, 800.0),
                    )),
                    ..Default::default()
                },
                |ctx| app.render(ctx),
            );
            assert!(!output.shapes.is_empty());
        }
    }
}
