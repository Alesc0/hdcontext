use crate::{
    ai::Moment,
    media::{self, MediaInfo},
    report::Clip,
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::HashSet,
    fs::{self, File, FileTimes},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
};

pub fn validate_range(start: f64, end: f64, duration: f64) -> Result<()> {
    ensure!(
        start.is_finite()
            && end.is_finite()
            && duration.is_finite()
            && start >= 0.0
            && start < end
            && end <= duration,
        "Choose a range with 0 <= start < end <= the recording's duration"
    );
    Ok(())
}

pub fn render(
    source: &Path,
    destination: &Path,
    moment: &Moment,
    info: &MediaInfo,
    preview: bool,
) -> Result<()> {
    render_with_date(source, destination, moment, info, preview, None)
}

fn render_with_date(
    source: &Path,
    destination: &Path,
    moment: &Moment,
    info: &MediaInfo,
    preview: bool,
    creation_time: Option<&str>,
) -> Result<()> {
    validate_range(
        moment.start_seconds,
        moment.end_seconds,
        info.duration_seconds,
    )?;
    ensure!(
        !destination.try_exists()?,
        "Segment destination already exists: {}",
        destination.display()
    );
    let mut command = Command::new("ffmpeg");
    command
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-n",
            "-ss",
        ])
        .arg(format!("{:.6}", moment.start_seconds))
        .arg("-i")
        .arg(source)
        .args([
            "-t",
            &format!("{:.6}", moment.end_seconds - moment.start_seconds),
            "-map",
            "0:V:0",
        ]);
    if info.audio_tracks > 1 {
        let inputs: String = (0..info.audio_tracks)
            .map(|i| format!("[0:a:{i}]"))
            .collect();
        command.args([
            "-filter_complex",
            &format!(
                "{inputs}amix=inputs={}:duration=longest:normalize=1[audio]",
                info.audio_tracks
            ),
            "-map",
            "[audio]",
        ]);
    } else if info.has_audio {
        command.args(["-map", "0:a:0"]);
    }
    if preview {
        command.args(["-vf", "scale=w='min(1280,iw)':h='min(720,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2"]);
    } else {
        command.args(["-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2"]);
    }
    command.args([
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        if preview { "23" } else { "18" },
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-b:a",
        "160k",
        "-map_metadata",
        "-1",
        "-map_chapters",
        "-1",
        "-movflags",
        "+faststart",
    ]);
    if let Some(date) = creation_time {
        command.args([
            "-metadata",
            &format!("creation_time={date}"),
            "-metadata:s:v:0",
            &format!("creation_time={date}"),
        ]);
        if info.has_audio {
            command.args(["-metadata:s:a:0", &format!("creation_time={date}")]);
        }
    }
    command.arg(destination);
    let output = command
        .output()
        .context("Could not start FFmpeg to create the segment")?;
    ensure!(
        output.status.success(),
        "Could not create segment: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(
        destination
            .metadata()
            .is_ok_and(|metadata| metadata.len() > 0),
        "FFmpeg did not produce a segment: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

pub struct PreparedPreview {
    pub directory: tempfile::TempDir,
    pub path: PathBuf,
    pub moment: Moment,
}

pub fn prepare_preview(clip: &Clip, moment: &Moment) -> Result<PreparedPreview> {
    ensure!(
        media::fingerprint(&clip.source)? == clip.sha256,
        "Recording changed since scanning; scan it again before previewing"
    );
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("preview.mp4");
    render(&clip.source, &path, moment, &clip.media, true)?;
    Ok(PreparedPreview {
        directory,
        path,
        moment: moment.clone(),
    })
}

pub fn export_name(clip: &Clip, moment: &Moment) -> String {
    let source = clip
        .source
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    crate::report::proposed_name(
        &format!(
            "{:.2}-{:.2} - {} - {}",
            moment.start_seconds, moment.end_seconds, moment.description, source
        ),
        Path::new("moment.mp4"),
    )
}

pub fn export_plan(jobs: &[(usize, usize, Clip, Moment)], folder: &Path) -> Result<Vec<PathBuf>> {
    ensure!(folder.is_dir(), "Select an existing export folder");
    let mut names = HashSet::new();
    let mut checked_sources = HashSet::new();
    let mut paths = Vec::new();
    for (_, _, clip, moment) in jobs {
        validate_range(
            moment.start_seconds,
            moment.end_seconds,
            clip.media.duration_seconds,
        )?;
        if checked_sources.insert(clip.source.clone()) {
            ensure!(
                media::fingerprint(&clip.source)? == clip.sha256,
                "Recording changed since scanning: {}",
                clip.source.display()
            );
        }
        let path = folder.join(export_name(clip, moment));
        ensure!(
            names.insert(path.to_string_lossy().to_lowercase()),
            "Two moments have the same export name; edit a label or range first"
        );
        ensure!(
            std::fs::symlink_metadata(&path)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
            "Export already exists or cannot be checked: {}",
            path.display()
        );
        paths.push(path);
    }
    Ok(paths)
}

#[derive(Clone, Copy)]
pub struct ExportOptions {
    pub replace_original: bool,
    pub preserve_times: bool,
}
impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            replace_original: false,
            preserve_times: true,
        }
    }
}

