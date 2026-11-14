use crate::{
    ai::Analyzer,
    preferences,
    provider::Provider,
    scan_cache::{Cache, FileStamp},
    workflow,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    pub path: PathBuf,
    pub enabled: bool,
    pub recursive: bool,
    pub provider: Provider,
    pub model: String,
    pub audio_model: String,
    pub hint: String,
    pub max_seconds: u32,
}
impl Folder {
    pub fn contains(&self, source: &Path) -> bool {
        if self.recursive {
            source.starts_with(&self.path)
        } else {
            source.parent() == Some(self.path.as_path())
        }
    }
}
#[derive(Default)]
pub struct Settling {
    seen: HashMap<PathBuf, (FileStamp, Instant)>,
}
impl Settling {
    pub fn ready(&mut self, source: &Path, stamp: &FileStamp, now: Instant) -> bool {
        let entry = self
            .seen
            .entry(source.to_path_buf())
            .or_insert_with(|| (stamp.clone(), now));
        if entry.0 != *stamp {
            *entry = (stamp.clone(), now);
        }
        stamp.bytes > 0 && now.duration_since(entry.1) >= Duration::from_secs(6)
    }
}
pub fn files(folder: &Folder) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in
        walkdir::WalkDir::new(&folder.path).max_depth(if folder.recursive { usize::MAX } else { 1 })
    {
        let entry = entry?;
        if entry.file_type().is_file() && workflow::supported(entry.path()) {
            files.push(entry.path().canonicalize()?);
        }
    }
    files.sort();
    Ok(files)
}
pub fn candidates(
    folder: &Folder,
    cache: &Cache,
    settling: &mut Settling,
    now: Instant,
) -> Result<Vec<PathBuf>> {
    if !folder.enabled || cache.paused(folder.provider)? {
        return Ok(Vec::new());
    }
    let mut ready = Vec::new();
    for source in files(folder)? {
        let stamp = FileStamp::read(&source)?;
        if cache.load(&source)?.is_some_and(|record| {
            record.stamp == stamp && !matches!(record.state, crate::scan_cache::State::Queued)
        }) {
            continue;
        }
        if settling.ready(&source, &stamp, now) {
            ready.push(source);
        }
    }
    Ok(ready)
}

fn scan_folder(
    folder: &Folder,
    cache: &Cache,
    settling: &mut Settling,
    cancelled: &AtomicBool,
    folders: &[Folder],
    contexts: &crate::context::FolderContexts,
) -> Result<()> {
    let ready: Vec<_> = candidates(folder, cache, settling, Instant::now())?
        .into_iter()
        .filter(|source| {
            !folders.iter().any(|other| {
                other.path.components().count() > folder.path.components().count()
                    && other.contains(source)
            })
        })
        .take(1)
        .collect();
    if ready.is_empty() {
        return Ok(());
    }
    let Some(key) = preferences::provider_key(folder.provider)? else {
        return Ok(());
    };
    let analyzer =
        Analyzer::for_provider(folder.provider, &key, &folder.model, &folder.audio_model)?;
    for source in ready {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let Some(claim) = cache.claim(&source, false)? else {
            continue;
        };
        let (hint, _) =
            crate::context::resolve(source.parent().unwrap_or(&source), contexts, folders);
        let result = workflow::analyze_one_with_previous(
            &source,
            &analyzer,
            Some(&hint),
            folder.max_seconds,
            None,
        );
        claim.finish(&result)?;
        if result
            .as_ref()
            .err()
            .is_some_and(crate::ai::errors::should_stop_batch)
        {
            cache.set_paused(folder.provider, true)?;
            break;
        }
    }
    Ok(())
}

