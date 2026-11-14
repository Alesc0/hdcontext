use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path, process::Command};

#[derive(Debug)]
pub struct VideoFrame {
    pub seconds: f64,
    pub jpeg: Vec<u8>,
}

pub struct SampledMedia {
    pub frames: Vec<VideoFrame>,
    pub audio_wav: Option<Vec<u8>>,
}

pub fn frame_stride(duration: f64) -> usize {
    ((duration * 8.0 / 120.0).ceil() as usize).max(4)
}

pub fn clip_timecode(seconds: f64) -> String {
    let milliseconds = (seconds.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        milliseconds / 3_600_000,
        milliseconds / 60_000 % 60,
        milliseconds / 1000 % 60,
        milliseconds % 1000
    )
}

pub fn analysis_video_filter() -> &'static str {
    // The clock belongs to the clip timeline. A separate top band keeps it distinct from the game's HUD clock.
    "fps=8:start_time=0,scale=w='min(960,iw)':h='min(540,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2,pad=iw:ih+64:0:64:color=black,drawtext=font=monospace:text='CLIP_TIME %{pts\\:hms}':fontcolor=white:fontsize='min(40,w/16)':x=10:y=12"
}

/// Sample a prepared, constant-8-fps MP4 across its complete timeline.
pub fn sample_prepared(source: &Path, directory: &Path, info: &MediaInfo) -> Result<SampledMedia> {
    std::fs::create_dir_all(directory)?;
    let stride = frame_stride(info.duration_seconds);
    let filter = format!(
        "select=not(mod(n\\,{stride})),scale=w='min(768,iw)':h='min(432,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2"
    );
    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-n", "-i"])
        .arg(source)
        .args([
            "-map",
            "0:V:0",
            "-vf",
            &filter,
            "-fps_mode",
            "vfr",
            "-frames:v",
            "120",
            "-q:v",
            "4",
        ])
        .arg(directory.join("frame-%03d.jpg"))
        .output()
        .context("Could not extract video frames with ffmpeg")?;
    ensure!(
        output.status.success(),
        "Frame extraction failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut paths = std::fs::read_dir(directory)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.retain(|p| p.extension().is_some_and(|e| e == "jpg"));
    paths.sort();
    let frames = paths
        .into_iter()
        .enumerate()
        .map(|(index, path)| {
            Ok(VideoFrame {
                seconds: index as f64 * stride as f64 / 8.0,
                jpeg: std::fs::read(path)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(!frames.is_empty(), "No frames could be extracted");
    ensure!(
        frames.iter().map(|f| f.jpeg.len()).sum::<usize>() <= 20_000_000,
        "Sampled images exceed the 20 MB limit. Use a shorter clip"
    );
    let audio_wav = if info.has_audio {
        let audio = directory.join("audio.wav");
        let output = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-n", "-i"])
            .arg(source)
            .args([
                "-map",
                "0:a:0",
                "-vn",
                "-c:a",
                "pcm_s16le",
                "-ar",
                "16000",
                "-ac",
                "1",
            ])
            .arg(&audio)
            .output()
            .context("Could not extract audio with ffmpeg")?;
        ensure!(
            output.status.success(),
            "Audio extraction failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        ensure!(
            audio.metadata()?.len() <= 16_000_000,
            "Extracted audio exceeds the 16 MB limit. Use a shorter clip"
        );
        Some(std::fs::read(audio)?)
    } else {
        None
    };
    Ok(SampledMedia { frames, audio_wav })
}

// Base64 and request metadata must also fit under the API's 20 MB request limit.
pub const MAX_MEDIA_BYTES: u64 = 14_000_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaInfo {
    pub duration_seconds: f64,
    pub has_audio: bool,
    pub audio_tracks: usize,
}

pub fn probe(path: &Path) -> Result<MediaInfo> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_format",
            "-show_streams",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .context("Could not start ffprobe. Install FFmpeg and put ffprobe on PATH")?;
    ensure!(
        output.status.success(),
        "ffprobe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let streams = value["streams"]
        .as_array()
        .context("No media streams found")?;
    ensure!(
        streams
            .iter()
            .any(|s| s["codec_type"] == "video" && s["disposition"]["attached_pic"] != 1),
        "File has no video stream"
    );
    let duration = value["format"]["duration"]
        .as_str()
        .context("Video duration is unknown")?
        .parse::<f64>()?;
    ensure!(
        duration.is_finite() && duration > 0.0,
        "Invalid video duration"
    );
    let audio_tracks = streams
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .count();
    Ok(MediaInfo {
        duration_seconds: duration,
        has_audio: audio_tracks > 0,
        audio_tracks,
    })
}

pub fn prepare(source: &Path, destination: &Path, info: &MediaInfo) -> Result<()> {
    // Keep the entire timeline. Mix every audio track, including separate microphone tracks.
    let mut command = Command::new("ffmpeg");
    command
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-n", "-i"])
        .arg(source)
        .args(["-map", "0:V:0"]);
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
    command
        .args([
            "-vf",
            analysis_video_filter(),
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-b:v",
            "280k",
            "-maxrate",
            "350k",
            "-bufsize",
            "700k",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "48k",
            "-ac",
            "1",
            "-ar",
            "24000",
            "-map_metadata",
            "-1",
            "-map_chapters",
            "-1",
            "-movflags",
            "+faststart",
        ])
        .arg(destination);
    let output = command
        .output()
        .context("Could not start ffmpeg. Install FFmpeg with libx264 and AAC support")?;
    if !output.status.success() {
        bail!("ffmpeg failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let size = destination.metadata()?.len();
    ensure!(
        size > 0 && size <= MAX_MEDIA_BYTES,
        "Compressed video is too large ({size} bytes). Use a shorter clip; maximum compressed size is {MAX_MEDIA_BYTES} bytes"
    );
    Ok(())
}

pub fn fingerprint(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("Cannot read {}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
