use crate::provider::{Provider, configured_audio_model};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::mpsc,
};

const KEYRING_SERVICE: &str = "io.hdcontext.desktop";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSettings {
    pub model: String,
    pub audio_model: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub version: u32,
    pub folder: String,
    pub recursive: bool,
    pub provider: Provider,
    pub models: [ModelSettings; 2],
    pub hint: String,
    pub folder_contexts: Option<crate::context::FolderContexts>,
    pub max_seconds: u32,
    pub auto_find: bool,
    pub watched_folders: Vec<crate::watch::Folder>,
    pub watch_in_background: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            version: 1,
            folder: String::new(),
            recursive: true,
            provider: Provider::Gemini,
            models: Provider::ALL.map(|provider| ModelSettings {
                model: provider.configured_model(),
                audio_model: configured_audio_model(),
            }),
            hint: String::new(),
            folder_contexts: None,
            max_seconds: 300,
            auto_find: true,
            watched_folders: Vec::new(),
            watch_in_background: false,
        }
    }
}

impl Preferences {
    pub fn effective_contexts(&self) -> crate::context::FolderContexts {
        if let Some(saved) = &self.folder_contexts {
            return saved.clone();
        }
        let mut saved = crate::context::FolderContexts::new();
        if !self.folder.trim().is_empty() && !self.hint.is_empty() {
            saved.insert(
                crate::context::folder_key(Path::new(&self.folder)),
                self.hint.clone(),
            );
        }
        for folder in &self.watched_folders {
            if !folder.hint.is_empty() {
                saved.insert(
                    crate::context::folder_key(&folder.path),
                    folder.hint.clone(),
                );
            }
        }
        saved
    }
}

pub fn provider_key(provider: Provider) -> Result<Option<String>> {
    if let Ok(key) = std::env::var(provider.key_env())
        && !key.trim().is_empty()
    {
        return Ok(Some(key));
    }
    DesktopCredentialStore
        .get(provider)
        .map_err(anyhow::Error::msg)
}

pub fn config_path() -> Result<PathBuf> {
    let base =
        directories::BaseDirs::new().context("Could not find your user configuration directory")?;
    Ok(base.config_dir().join("hdcontext").join("settings.json"))
}

pub fn load(path: &Path) -> Result<Option<Preferences>> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => anyhow::bail!("Could not read the saved app settings"),
    };
    // Do not include deserialization errors: malformed files might contain misplaced secrets.
    let preferences: Preferences = serde_json::from_reader(file)
        .map_err(|_| anyhow::anyhow!("The saved app settings are not valid JSON settings"))?;
    ensure!(preferences.version == 1, "Unsupported app settings version");
    ensure!(
        preferences.max_seconds > 0,
        "The saved maximum clip duration must be positive"
    );
    Ok(Some(preferences))
}

pub fn save(preferences: &Preferences, path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .context("Settings file has no parent directory")?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(parent)
        .context("Could not create the app settings directory")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    // NamedTempFile is created with private permissions on Unix. No API keys are part of this type.
    serde_json::to_writer_pretty(&mut temporary, preferences)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|_| anyhow::anyhow!("Could not replace the app settings file"))?;
    Ok(())
}

#[derive(Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub preferences: Preferences,
    /// None preserves an inaccessible/unloaded credential. Some("") explicitly deletes one.
    pub keys: [Option<String>; 2],
}

pub enum Event {
    Loaded([std::result::Result<Option<String>, String>; 2]),
    Saved {
        revision: u64,
        settings: std::result::Result<(), String>,
        keys: [Option<std::result::Result<(), String>>; 2],
    },
}

enum Command {
    Load,
    Save {
        snapshot: Snapshot,
        revision: u64,
    },
    Flush {
        snapshot: Snapshot,
        done: mpsc::Sender<()>,
    },
}

