use std::{
    ffi::OsString,
    fs,
    fs::File,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use fractal_renderer_core::VideoConfig;
use serde::{Deserialize, Serialize};

use crate::{
    Artifact, ProjectStore,
    render::{SequenceManifest, frame_path, read_sequence_manifest, validate_png},
};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SequenceSelection {
    #[default]
    Complete,
    Range {
        start_frame: u32,
        end_frame: u32,
    },
    Available {
        start_frame: u32,
    },
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct VideoOverrides {
    pub codec: Option<String>,
    pub pixel_format: Option<String>,
    pub crf: Option<u8>,
    pub preset: Option<String>,
    pub faststart: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EncodeRequest {
    pub project_id: String,
    pub source_job_id: String,
    #[serde(default)]
    pub selection: SequenceSelection,
    #[serde(default)]
    pub output_name: Option<String>,
    #[serde(default)]
    pub video: VideoOverrides,
    #[serde(default)]
    pub ffmpeg: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EncodeResult {
    pub project_id: String,
    pub revision_id: String,
    pub source_job_id: String,
    pub start_frame: u32,
    pub frame_count: u32,
    pub output: Artifact,
    pub total_seconds: f64,
}

pub(crate) fn encode_sequence(
    store: &ProjectStore,
    request: &EncodeRequest,
    frames_directory: &Path,
    output_directory: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<EncodeResult> {
    let sequence = read_sequence_manifest(frames_directory)?;
    validate_source(store, request, &sequence)?;
    let (_, spec) = store.scene(&request.project_id, Some(&sequence.revision_id))?;
    let scene = spec.resolve()?;
    let mut video = scene.video.unwrap_or_default();
    apply_overrides(&mut video, &request.video);
    video
        .validate_dimensions(sequence.width, sequence.height)
        .context("invalid effective video configuration")?;
    let (start_frame, end_frame) = select_frames(frames_directory, &sequence, &request.selection)?;
    for frame_index in start_frame..=end_frame {
        validate_png(
            &frame_path(frames_directory, frame_index),
            sequence.width,
            sequence.height,
        )?;
    }
    if cancelled() {
        bail!("encode cancelled before FFmpeg started");
    }

    fs::create_dir_all(output_directory)?;
    let output_name = request
        .output_name
        .clone()
        .unwrap_or_else(|| default_output_name(&sequence, start_frame, end_frame));
    validate_output_name(&output_name)?;
    let output = output_directory.join(output_name);
    if output.exists() {
        bail!("encode output {} already exists", output.display());
    }
    let temporary = temporary_video_path(&output)?;
    let ffmpeg_log = output_directory.join("ffmpeg.stderr.log");
    let stderr = File::create(&ffmpeg_log)
        .with_context(|| format!("could not create {}", ffmpeg_log.display()))?;
    let ffmpeg = request
        .ffmpeg
        .clone()
        .unwrap_or_else(|| PathBuf::from("ffmpeg"));
    verify_ffmpeg(&ffmpeg)?;
    let frame_count = end_frame - start_frame + 1;
    let started = Instant::now();
    let mut child = Command::new(&ffmpeg)
        .args(ffmpeg_arguments(
            frames_directory,
            &temporary,
            sequence.fps,
            start_frame,
            frame_count,
            &video,
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr))
        .spawn()
        .with_context(|| format!("could not execute {}", ffmpeg.display()))?;
    loop {
        if let Some(status) = child
            .try_wait()
            .context("could not inspect FFmpeg status")?
        {
            if !status.success() {
                let _ = fs::remove_file(&temporary);
                let diagnostic = fs::read_to_string(&ffmpeg_log).unwrap_or_default();
                bail!(
                    "FFmpeg failed with status {status}: {}",
                    diagnostic.trim().chars().take(2_000).collect::<String>()
                );
            }
            break;
        }
        if cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(&temporary);
            let _ = fs::remove_file(&ffmpeg_log);
            bail!("encode cancelled while FFmpeg was running");
        }
        thread::sleep(Duration::from_millis(100));
    }
    fs::rename(&temporary, &output)
        .with_context(|| format!("could not publish video {}", output.display()))?;
    let _ = fs::remove_file(&ffmpeg_log);
    let mut artifact = Artifact::from_file("video", video_media_type(&output), output)?;
    artifact.metadata.insert(
        "selection".to_owned(),
        serde_json::json!({"start_frame": start_frame, "end_frame": end_frame}),
    );
    Ok(EncodeResult {
        project_id: request.project_id.clone(),
        revision_id: sequence.revision_id,
        source_job_id: request.source_job_id.clone(),
        start_frame,
        frame_count,
        output: artifact,
        total_seconds: started.elapsed().as_secs_f64(),
    })
}

fn validate_source(
    store: &ProjectStore,
    request: &EncodeRequest,
    sequence: &SequenceManifest,
) -> Result<()> {
    if sequence.project_id != request.project_id {
        bail!(
            "source sequence belongs to project {}, not {}",
            sequence.project_id,
            request.project_id
        );
    }
    let (revision, _) = store.scene(&request.project_id, Some(&sequence.revision_id))?;
    if revision.scene_hash != sequence.scene_hash {
        bail!("source sequence scene hash does not match its project revision");
    }
    Ok(())
}

fn apply_overrides(config: &mut VideoConfig, overrides: &VideoOverrides) {
    if let Some(codec) = &overrides.codec {
        config.codec.clone_from(codec);
    }
    if let Some(pixel_format) = &overrides.pixel_format {
        config.pixel_format.clone_from(pixel_format);
    }
    if let Some(crf) = overrides.crf {
        config.crf = crf;
    }
    if let Some(preset) = &overrides.preset {
        config.preset.clone_from(preset);
    }
    if let Some(faststart) = overrides.faststart {
        config.faststart = faststart;
    }
}

fn select_frames(
    frames_directory: &Path,
    sequence: &SequenceManifest,
    selection: &SequenceSelection,
) -> Result<(u32, u32)> {
    match selection {
        SequenceSelection::Complete => {
            let end = sequence.frame_count - 1;
            for frame in 0..=end {
                if !frame_path(frames_directory, frame).exists() {
                    bail!("complete encode requires missing frame {frame}");
                }
            }
            Ok((0, end))
        }
        SequenceSelection::Range {
            start_frame,
            end_frame,
        } => {
            if start_frame > end_frame || *end_frame >= sequence.frame_count {
                bail!("encode range is outside the sequence frame count");
            }
            for frame in *start_frame..=*end_frame {
                if !frame_path(frames_directory, frame).exists() {
                    bail!("encode range is missing frame {frame}");
                }
            }
            Ok((*start_frame, *end_frame))
        }
        SequenceSelection::Available { start_frame } => {
            if *start_frame >= sequence.frame_count {
                bail!("available encode start frame is outside the sequence");
            }
            let mut end = None;
            for frame in *start_frame..sequence.frame_count {
                if !frame_path(frames_directory, frame).exists() {
                    break;
                }
                end = Some(frame);
            }
            let end = end.context("no contiguous frames are available for encoding")?;
            Ok((*start_frame, end))
        }
    }
}

fn verify_ffmpeg(ffmpeg: &Path) -> Result<()> {
    let status = Command::new(ffmpeg)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("could not execute {}", ffmpeg.display()))?;
    if !status.success() {
        bail!("{} -version failed with status {status}", ffmpeg.display());
    }
    Ok(())
}

fn ffmpeg_arguments(
    frames_directory: &Path,
    temporary: &Path,
    fps: u32,
    start_frame: u32,
    frame_count: u32,
    config: &VideoConfig,
) -> Vec<OsString> {
    let mut arguments = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "warning".into(),
        "-y".into(),
        "-framerate".into(),
        fps.to_string().into(),
        "-start_number".into(),
        start_frame.to_string().into(),
        "-i".into(),
        frames_directory.join("frame_%06d.png").into_os_string(),
        "-frames:v".into(),
        frame_count.to_string().into(),
        "-an".into(),
        "-c:v".into(),
        config.codec.clone().into(),
        "-pix_fmt".into(),
        config.pixel_format.clone().into(),
        "-crf".into(),
        config.crf.to_string().into(),
        "-preset".into(),
        config.preset.clone().into(),
    ];
    if config.faststart {
        arguments.extend([OsString::from("-movflags"), OsString::from("+faststart")]);
    }
    arguments.push(temporary.as_os_str().to_owned());
    arguments
}

fn default_output_name(sequence: &SequenceManifest, start: u32, end: u32) -> String {
    if start == 0 && end + 1 == sequence.frame_count {
        format!("{}.mp4", sequence.revision_id)
    } else {
        format!("{}-frames-{start}-{end}.mp4", sequence.revision_id)
    }
}

fn validate_output_name(name: &str) -> Result<()> {
    let path = Path::new(name);
    if name.is_empty()
        || path.extension().is_none()
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        bail!("encode output_name must be one file name with a container extension");
    }
    Ok(())
}

fn temporary_video_path(output: &Path) -> Result<PathBuf> {
    let stem = output
        .file_stem()
        .context("video output must have a file stem")?
        .to_string_lossy();
    let extension = output
        .extension()
        .context("video output must have an extension")?
        .to_string_lossy();
    Ok(output.with_file_name(format!(".{stem}.{}.tmp.{extension}", std::process::id())))
}

fn video_media_type(output: &Path) -> &'static str {
    match output.extension().and_then(|extension| extension.to_str()) {
        Some("webm") => "video/webm",
        Some("mov") => "video/quicktime",
        _ => "video/mp4",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::unix_time_ms;

    #[test]
    fn available_selection_stops_at_first_gap() {
        let directory = std::env::temp_dir().join(format!(
            "fractal-partial-encode-{}-{}",
            std::process::id(),
            unix_time_ms()
        ));
        fs::create_dir_all(&directory).unwrap();
        for frame in 0..3 {
            fs::write(frame_path(&directory, frame), b"placeholder").unwrap();
        }
        let sequence = SequenceManifest {
            version: 1,
            project_id: "p".to_owned(),
            revision_id: "rev-a".to_owned(),
            scene_hash: "hash".to_owned(),
            width: 1,
            height: 1,
            fps: 30,
            frame_count: 10,
            created_unix_ms: 0,
        };
        assert_eq!(
            select_frames(
                &directory,
                &sequence,
                &SequenceSelection::Available { start_frame: 0 }
            )
            .unwrap(),
            (0, 2)
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
