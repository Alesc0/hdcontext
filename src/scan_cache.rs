use crate::{ai::Analyzer, provider::Provider, report::Clip, workflow};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStamp {
    pub bytes: u64,
    pub modified_ns: u128,
}
impl FileStamp {
    pub fn read(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)?;
        anyhow::ensure!(metadata.is_file(), "Not a recording file");
        Ok(Self {
            bytes: metadata.len(),
            modified_ns: metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos(),
        })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum State {
    Queued,
    Scanning,
    Ready(Box<Clip>),
    Failed(String),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub source: PathBuf,
    pub stamp: FileStamp,
    pub revision: u128,
    pub state: State,
}
#[derive(Clone)]
pub struct Cache {
    root: PathBuf,
}
impl Cache {
    pub fn new(root: PathBuf) -> Result<Self> {
        private_dir(&root)?;
        Ok(Self { root })
    }
    pub fn from_config(config: &Path) -> Result<Self> {
        Self::new(
            config
                .parent()
                .context("Settings path needs a parent directory")?
                .join("scans"),
        )
    }
    fn key(source: &Path) -> String {
        format!(
            "{:x}",
            Sha256::digest(source.as_os_str().as_encoded_bytes())
        )
    }
    fn record_path(&self, source: &Path) -> PathBuf {
        self.root.join(format!("{}.json", Self::key(source)))
    }
    fn lock_path(&self, source: &Path) -> PathBuf {
        self.root.join(format!("{}.lock", Self::key(source)))
    }
    pub fn load(&self, source: &Path) -> Result<Option<Record>> {
        read_optional(&self.record_path(source))
    }
    pub fn records(&self) -> Result<Vec<Record>> {
        let mut records = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
                && path.file_name().is_some_and(|name| name != "paused.json")
                && let Some(record) = read_optional::<Record>(&path)?
            {
                records.push(record);
            }
        }
        Ok(records)
    }
    pub fn lock_source(&self, source: &Path) -> Result<File> {
        try_lock(&self.lock_path(source))?.context(
            "This recording is being scanned. Wait for that scan to finish before exporting.",
        )
    }
    pub fn remember_export(&self, clip: &Clip) -> Result<()> {
        let claim = self
            .claim(&clip.source, true)?
            .context("An exported clip is currently being scanned")?;
        claim.finish(&Ok(clip.clone()))
    }
    pub fn busy(&self, source: &Path) -> Result<bool> {
        Ok(try_lock(&self.lock_path(source))?.is_none())
    }
    pub fn claim(&self, source: &Path, force: bool) -> Result<Option<Claim>> {
        let Some(lock) = try_lock(&self.lock_path(source))? else {
            return Ok(None);
        };
        let stamp = FileStamp::read(source)?;
        if !force
            && self.load(source)?.is_some_and(|record| {
                record.stamp == stamp && !matches!(record.state, State::Queued)
            })
        {
            return Ok(None);
        }
        let claim = Claim {
            cache: self.clone(),
            source: source.to_path_buf(),
            stamp,
            _lock: lock,
        };
        claim.save(State::Scanning)?;
        Ok(Some(claim))
    }
    pub fn request_rescan(&self, source: &Path) -> Result<()> {
        let _lock = try_lock(&self.lock_path(source))?
            .context("A scan is already running for this video")?;
        atomic_json(
            &self.record_path(source),
            &Record {
                source: source.into(),
                stamp: FileStamp::read(source)?,
                revision: SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
                state: State::Queued,
            },
        )
    }
    pub fn paused(&self, provider: Provider) -> Result<bool> {
        Ok(
            read_optional::<Vec<Provider>>(&self.root.join("paused.json"))?
                .unwrap_or_default()
                .contains(&provider),
        )
    }
    pub fn set_paused(&self, provider: Provider, paused: bool) -> Result<()> {
        let lock_path = self.root.join("paused.lock");
        let lock = open_lock(&lock_path)?;
        lock.lock()?;
        let mut providers =
            read_optional::<Vec<Provider>>(&self.root.join("paused.json"))?.unwrap_or_default();
        providers.retain(|value| *value != provider);
        if paused {
            providers.push(provider);
        }
        atomic_json(&self.root.join("paused.json"), &providers)
    }
}
pub struct Claim {
    cache: Cache,
    source: PathBuf,
    stamp: FileStamp,
    _lock: File,
}
impl Claim {
    fn save(&self, state: State) -> Result<()> {
        atomic_json(
            &self.cache.record_path(&self.source),
            &Record {
                source: self.source.clone(),
                stamp: self.stamp.clone(),
                revision: SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
                state,
            },
        )
    }
    pub fn finish(&self, result: &Result<Clip>) -> Result<()> {
        self.save(match result {
            Ok(clip) => State::Ready(Box::new(clip.clone())),
            Err(error) => State::Failed(format!("{error:#}")),
        })
    }
}
pub fn analyze_cached(
    source: &Path,
    analyzer: &Analyzer,
    hint: Option<&str>,
    max_seconds: u32,
    previous: Option<&str>,
    cache: Option<&Cache>,
    force: bool,
) -> Result<Clip> {
    let claim = if let Some(cache) = cache {
        match cache.claim(source, force)? {
            Some(claim) => Some(claim),
            None => {
                if !force
                    && let Some(record) = cache.load(source)?
                    && record.stamp == FileStamp::read(source)?
                    && let State::Ready(clip) = record.state
                {
                    return Ok(*clip);
                }
                anyhow::bail!(
                    "This recording is already being scanned or has a saved scan. Rescan it from the player when the current search finishes."
                );
            }
        }
    } else {
        None
    };
    let result = workflow::analyze_one_with_previous(source, analyzer, hint, max_seconds, previous);
    if let Some(claim) = &claim {
        claim.finish(&result)?;
    }
    if let Some(cache) = cache
        && result
            .as_ref()
            .err()
            .is_some_and(crate::ai::errors::should_stop_batch)
    {
        cache.set_paused(analyzer.provider(), true)?;
    }
    result
}
pub(crate) fn private_dir(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}
pub(crate) fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("File needs a parent directory")?;
    private_dir(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}
