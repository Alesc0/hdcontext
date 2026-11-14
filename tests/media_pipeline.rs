use hdcontext::{ai::Moment, media, segments};
use std::{path::Path, process::Command};

fn fixture(path: &Path, audio_tracks: usize) {
    let mut command = Command::new("ffmpeg");
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-f",
        "lavfi",
        "-i",
        "color=c=blue:s=320x180:r=24:d=1",
    ]);
    for frequency in [440, 880].iter().take(audio_tracks) {
        command.args([
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency={frequency}:duration=1"),
        ]);
    }
    command.args(["-map", "0:v"]);
    for i in 1..=audio_tracks {
        command.args(["-map", &format!("{i}:a")]);
    }
    let result = command
        .args(["-c:v", "libx264", "-c:a", "aac", "-t", "1"])
        .arg(path)
        .output()
        .expect("FFmpeg is required for media integration tests");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn compresses_entire_video_and_mixes_all_audio_tracks() {
    let dir = tempfile::tempdir().unwrap();
    // Punctuation and spaces must remain literal path characters.
    let source = dir.path().join("clip $literal 'name';.mkv");
    let destination = dir.path().join("prepared.mp4");
    fixture(&source, 2);
    let original_hash = media::fingerprint(&source).unwrap();
    let info = media::probe(&source).unwrap();
    assert_eq!(info.audio_tracks, 2);
    media::prepare(&source, &destination, &info).unwrap();
    let prepared = media::probe(&destination).unwrap();
    assert_eq!(prepared.audio_tracks, 1);
    assert!((prepared.duration_seconds - info.duration_seconds).abs() < 0.3);
    assert_eq!(media::fingerprint(&source).unwrap(), original_hash);
    assert!(destination.metadata().unwrap().len() < media::MAX_MEDIA_BYTES);
    let samples = media::sample_prepared(&destination, &dir.path().join("samples"), &info).unwrap();
    assert!(!samples.frames.is_empty() && samples.frames.len() <= 120);
    assert_eq!(samples.frames[0].seconds, 0.0);
    assert!(
        samples
            .frames
            .iter()
            .all(|f| f.jpeg.starts_with(&[0xff, 0xd8]) && f.seconds < info.duration_seconds)
    );
    assert!(samples.audio_wav.as_ref().unwrap().starts_with(b"RIFF"));
    // Decode the mixed track to confirm it contains audible samples, not just an empty stream.
    let audio = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&destination)
        .args(["-map", "0:a:0", "-f", "s16le", "-acodec", "pcm_s16le", "-"])
        .output()
        .unwrap();
    assert!(audio.status.success());
    assert!(
        audio
            .stdout
            .as_chunks::<2>()
            .0
            .iter()
            .any(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs() > 100)
    );
}

#[test]
fn supports_silent_video_and_rejects_invalid_media() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("silent.mp4");
    fixture(&source, 0);
    let info = media::probe(&source).unwrap();
    assert!(!info.has_audio);
    let destination = dir.path().join("prepared.mp4");
    media::prepare(&source, &destination, &info).unwrap();
    assert!(!media::probe(&destination).unwrap().has_audio);
    let samples = media::sample_prepared(&destination, &dir.path().join("samples"), &info).unwrap();
    assert!(samples.audio_wav.is_none());
    assert!(!samples.frames.is_empty());
    let invalid = dir.path().join("invalid.mp4");
    std::fs::write(&invalid, b"not a video").unwrap();
    assert!(media::probe(&invalid).is_err());
}

#[test]
fn frame_sampling_covers_long_clips_without_exceeding_the_frame_budget() {
    for duration in [0.1_f64, 1.0, 30.0, 61.0, 300.0, 3600.0] {
        let stride = media::frame_stride(duration);
        let samples = (duration * 8.0 / stride as f64).ceil();
        assert!(samples <= 120.0);
        assert!(stride >= 4); // At most 2 fps from the prepared 8-fps timeline.
        assert!((samples - 1.0) * stride as f64 / 8.0 < duration);
    }
}