pub fn export(clip: &Clip, moment: &Moment, destination: &Path) -> Result<()> {
    export_group(
        clip,
        std::slice::from_ref(moment),
        &[destination.to_path_buf()],
        ExportOptions::default(),
        &AtomicBool::new(false),
        None,
    )
    .map(|_| ())
}

/// Stage and verify every selected segment before publishing any output or removing a source.
pub fn export_group(
    clip: &Clip,
    moments: &[Moment],
    destinations: &[PathBuf],
    options: ExportOptions,
    cancel: &AtomicBool,
    cache: Option<&crate::scan_cache::Cache>,
) -> Result<Vec<Clip>> {
    ensure!(
        !moments.is_empty() && moments.len() == destinations.len(),
        "Choose at least one clip to export"
    );
    ensure!(
        !cancel.load(Ordering::Relaxed),
        "Export stopped. The original recording was kept"
    );
    let _source_lock = cache
        .map(|cache| cache.lock_source(&clip.source))
        .transpose()?;
    let metadata = fs::symlink_metadata(&clip.source)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Export requires an original recording file, not a symlink"
    );
    ensure!(
        media::fingerprint(&clip.source)? == clip.sha256,
        "Recording changed since scanning; scan it again before exporting"
    );
    let creation_time = if options.preserve_times {
        Some(recording_date(&clip.source, &metadata)?)
    } else {
        None
    };
    let mut directories = Vec::new();
    let mut staged = Vec::new();
    let mut outputs = Vec::new();
    let mut names = HashSet::new();
    for (moment, destination) in moments.iter().zip(destinations) {
        validate_range(
            moment.start_seconds,
            moment.end_seconds,
            clip.media.duration_seconds,
        )?;
        ensure!(
            names.insert(destination.to_string_lossy().to_lowercase()),
            "Two clips have the same destination"
        );
        ensure!(
            !destination.try_exists()?,
            "Export already exists: {}",
            destination.display()
        );
        ensure!(
            destination != &clip.source,
            "A clip cannot overwrite its source recording"
        );
    }
    for (moment, destination) in moments.iter().zip(destinations) {
        ensure!(
            !cancel.load(Ordering::Relaxed),
            "Export stopped. The original recording was kept"
        );
        let directory = tempfile::tempdir_in(
            destination
                .parent()
                .context("Export has no parent folder")?,
        )?;
        let temporary = directory.path().join("segment.mp4");
        render_with_date(
            &clip.source,
            &temporary,
            moment,
            &clip.media,
            false,
            creation_time.as_deref(),
        )?;
        let info = media::probe(&temporary)?;
        let expected = moment.end_seconds - moment.start_seconds;
        ensure!(
            (info.duration_seconds - expected).abs() <= 0.25_f64.max(expected * 0.01),
            "Exported clip has an unexpected duration. Original kept"
        );
        ensure!(
            info.has_audio == clip.media.has_audio,
            "Exported audio could not be verified. Original kept"
        );
        let hash = media::fingerprint(&temporary)?;
        if options.preserve_times {
            preserve_file_times(&temporary, &metadata)?;
        }
        File::open(&temporary)?.sync_all()?;
        let exported = Clip {
            source: destination.clone(),
            sha256: hash,
            provider: clip.provider,
            model: clip.model.clone(),
            audio_model: clip.audio_model.clone(),
            proposed_name: destination
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into(),
            media: info.clone(),
            analysis: crate::ai::Analysis {
                moments: vec![Moment {
                    start_seconds: 0.0,
                    end_seconds: info.duration_seconds,
                    description: moment.description.clone(),
                    kept: true,
                    exported_to: Some(destination.clone()),
                }],
                ..Default::default()
            },
        };
        staged.push(temporary);
        directories.push(directory);
        outputs.push(exported);
    }
    ensure!(
        !cancel.load(Ordering::Relaxed),
        "Export stopped. The original recording was kept"
    );
    ensure!(
        media::fingerprint(&clip.source)? == clip.sha256,
        "Recording changed during export. Original kept"
    );
    let mut committed = Vec::new();
    let mut removal_started = false;
    let result = (|| -> Result<()> {
        for (temporary, destination) in staged.iter().zip(destinations) {
            fs::hard_link(temporary, destination).with_context(|| {
                format!(
                    "Cannot save {}. Existing files are never overwritten",
                    destination.display()
                )
            })?;
            committed.push((temporary.clone(), destination.clone()));
        }
        #[cfg(unix)]
        for parent in destinations
            .iter()
            .filter_map(|path| path.parent())
            .collect::<HashSet<_>>()
        {
            File::open(parent)?.sync_all()?;
        }
        if let Some(cache) = cache {
            for output in &outputs {
                cache.remember_export(output)?;
            }
        }
        if options.replace_original {
            ensure!(
                media::fingerprint(&clip.source)? == clip.sha256,
                "Recording changed before replacement. Original kept"
            );
            removal_started = true;
            fs::remove_file(&clip.source).context("Clips were saved, but the original could not be removed. The exported clips were kept")?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        // Remove only links still referring to our own staged files, never an unrelated replacement.
        for (temporary, destination) in committed.iter().filter(|_| !removal_started) {
            if same_output(temporary, destination) {
                let _ = fs::remove_file(destination);
            }
        }
        return Err(error);
    }
    Ok(outputs)
}

fn same_output(staged: &Path, destination: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (fs::metadata(staged), fs::metadata(destination)) {
            (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        matches!((media::fingerprint(staged),media::fingerprint(destination)),(Ok(a),Ok(b))if a==b)
    }
}
fn preserve_file_times(destination: &Path, metadata: &fs::Metadata) -> Result<()> {
    let mut times = FileTimes::new().set_modified(metadata.modified()?);
    if let Ok(accessed) = metadata.accessed() {
        times = times.set_accessed(accessed);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTimesExt;
        if let Ok(created) = metadata.created() {
            times = times.set_created(created);
        }
    }
    fs::OpenOptions::new()
        .write(true)
        .open(destination)?
        .set_times(times)
        .context("Could not preserve the recording's file date and time")
}
fn recording_date(source: &Path, metadata: &fs::Metadata) -> Result<String> {
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format_tags=creation_time:stream_tags=creation_time",
            "-of",
            "json",
        ])
        .arg(source)
        .output()?;
    ensure!(output.status.success(), "Could not read the recording date");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let mut candidates = Vec::new();
    if let Some(date) = value["format"]["tags"]["creation_time"].as_str() {
        candidates.push(date);
    }
    if let Some(streams) = value["streams"].as_array() {
        for stream in streams {
            if let Some(date) = stream["tags"]["creation_time"].as_str() {
                candidates.push(date);
            }
        }
    }
    for date in candidates {
        if let Ok(date) = OffsetDateTime::parse(date, &Rfc3339) {
            return Ok(date.to_offset(time::UtcOffset::UTC).format(&Rfc3339)?);
        }
    }
    Ok(OffsetDateTime::from(metadata.modified()?).format(&Rfc3339)?)
}