fn read_optional<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    match File::open(path) {
        Ok(file) => Ok(Some(
            serde_json::from_reader(file).context("Saved scan data is invalid")?,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn open_lock(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}
fn try_lock(path: &Path) -> Result<Option<File>> {
    let file = open_lock(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ai::Analysis, media};
    fn clip(source: &Path) -> Clip {
        Clip {
            source: source.into(),
            sha256: media::fingerprint(source).unwrap(),
            provider: Provider::Gemini,
            model: "test".into(),
            audio_model: None,
            proposed_name: String::new(),
            media: media::MediaInfo {
                duration_seconds: 10.0,
                has_audio: false,
                audio_tracks: 0,
            },
            analysis: Analysis::default(),
        }
    }
    #[test]
    fn scan_claims_deduplicate_gui_and_daemon_and_saved_results_survive_restart() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("recording.mp4");
        fs::write(&source, b"recording").unwrap();
        let cache = Cache::new(directory.path().join("cache")).unwrap();
        let claim = cache.claim(&source, false).unwrap().unwrap();
        assert!(cache.busy(&source).unwrap());
        assert!(cache.claim(&source, true).unwrap().is_none());
        claim.finish(&Ok(clip(&source))).unwrap();
        drop(claim);
        let restarted = Cache::new(directory.path().join("cache")).unwrap();
        assert!(!restarted.busy(&source).unwrap());
        assert!(matches!(
            restarted.load(&source).unwrap().unwrap().state,
            State::Ready(_)
        ));
        assert!(restarted.claim(&source, false).unwrap().is_none());
        let rescan = restarted.claim(&source, true).unwrap().unwrap();
        rescan
            .finish(&Err(anyhow::anyhow!("Provider limit")))
            .unwrap();
        drop(rescan);
        assert!(
            restarted.claim(&source, false).unwrap().is_none(),
            "Failures are not automatically retried"
        );
        fs::write(&source, b"new recording version").unwrap();
        assert!(
            restarted.claim(&source, false).unwrap().is_some(),
            "Changed recordings become eligible"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(restarted.record_path(&source))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn interrupted_scans_and_provider_pauses_survive_process_restart() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("recording.mp4");
        fs::write(&source, b"recording").unwrap();
        let root = directory.path().join("cache");
        let cache = Cache::new(root.clone()).unwrap();
        drop(cache.claim(&source, false).unwrap().unwrap());
        let restarted = Cache::new(root).unwrap();
        assert!(!restarted.busy(&source).unwrap());
        assert!(
            restarted.claim(&source, false).unwrap().is_none(),
            "An interrupted paid request must not silently repeat"
        );
        restarted.set_paused(Provider::Gemini, true).unwrap();
        assert!(cache.paused(Provider::Gemini).unwrap());
        assert!(!cache.paused(Provider::OpenAi).unwrap());
        cache.set_paused(Provider::Gemini, false).unwrap();
        assert!(!restarted.paused(Provider::Gemini).unwrap());
        assert!(restarted.claim(&source, true).unwrap().is_some());
    }
}
