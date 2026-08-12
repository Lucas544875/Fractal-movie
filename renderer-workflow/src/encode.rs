use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use fractal_renderer_core::VideoConfig;
use serde::{Deserialize, Serialize};

use crate::{
    Artifact, ProjectStore, VideoEncodeJob,
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
    let ffmpeg_log = output_directory.join("ffmpeg.stderr.log");
    let ffmpeg = request
        .ffmpeg
        .clone()
        .unwrap_or_else(|| PathBuf::from("ffmpeg"));
    let frame_count = end_frame - start_frame + 1;
    let job = VideoEncodeJob {
        ffmpeg,
        frames_directory: frames_directory.to_owned(),
        output_path: output,
        fps: sequence.fps,
        start_frame,
        frame_count,
        config: video,
        overwrite: false,
        diagnostic_log: Some(ffmpeg_log),
        show_progress: false,
    };
    job.validate(sequence.width, sequence.height)
        .context("invalid effective video configuration")?;
    job.ffmpeg_version()
        .context("FFmpeg is unavailable for the requested encode")?;
    let summary = job.encode_with_cancel(cancelled)?;
    let mut artifact = Artifact::from_file(
        "video",
        video_media_type(&summary.output_path),
        summary.output_path,
    )?;
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
        total_seconds: summary.total_seconds,
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