pub fn timestamp(seconds: f64) -> String {
    let tenths = (seconds.max(0.0) * 10.0).round() as u64;
    format!(
        "{:02}:{:02}.{}",
        tenths / 600,
        tenths / 10 % 60,
        tenths % 10
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ai::Analysis, provider::Provider};

    fn sample(folder: &Path) -> (Clip, Moment) {
        let source = folder.join("recording.mkv");
        std::fs::write(&source, b"recording bytes").unwrap();
        let clip = Clip {
            provider: Provider::Gemini,
            model: String::new(),
            audio_model: None,
            sha256: media::fingerprint(&source).unwrap(),
            source,
            proposed_name: String::new(),
            media: MediaInfo {
                duration_seconds: 10.0,
                has_audio: false,
                audio_tracks: 0,
            },
            analysis: Analysis::default(),
        };
        let moment = Moment {
            start_seconds: 2.0,
            end_seconds: 5.0,
            description: "Shotgun / whiff?".into(),
            kept: true,
            exported_to: None,
        };
        (clip, moment)
    }

    #[test]
    fn export_plan_rejects_collisions_and_changed_sources_before_creating_files() {
        let directory = tempfile::tempdir().unwrap();
        let (clip, moment) = sample(directory.path());
        let jobs = vec![(0, 0, clip.clone(), moment.clone())];
        let plan = export_plan(&jobs, directory.path()).unwrap();
        assert_eq!(plan[0].extension().unwrap(), "mp4");
        assert!(!plan[0].file_name().unwrap().to_string_lossy().contains('?'));
        assert!(export_plan(&[jobs[0].clone(), jobs[0].clone()], directory.path()).is_err());
        std::fs::write(&plan[0], b"existing export").unwrap();
        assert!(export_plan(&jobs, directory.path()).is_err());
        assert_eq!(std::fs::read(&plan[0]).unwrap(), b"existing export");
        std::fs::remove_file(&plan[0]).unwrap();
        std::fs::write(&clip.source, b"changed recording").unwrap();
        assert!(export_plan(&jobs, directory.path()).is_err());
        assert!(prepare_preview(&clip, &moment).is_err());
        assert!(!plan[0].exists());
    }
}