#[test]
fn preview_and_export_cut_only_the_moment_and_keep_audio_and_original_frame_rate() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.mkv");
    fixture(&source, 2);
    let info = media::probe(&source).unwrap();
    let fingerprint = media::fingerprint(&source).unwrap();
    let moment = Moment {
        start_seconds: 0.2,
        end_seconds: 0.7,
        description: "Shotgun whiff".into(),
        kept: false,
        exported_to: None,
    };
    for preview in [true, false] {
        let destination = dir
            .path()
            .join(if preview { "preview.mp4" } else { "export.mp4" });
        segments::render(&source, &destination, &moment, &info, preview).unwrap();
        let result = media::probe(&destination).unwrap();
        assert!((result.duration_seconds - 0.5).abs() < 0.15);
        assert_eq!(result.audio_tracks, 1);
        let output = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=avg_frame_rate",
                "-of",
                "json",
            ])
            .arg(&destination)
            .output()
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["streams"][0]["avg_frame_rate"], "24/1");
        assert!(segments::render(&source, &destination, &moment, &info, preview).is_err());
    }
    assert_eq!(media::fingerprint(&source).unwrap(), fingerprint);
}

#[test]
fn segments_support_silent_recordings_and_reject_invalid_ranges() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("silent.mp4");
    fixture(&source, 0);
    let info = media::probe(&source).unwrap();
    let destination = dir.path().join("segment.mp4");
    let mut moment = Moment {
        start_seconds: 0.0,
        end_seconds: 0.5,
        description: "Missed jump".into(),
        kept: true,
        exported_to: None,
    };
    segments::render(&source, &destination, &moment, &info, true).unwrap();
    assert!(!media::probe(&destination).unwrap().has_audio);
    for (start, end) in [
        (0.5, 0.5),
        (0.9, 0.2),
        (-1.0, 0.2),
        (0.0, 10.0),
        (f64::NAN, 0.2),
    ] {
        moment.start_seconds = start;
        moment.end_seconds = end;
        assert!(
            segments::render(
                &source,
                &dir.path().join("invalid.mp4"),
                &moment,
                &info,
                false
            )
            .is_err()
        );
        assert!(!dir.path().join("invalid.mp4").exists());
    }
}

// Verify actual picture content, including variable frame rate and seeks between keyframes.
#[test]
fn analysis_and_excerpts_preserve_the_source_event_timeline() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("timeline.mp4");
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin"])
        .args(["-f", "lavfi", "-i", "color=red:s=320x180:r=24:d=2"])
        .args(["-f", "lavfi", "-i", "color=green:s=320x180:r=24:d=2"])
        .args(["-f", "lavfi", "-i", "color=blue:s=320x180:r=24:d=2"])
        .args([
            "-filter_complex",
            "[0:v][1:v][2:v]concat=n=3:v=1:a=0,select='if(lt(t,2),not(mod(n,3)),1)'[v]",
            "-map",
            "[v]",
            "-fps_mode",
            "vfr",
            "-c:v",
            "libx264",
            "-g",
            "240",
            "-sc_threshold",
            "0",
        ])
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let info = media::probe(&source).unwrap();
    let prepared = directory.path().join("analysis.mp4");
    media::prepare(&source, &prepared, &info).unwrap();
    assert!(
        (media::probe(&prepared).unwrap().duration_seconds - info.duration_seconds).abs() < 0.15
    );
    for (at, channel) in [(0.5, 0), (2.5, 1), (4.5, 2)] {
        assert_picture_color(&source, at, channel);
        assert_picture_color(&prepared, at, channel);
    }
    for (start, channel) in [(2.25, 1), (4.25, 2)] {
        for preview in [true, false] {
            let target = directory.path().join(format!("cut-{start}-{preview}.mp4"));
            let moment = Moment {
                start_seconds: start,
                end_seconds: start + 1.5,
                description: "Known event".into(),
                kept: false,
                exported_to: None,
            };
            segments::render(&source, &target, &moment, &info, preview).unwrap();
            for at in [0.0, 0.5, 1.25] {
                assert_picture_color(&target, at, channel);
            }
        }
    }
    assert_eq!(media::clip_timecode(59.9996), "00:01:00.000");
    assert_eq!(segments::timestamp(59.999), "01:00.0");
}

