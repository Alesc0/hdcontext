use crate::{
    ai::Analysis,
    media::{MediaInfo, fingerprint},
    provider::Provider,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    #[serde(default)]
    pub provider: Provider,
    #[serde(default)]
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_model: Option<String>,
    pub source: PathBuf,
    pub sha256: String,
    pub proposed_name: String,
    pub media: MediaInfo,
    pub analysis: Analysis,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Failure {
    pub source: PathBuf,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub version: u32,
    #[serde(default)]
    pub provider: Provider,
    pub model: String,
    pub clips: Vec<Clip>,
    pub failures: Vec<Failure>,
}

pub fn proposed_name(title: &str, source: &Path) -> String {
    let mut stem = String::new();
    let mut space = false;
    for ch in title.chars() {
        if ch.is_control() || "<>:\"/\\|?*".contains(ch) || ch.is_whitespace() {
            space = true;
        } else {
            if space && !stem.is_empty() {
                stem.push(' ');
            }
            stem.push(ch);
            space = false;
        }
        if stem.len() >= 150 {
            break;
        }
    }
    let stem = stem.trim_matches([' ', '.']).to_string();
    let mut stem = if stem.is_empty() {
        "Game clip".to_string()
    } else {
        stem
    };
    if reserved(&stem) {
        stem.insert_str(0, "Clip ");
    }
    // Never use a model-generated extension.
    if let Some(ext) = source.extension().and_then(|e| e.to_str()) {
        format!("{stem}.{ext}")
    } else {
        stem
    }
}

fn reserved(name: &str) -> bool {
    let base = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ]
    .contains(&base.as_str())
}

pub fn save(report: &Report, destination: &Path) -> Result<()> {
    save_json(report, destination)
}

pub fn save_json(value: &impl Serialize, destination: &Path) -> Result<()> {
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, value)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(destination).with_context(|| {
        format!(
            "Cannot save report to {}; choose an unused output path",
            destination.display()
        )
    })?;
    Ok(())
}

pub fn load(path: &Path) -> Result<Report> {
    let report: Report = serde_json::from_reader(fs::File::open(path)?)?;
    ensure!(
        report.version == 1,
        "Unsupported report version {}",
        report.version
    );
    Ok(report)
}

pub fn rename_plan(report: &Report, indices: &[usize]) -> Result<Vec<(PathBuf, PathBuf)>> {
    let mut targets = HashSet::new();
    let mut sources = HashSet::new();
    let mut plan = Vec::new();
    for &index in indices {
        let clip = report
            .clips
            .get(index.checked_sub(1).context("Clip indices start at 1")?)
            .context("Clip index is outside this report")?;
        ensure!(
            clip.source.is_absolute(),
            "Report source must be an absolute path"
        );
        let metadata = fs::symlink_metadata(&clip.source)
            .with_context(|| format!("Cannot access {}", clip.source.display()))?;
        ensure!(
            metadata.file_type().is_file(),
            "Source must be a regular file, not a symlink"
        );
        let name = &clip.proposed_name;
        ensure!(
            !name.is_empty()
                && name.len() <= 200
                && !name
                    .chars()
                    .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
                && !name.ends_with([' ', '.'])
                && name != "."
                && name != ".."
                && !reserved(name),
            "Invalid proposed filename: {name:?}"
        );
        ensure!(
            Path::new(name).extension() == clip.source.extension(),
            "Proposed filename must preserve the original extension"
        );
        ensure!(
            fingerprint(&clip.source)? == clip.sha256,
            "Source changed since analysis: {}",
            clip.source.display()
        );
        let destination = clip.source.with_file_name(name);
        ensure!(
            sources.insert(clip.source.clone()),
            "Duplicate source in rename selection"
        );
        ensure!(
            targets.insert(destination.to_string_lossy().to_lowercase()),
            "Two selected clips have the same target name; edit the report to disambiguate them"
        );
        if destination != clip.source {
            ensure!(
                fs::symlink_metadata(&destination)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
                "Destination already exists or cannot be checked: {}",
                destination.display()
            );
        }
        plan.push((clip.source.clone(), destination));
    }
    Ok(plan)
}

pub fn rename_no_clobber(source: &Path, destination: &Path) -> Result<()> {
    if source == destination {
        return Ok(());
    }
    // Creating a hard link fails atomically if destination exists. Both names are in the same directory.
    fs::hard_link(source, destination).with_context(|| format!("Cannot create {}. The filesystem must support hard links; existing files are never overwritten", destination.display()))?;
    if let Err(error) = fs::remove_file(source) {
        let rollback = fs::remove_file(destination);
        return Err(error).context(format!(
            "Could not remove original {}; destination rollback: {rollback:?}",
            source.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filenames_are_portable_and_keep_extension() {
        let source = Path::new("old.MKV");
        assert_eq!(
            proposed_name("Boss: fight / victory?", source),
            "Boss fight victory.MKV"
        );
        assert_eq!(proposed_name("...", source), "Game clip.MKV");
        assert_eq!(proposed_name("CON", source), "Clip CON.MKV");
        assert!(proposed_name(&"🎮".repeat(100), source).len() < 200);
    }
    #[test]
    fn rename_preserves_data_and_refuses_collisions() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("old.mp4");
        let destination = dir.path().join("new.mp4");
        fs::write(&source, b"clip").unwrap();
        fs::write(&destination, b"other").unwrap();
        assert!(rename_no_clobber(&source, &destination).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"clip");
        assert_eq!(fs::read(&destination).unwrap(), b"other");
        fs::remove_file(&destination).unwrap();
        rename_no_clobber(&source, &destination).unwrap();
        assert!(!source.exists());
        assert_eq!(fs::read(&destination).unwrap(), b"clip");
    }
    fn sample_report(source: PathBuf) -> Report {
        Report {
            version: 1,
            provider: Provider::Gemini,
            model: "test".into(),
            failures: vec![],
            clips: vec![Clip {
                provider: Provider::Gemini,
                model: "test".into(),
                audio_model: None,
                sha256: fingerprint(&source).unwrap(),
                source,
                proposed_name: "new.mp4".into(),
                media: MediaInfo {
                    duration_seconds: 1.0,
                    has_audio: false,
                    audio_tracks: 0,
                },
                analysis: Analysis {
                    title: "Test".into(),
                    game: "Unknown game".into(),
                    summary: "Test".into(),
                    audio_summary: "Silent".into(),
                    confidence: "low".into(),
                    highlights: vec![],
                    moments: vec![],
                    uncertainty: "Test".into(),
                },
            }],
        }
    }
    #[test]
    fn review_rejects_traversal_changed_source_and_bad_indices() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("old.mp4");
        fs::write(&source, b"clip").unwrap();
        let mut report = sample_report(source.clone());
        assert!(rename_plan(&report, &[1]).is_ok());
        assert!(rename_plan(&report, &[0]).is_err());
        assert!(rename_plan(&report, &[2]).is_err());
        assert!(rename_plan(&report, &[1, 1]).is_err());
        report.clips[0].proposed_name = "../other.mp4".into();
        assert!(rename_plan(&report, &[1]).is_err());
        report.clips[0].proposed_name = "new.mp4".into();
        fs::write(source, b"changed").unwrap();
        assert!(rename_plan(&report, &[1]).is_err());
    }
    #[test]
    fn reports_roundtrip_and_never_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("old.mp4");
        fs::write(&source, b"clip").unwrap();
        let report = sample_report(source);
        let output = dir.path().join("report.json");
        save(&report, &output).unwrap();
        assert_eq!(load(&output).unwrap().clips.len(), 1);
        assert!(save(&report, &output).is_err());
    }
}