/// Each cache claim is protected by an OS lock shared with the GUI and other watchers.
pub fn run(config: &Path, cancelled: Arc<AtomicBool>, service: bool) -> Result<()> {
    let cache = Cache::from_config(config)?;
    let mut settling = Settling::default();
    let mut last_warning = String::new();
    let mut folder_warnings: HashMap<PathBuf, String> = HashMap::new();
    while !cancelled.load(Ordering::Relaxed) {
        let result = (|| -> Result<()> {
            let Some(settings) = preferences::load(config)? else {
                return Ok(());
            };
            if service && !settings.watch_in_background {
                cancelled.store(true, Ordering::Relaxed);
                return Ok(());
            }
            for folder in settings
                .watched_folders
                .iter()
                .filter(|folder| folder.enabled)
            {
                if cancelled.load(Ordering::Relaxed) {
                    break;
                }
                // Reload between requests so disabling a folder does not drain an old backlog.
                let Some(current) = preferences::load(config)? else {
                    continue;
                };
                if service && !current.watch_in_background {
                    cancelled.store(true, Ordering::Relaxed);
                    break;
                }
                let Some(folder) = current
                    .watched_folders
                    .iter()
                    .find(|current| current.path == folder.path && current.enabled)
                else {
                    continue;
                };
                match scan_folder(
                    folder,
                    &cache,
                    &mut settling,
                    &cancelled,
                    &current.watched_folders,
                    &current.effective_contexts(),
                ) {
                    Ok(()) => {
                        folder_warnings.remove(&folder.path);
                    }
                    Err(error) => {
                        let warning = format!("{error:#}");
                        if folder_warnings.get(&folder.path) != Some(&warning) {
                            eprintln!("Folder watcher [{}]: {warning}", folder.path.display());
                            folder_warnings.insert(folder.path.clone(), warning);
                        }
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let warning = format!("{error:#}");
            if warning != last_warning {
                eprintln!("Folder watcher: {warning}");
                last_warning = warning;
            }
        } else {
            last_warning.clear();
        }
        // Short waits let the GUI stop its local watcher promptly without interrupting requests.
        for _ in 0..20 {
            if cancelled.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn incoming_and_growing_files_settle_before_scanning_and_cached_files_are_skipped() {
        let directory = tempfile::tempdir().unwrap();
        let folder = Folder {
            path: directory.path().into(),
            enabled: true,
            recursive: false,
            provider: Provider::Gemini,
            model: "test".into(),
            audio_model: String::new(),
            hint: "Hunt".into(),
            max_seconds: 300,
        };
        let cache = Cache::new(directory.path().join("cache")).unwrap();
        let mut settling = Settling::default();
        let now = Instant::now();
        assert!(
            candidates(&folder, &cache, &mut settling, now)
                .unwrap()
                .is_empty()
        );
        let source = directory.path().join("incoming.mp4");
        fs::write(&source, b"partial").unwrap();
        assert!(
            candidates(&folder, &cache, &mut settling, now)
                .unwrap()
                .is_empty()
        );
        fs::write(&source, b"finished copying").unwrap();
        assert!(
            candidates(&folder, &cache, &mut settling, now + Duration::from_secs(5))
                .unwrap()
                .is_empty()
        );
        assert!(
            candidates(
                &folder,
                &cache,
                &mut settling,
                now + Duration::from_secs(10)
            )
            .unwrap()
            .is_empty()
        );
        assert_eq!(
            candidates(
                &folder,
                &cache,
                &mut settling,
                now + Duration::from_secs(11)
            )
            .unwrap(),
            vec![source.clone()]
        );
        let claim = cache.claim(&source, false).unwrap().unwrap();
        claim
            .finish(&Err(anyhow::anyhow!("No clear event")))
            .unwrap();
        drop(claim);
        assert!(
            candidates(
                &folder,
                &cache,
                &mut Settling::default(),
                now + Duration::from_secs(30)
            )
            .unwrap()
            .is_empty()
        );
        let nested = directory.path().join("sub");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("clip.mkv"), b"clip").unwrap();
        fs::write(
            directory.path().join("clip.mp4.part"),
            b"temporary transfer",
        )
        .unwrap();
        assert_eq!(files(&folder).unwrap().len(), 1);
        let mut recursive = folder;
        recursive.recursive = true;
        assert_eq!(files(&recursive).unwrap().len(), 2);
        cache.set_paused(Provider::Gemini, true).unwrap();
        assert!(
            candidates(
                &recursive,
                &cache,
                &mut settling,
                now + Duration::from_secs(50)
            )
            .unwrap()
            .is_empty()
        );
    }
}