fn assert_picture_color(path: &Path, at: f64, channel: usize) {
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-ss", &at.to_string(), "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            "crop=2:2:iw/2:ih/2,scale=1:1",
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "-",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout.len(),
        3,
        "missing frame at {at} in {}",
        path.display()
    );
    let rgb = output.stdout;
    assert!(
        rgb[channel] > 80 && (0..3).filter(|&c| c != channel).all(|c| rgb[c] < 35),
        "wrong picture at {at} in {}: {rgb:?}",
        path.display()
    );
}

#[test]
fn replacement_exports_all_selected_clips_preserves_dates_and_caches_the_results() {
    use hdcontext::{ai::Analysis, provider::Provider, report::Clip, scan_cache::Cache, watch};
    use std::{
        fs::{self, File, FileTimes},
        sync::atomic::AtomicBool,
        time::{Duration, Instant, UNIX_EPOCH},
    };
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("dated.mp4");
    fixture(&source, 1);
    let tagged = directory.path().join("tagged.mp4");
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(&source)
        .args([
            "-c",
            "copy",
            "-metadata",
            "creation_time=2020-06-01T12:34:56Z",
        ])
        .arg(&tagged)
        .output()
        .unwrap();
    assert!(output.status.success());
    fs::rename(tagged, &source).unwrap();
    let modified = UNIX_EPOCH + Duration::new(1_609_459_200, 123_456_789);
    File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_times(FileTimes::new().set_modified(modified))
        .unwrap();
    let clip = Clip {
        source: source.clone(),
        sha256: media::fingerprint(&source).unwrap(),
        provider: Provider::Gemini,
        model: "test".into(),
        audio_model: None,
        proposed_name: String::new(),
        media: media::probe(&source).unwrap(),
        analysis: Analysis::default(),
    };
    let moments = vec![
        Moment {
            start_seconds: 0.0,
            end_seconds: 0.4,
            description: "First event".into(),
            kept: true,
            exported_to: None,
        },
        Moment {
            start_seconds: 0.5,
            end_seconds: 0.9,
            description: "Second event".into(),
            kept: true,
            exported_to: None,
        },
    ];
    let destinations = vec![
        directory.path().join("first.mp4"),
        directory.path().join("second.mp4"),
    ];
    let cache = Cache::new(directory.path().join("cache")).unwrap();
    let outputs = segments::export_group(
        &clip,
        &moments,
        &destinations,
        segments::ExportOptions {
            replace_original: true,
            preserve_times: true,
        },
        &AtomicBool::new(false),
        Some(&cache),
    )
    .unwrap();
    assert!(!source.exists());
    assert_eq!(outputs.len(), 2);
    for (output, path) in outputs.iter().zip(&destinations) {
        assert!(path.exists());
        assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), modified);
        assert!((media::probe(path).unwrap().duration_seconds - 0.4).abs() < 0.15);
        assert_eq!(output.analysis.moments[0].start_seconds, 0.0);
        assert!(cache.load(path).unwrap().is_some());
        let result = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "format_tags=creation_time",
                "-of",
                "json",
            ])
            .arg(path)
            .output()
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(
            value["format"]["tags"]["creation_time"],
            "2020-06-01T12:34:56.000000Z"
        );
    }
    let folder = watch::Folder {
        path: directory.path().into(),
        enabled: true,
        recursive: false,
        provider: Provider::Gemini,
        model: "test".into(),
        audio_model: String::new(),
        hint: String::new(),
        max_seconds: 300,
    };
    assert!(
        watch::candidates(
            &folder,
            &cache,
            &mut watch::Settling::default(),
            Instant::now()
        )
        .unwrap()
        .is_empty(),
        "Exported clips must not trigger new API scans"
    );
}