trait CredentialStore: Send + 'static {
    fn get(&self, provider: Provider) -> std::result::Result<Option<String>, String>;
    fn set(&self, provider: Provider, value: &str) -> std::result::Result<(), String>;
    fn delete(&self, provider: Provider) -> std::result::Result<(), String>;
}

struct DesktopCredentialStore;

fn credential(provider: Provider) -> std::result::Result<keyring::Entry, String> {
    keyring::Entry::new(KEYRING_SERVICE, provider.key_env()).map_err(|_| credential_error(provider))
}

fn credential_error(provider: Provider) -> String {
    format!(
        "Could not access the {} key in your desktop credential store. Unlock the keyring and retry, or paste the key again. It has not been saved to a file.",
        provider.label()
    )
}

impl CredentialStore for DesktopCredentialStore {
    fn get(&self, provider: Provider) -> std::result::Result<Option<String>, String> {
        match credential(provider)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(credential_error(provider)),
        }
    }
    fn set(&self, provider: Provider, value: &str) -> std::result::Result<(), String> {
        credential(provider)?
            .set_password(value)
            .map_err(|_| credential_error(provider))
    }
    fn delete(&self, provider: Provider) -> std::result::Result<(), String> {
        match credential(provider)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(credential_error(provider)),
        }
    }
}

pub struct Worker {
    sender: mpsc::Sender<Command>,
    pub events: mpsc::Receiver<Event>,
}

impl Worker {
    pub fn start(path: PathBuf) -> Self {
        Self::with_store(path, DesktopCredentialStore)
    }

    #[cfg(test)]
    pub(crate) fn test_events() -> (Self, mpsc::Sender<Event>) {
        let (sender, _commands) = mpsc::channel();
        let (events_sender, events) = mpsc::channel();
        (Self { sender, events }, events_sender)
    }

    fn with_store(path: PathBuf, store: impl CredentialStore) -> Self {
        let (sender, receiver) = mpsc::channel();
        let (events_sender, events) = mpsc::channel();
        std::thread::spawn(move || {
            let mut cached: [Option<Option<String>>; 2] = [None, None];
            while let Ok(command) = receiver.recv() {
                match command {
                    Command::Load => {
                        let keys = Provider::ALL.map(|provider| {
                            let value = store.get(provider);
                            if let Ok(value) = &value {
                                cached[provider.index()] = Some(value.clone());
                            }
                            value
                        });
                        let _ = events_sender.send(Event::Loaded(keys));
                    }
                    Command::Save { snapshot, revision } => {
                        let (settings, keys) = persist(&store, &mut cached, &snapshot, &path);
                        let _ = events_sender.send(Event::Saved {
                            revision,
                            settings,
                            keys,
                        });
                    }
                    Command::Flush { snapshot, done } => {
                        let _ = persist(&store, &mut cached, &snapshot, &path);
                        let _ = done.send(());
                    }
                }
            }
        });
        let worker = Self { sender, events };
        worker.reload();
        worker
    }

    pub fn reload(&self) {
        let _ = self.sender.send(Command::Load);
    }
    pub fn save(&self, snapshot: Snapshot, revision: u64) {
        let _ = self.sender.send(Command::Save { snapshot, revision });
    }
    pub fn flush_async(&self, snapshot: Snapshot) -> mpsc::Receiver<()> {
        let (done, receiver) = mpsc::channel();
        let _ = self.sender.send(Command::Flush { snapshot, done });
        receiver
    }
    pub fn flush(&self, snapshot: Snapshot) {
        let _ = self
            .flush_async(snapshot)
            .recv_timeout(std::time::Duration::from_secs(3));
    }
}

type PersistResult = (
    std::result::Result<(), String>,
    [Option<std::result::Result<(), String>>; 2],
);

