use crate::{
    ai::Analyzer,
    media,
    report::{self, Clip},
};
use anyhow::{Result, ensure};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

pub fn discover(paths: &[PathBuf], recursive: bool) -> Result<Vec<PathBuf>> {
    let mut videos = BTreeSet::new();
    for path in paths {
        ensure!(
            !fs::symlink_metadata(path)?.file_type().is_symlink(),
            "Use a real file or directory, not a symlink: {}",
            path.display()
        );
        if path.is_file() {
            // Explicit files are probed even if their extension is unusual.
            videos.insert(path.canonicalize()?);
        } else {
            ensure!(
                path.is_dir(),
                "Not a video file or directory: {}",
                path.display()
            );
            for entry in WalkDir::new(path).max_depth(if recursive { usize::MAX } else { 1 }) {
                let entry = entry?;
                if entry.file_type().is_file() && supported(entry.path()) {
                    videos.insert(entry.path().canonicalize()?);
                }
            }
        }
    }
    ensure!(!videos.is_empty(), "No video files found");
    Ok(videos.into_iter().collect())
}

pub(crate) fn supported(path: &Path) -> bool {
    path.extension().and_then(|x| x.to_str()).is_some_and(|x| {
        [
            "mp4", "mkv", "mov", "webm", "avi", "m4v", "ts", "mts", "flv",
        ]
        .contains(&x.to_ascii_lowercase().as_str())
    })
}

pub fn analyze_one(
    source: &Path,
    analyzer: &Analyzer,
    hint: Option<&str>,
    max_seconds: u32,
) -> Result<Clip> {
    analyze_one_with_previous(source, analyzer, hint, max_seconds, None)
}

pub fn analyze_one_with_previous(
    source: &Path,
    analyzer: &Analyzer,
    hint: Option<&str>,
    max_seconds: u32,
    previous: Option<&str>,
) -> Result<Clip> {
    let info = media::probe(source)?;
    ensure!(
        info.duration_seconds <= f64::from(max_seconds),
        "Clip is {:.1}s, exceeding --max-seconds {max_seconds}. Use a shorter clip or raise the limit. Nothing was uploaded",
        info.duration_seconds
    );
    let sha256 = media::fingerprint(source)?;
    let temporary = tempfile::tempdir()?;
    let prepared = temporary.path().join("clip.mp4");
    media::prepare(source, &prepared, &info)?;
    ensure!(
        media::fingerprint(source)? == sha256,
        "Source changed during preparation. Nothing was uploaded"
    );
    let analysis = analyzer.analyze_prepared(&prepared, &info, hint, previous)?;
    let proposed_name = report::proposed_name(&analysis.title, source);
    Ok(Clip {
        provider: analyzer.provider(),
        model: analyzer.model().into(),
        audio_model: (info.has_audio && analyzer.provider() == crate::provider::Provider::OpenAi)
            .then(|| analyzer.audio_model().into()),
        source: source.to_path_buf(),
        sha256,
        proposed_name,
        media: info,
        analysis,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovers_videos_without_duplicates_and_respects_recursion() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("one.MP4"), b"test").unwrap();
        fs::write(dir.path().join("sub/two.mkv"), b"test").unwrap();
        fs::write(dir.path().join("notes.txt"), b"test").unwrap();
        let paths = vec![dir.path().to_path_buf(), dir.path().join("one.MP4")];
        assert_eq!(discover(&paths, false).unwrap().len(), 1);
        assert_eq!(discover(&paths, true).unwrap().len(), 2);
    }
}