#[test]
fn replacement_keeps_original_on_collision_invalid_range_cancellation_or_active_scan() {
    use hdcontext::{ai::Analysis, provider::Provider, report::Clip, scan_cache::Cache};
    use std::{fs, sync::atomic::AtomicBool};
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.mp4");
    fixture(&source, 0);
    let hash = media::fingerprint(&source).unwrap();
    let clip = Clip {
        source: source.clone(),
        sha256: hash.clone(),
        provider: Provider::Gemini,
        model: "test".into(),
        audio_model: None,
        proposed_name: String::new(),
        media: media::probe(&source).unwrap(),
        analysis: Analysis::default(),
    };
    let good = Moment {
        start_seconds: 0.0,
        end_seconds: 0.5,
        description: "Event".into(),
        kept: true,
        exported_to: None,
    };
    let paths = vec![
        directory.path().join("one.mp4"),
        directory.path().join("two.mp4"),
    ];
    let options = segments::ExportOptions {
        replace_original: true,
        preserve_times: true,
    };
    fs::write(&paths[1], b"existing data").unwrap();
    assert!(
        segments::export_group(
            &clip,
            &[good.clone(), good.clone()],
            &paths,
            options,
            &AtomicBool::new(false),
            None
        )
        .is_err()
    );
    assert_eq!(fs::read(&paths[1]).unwrap(), b"existing data");
    assert!(!paths[0].exists());
    fs::remove_file(&paths[1]).unwrap();
    let mut invalid = good.clone();
    invalid.end_seconds = 9.0;
    assert!(
        segments::export_group(
            &clip,
            &[good.clone(), invalid],
            &paths,
            options,
            &AtomicBool::new(false),
            None
        )
        .is_err()
    );
    assert!(
        segments::export_group(
            &clip,
            std::slice::from_ref(&good),
            &paths[..1],
            options,
            &AtomicBool::new(true),
            None
        )
        .is_err()
    );
    let mut truncated = clip.clone();
    truncated.media.duration_seconds = 2.0;
    let mut missing = good.clone();
    missing.start_seconds = 1.25;
    missing.end_seconds = 1.5;
    assert!(
        segments::export_group(
            &truncated,
            &[good.clone(), missing],
            &paths,
            options,
            &AtomicBool::new(false),
            None
        )
        .is_err(),
        "A failure after rendering the first staged clip must keep the original"
    );
    let cache = Cache::new(directory.path().join("cache")).unwrap();
    let claim = cache.claim(&source, false).unwrap().unwrap();
    assert!(
        segments::export_group(
            &clip,
            &[good],
            &paths[..1],
            options,
            &AtomicBool::new(false),
            Some(&cache)
        )
        .is_err()
    );
    drop(claim);
    assert_eq!(media::fingerprint(&source).unwrap(), hash);
    assert!(paths.iter().all(|path| !path.exists()));
}

#[test]
fn copy_export_preserves_file_date_as_metadata_fallback_and_allows_new_dates() {
    use hdcontext::{ai::Analysis, provider::Provider, report::Clip};
    use std::{
        fs::{self, File, FileTimes},
        sync::atomic::AtomicBool,
        time::{Duration, UNIX_EPOCH},
    };
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.mp4");
    fixture(&source, 0);
    let old = UNIX_EPOCH + Duration::from_secs(978_307_200);
    File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_times(FileTimes::new().set_modified(old))
        .unwrap();
    let clip = Clip {
        source: source.clone(),
        sha256: media::fingerprint(&source).unwrap(),
        provider: Provider::Gemini,
        model: "test".into(),
        audio_model: None,
        proposed_name: String::new(),
        media: media::probe(&source).unwrap(),
        analysis: Analysis::default(),
    };
    let moment = Moment {
        start_seconds: 0.0,
        end_seconds: 0.5,
        description: "Highlight".into(),
        kept: false,
        exported_to: None,
    };
    let preserved = directory.path().join("preserved.mp4");
    segments::export(&clip, &moment, &preserved).unwrap();
    assert_eq!(fs::metadata(&preserved).unwrap().modified().unwrap(), old);
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format_tags=creation_time",
            "-of",
            "json",
        ])
        .arg(&preserved)
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["format"]["tags"]["creation_time"],
        "2001-01-01T00:00:00.000000Z"
    );
    let fresh = directory.path().join("fresh.mp4");
    segments::export_group(
        &clip,
        &[moment],
        std::slice::from_ref(&fresh),
        segments::ExportOptions {
            replace_original: false,
            preserve_times: false,
        },
        &AtomicBool::new(false),
        None,
    )
    .unwrap();
    assert_ne!(fs::metadata(fresh).unwrap().modified().unwrap(), old);
    assert!(source.exists());
    assert_eq!(media::fingerprint(&source).unwrap(), clip.sha256);
}