fn persist(
    store: &impl CredentialStore,
    cached: &mut [Option<Option<String>>; 2],
    snapshot: &Snapshot,
    path: &Path,
) -> PersistResult {
    let settings = save(&snapshot.preferences, path).map_err(|_| {
        "Could not save your app settings. Check the configuration directory's permissions."
            .to_string()
    });
    let keys = Provider::ALL.map(|provider| {
        let index = provider.index();
        let key = snapshot.keys[index].as_ref()?;
        let desired = if key.trim().is_empty() {
            None
        } else {
            Some(key.trim().to_string())
        };
        if cached[index].as_ref() == Some(&desired) {
            return Some(Ok(()));
        }
        let result = match &desired {
            Some(key) => store.set(provider, key),
            None => store.delete(provider),
        };
        if result.is_ok() {
            cached[index] = Some(desired);
        }
        Some(result)
    });
    (settings, keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    #[derive(Clone, Default)]
    struct MemoryStore {
        keys: Arc<Mutex<[Option<String>; 2]>>,
        fail_read: Arc<AtomicBool>,
        fail_write: Arc<AtomicBool>,
    }

    impl CredentialStore for MemoryStore {
        fn get(&self, provider: Provider) -> std::result::Result<Option<String>, String> {
            if self.fail_read.load(Ordering::Relaxed) {
                return Err(credential_error(provider));
            }
            Ok(self.keys.lock().unwrap()[provider.index()].clone())
        }
        fn set(&self, provider: Provider, key: &str) -> std::result::Result<(), String> {
            if self.fail_write.load(Ordering::Relaxed) {
                return Err(credential_error(provider));
            }
            self.keys.lock().unwrap()[provider.index()] = Some(key.into());
            Ok(())
        }
        fn delete(&self, provider: Provider) -> std::result::Result<(), String> {
            if self.fail_write.load(Ordering::Relaxed) {
                return Err(credential_error(provider));
            }
            self.keys.lock().unwrap()[provider.index()] = None;
            Ok(())
        }
    }

    fn event(worker: &Worker) -> Event {
        worker
            .events
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
    }

    #[test]
    fn legacy_context_migrates_only_to_the_original_folder() {
        let old: Preferences =
            serde_json::from_str(r#"{"folder":"/clips/Hunt", "hint":"Hunt custom context"}"#)
                .unwrap();
        let saved = old.effective_contexts();
        assert_eq!(
            crate::context::resolve(Path::new("/clips/Hunt"), &saved, &[]).0,
            "Hunt custom context"
        );
        assert_eq!(
            crate::context::resolve(Path::new("/clips/Helldivers2"), &saved, &[]).0,
            "Helldivers 2"
        );
        let modern = Preferences {
            folder_contexts: Some(Default::default()),
            ..old
        };
        assert!(modern.effective_contexts().is_empty());
    }

    #[test]
    fn preferences_roundtrip_replace_atomically_and_use_private_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hdcontext/settings.json");
        assert!(load(&path).unwrap().is_none());
        let mut prefs = Preferences {
            folder: "/recordings/Game clips".into(),
            recursive: false,
            provider: Provider::OpenAi,
            hint: "Shotgun whiffs".into(),
            max_seconds: 900,
            ..Default::default()
        };
        prefs.models[1].model = "chosen-model".into();
        save(&prefs, &path).unwrap();
        assert!(load(&path).unwrap().unwrap() == prefs);
        prefs.folder = "/another folder".into();
        save(&prefs, &path).unwrap();
        assert_eq!(load(&path).unwrap().unwrap().folder, "/another folder");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
            assert_eq!(
                fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
        }
    }

    #[test]
    fn invalid_preferences_are_not_overwritten_or_echoed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let malformed = br#"{"max_seconds":"private-test-value"}"#;
        fs::write(&path, malformed).unwrap();
        let error = load(&path).err().unwrap().to_string();
        assert!(!error.contains("private-test-value"));
        assert_eq!(fs::read(path).unwrap(), malformed);
    }

    #[test]
    fn worker_saves_provider_keys_separately_and_forgets_only_the_selected_key() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let store = MemoryStore::default();
        let worker = Worker::with_store(path.clone(), store.clone());
        assert!(matches!(event(&worker), Event::Loaded(_)));
        let snapshot = Snapshot {
            preferences: Preferences::default(),
            keys: [
                Some("gemini-test-value".into()),
                Some("openai-test-value".into()),
            ],
        };
        worker.save(snapshot.clone(), 1);
        let Event::Saved {
            revision,
            settings,
            keys,
        } = event(&worker)
        else {
            panic!("Expected save result")
        };
        assert_eq!(revision, 1);
        assert!(settings.is_ok() && keys.iter().all(|k| k.as_ref().unwrap().is_ok()));
        assert_eq!(
            store.keys.lock().unwrap()[0].as_deref(),
            Some("gemini-test-value")
        );
        assert_eq!(
            store.keys.lock().unwrap()[1].as_deref(),
            Some("openai-test-value")
        );
        let config = fs::read_to_string(&path).unwrap();
        assert!(!config.contains("gemini-test-value") && !config.contains("openai-test-value"));
        worker.reload();
        let Event::Loaded(keys) = event(&worker) else {
            panic!("Expected restored keys")
        };
        assert_eq!(
            keys[1].as_ref().unwrap().as_deref(),
            Some("openai-test-value")
        );
        worker.save(
            Snapshot {
                preferences: Preferences::default(),
                keys: [Some(String::new()), None],
            },
            2,
        );
        let _ = event(&worker);
        assert!(store.keys.lock().unwrap()[0].is_none());
        assert_eq!(
            store.keys.lock().unwrap()[1].as_deref(),
            Some("openai-test-value")
        );
    }

    #[test]
    fn inaccessible_credentials_are_preserved_and_settings_still_save_when_key_storage_fails() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let store = MemoryStore::default();
        store.keys.lock().unwrap()[0] = Some("existing-test-value".into());
        store.fail_read.store(true, Ordering::Relaxed);
        let worker = Worker::with_store(path.clone(), store.clone());
        let Event::Loaded(keys) = event(&worker) else {
            panic!("Expected loaded status")
        };
        assert!(keys[0].is_err());
        worker.save(
            Snapshot {
                preferences: Preferences {
                    folder: "remember this folder".into(),
                    ..Default::default()
                },
                keys: [None, None],
            },
            1,
        );
        let _ = event(&worker);
        assert_eq!(
            store.keys.lock().unwrap()[0].as_deref(),
            Some("existing-test-value")
        );
        assert_eq!(load(&path).unwrap().unwrap().folder, "remember this folder");
        store.fail_write.store(true, Ordering::Relaxed);
        worker.save(
            Snapshot {
                preferences: Preferences::default(),
                keys: [Some("new-test-value".into()), None],
            },
            2,
        );
        let Event::Saved { settings, keys, .. } = event(&worker) else {
            panic!("Expected save result")
        };
        assert!(settings.is_ok());
        assert!(keys[0].as_ref().unwrap().is_err());
        let json = fs::read_to_string(path).unwrap();
        assert!(!json.contains("new-test-value") && !json.contains("existing-test-value"));
    }

    #[test]
    #[ignore = "Uses the real desktop credential store with a temporary test entry"]
    fn desktop_keyring_roundtrip() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let service = format!(
            "io.hdcontext.persistence-test.{}.{unique}",
            std::process::id()
        );
        let entry = keyring::Entry::new(&service, "temporary-test").unwrap();
        struct Cleanup(keyring::Entry);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.delete_credential();
            }
        }
        let entry = Cleanup(entry);
        entry
            .0
            .set_password("hdcontext-temporary-test-value")
            .unwrap();
        assert_eq!(
            entry.0.get_password().unwrap(),
            "hdcontext-temporary-test-value"
        );
        entry.0.delete_credential().unwrap();
        assert!(matches!(
            entry.0.get_password(),
            Err(keyring::Error::NoEntry)
        ));
    }
}
