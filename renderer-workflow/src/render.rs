use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, Result, bail};
use image::RgbaImage;
use serde::{Deserialize, Serialize};

use crate::{
    Artifact, FrameRenderSession, ProjectStore, RenderEnvironment, RendererPolicy,
    artifact::{save_png_atomic, write_atomic},
    project::unix_time_ms,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RenderRequest {
    pub project_id: String,
    #[serde(default)]
    pub revision_id: Option<String>,
    #[serde(default)]
    pub start_frame: Option<u32>,
    #[serde(default)]
    pub end_frame: Option<u32>,
    #[serde(default)]
    pub resume: bool,
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default)]
    pub gpu_duty_cycle: Option<f64>,
    #[serde(default)]
    pub allow_software: bool,
    #[serde(default)]
    pub adapter: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SequenceManifest {
    pub version: u32,
    pub project_id: String,
    pub revision_id: String,
    pub scene_hash: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub frame_count: u32,
    pub created_unix_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RenderResult {
    pub project_id: String,
    pub revision_id: String,
    pub sequence: SequenceManifest,
    pub environment: Option<RenderEnvironment>,
    pub frames_directory: PathBuf,
    pub rendered_frames: Vec<u32>,
    pub skipped_frames: Vec<u32>,
    pub sequence_artifact: Artifact,
    pub total_seconds: f64,
}

pub fn render_sequence(
    store: &ProjectStore,
    request: &RenderRequest,
    frames_directory: &Path,
) -> Result<RenderResult> {
    render_sequence_with_progress(store, request, frames_directory, &mut |_, _| Ok(()))
}

pub(crate) fn render_sequence_with_progress(
    store: &ProjectStore,
    request: &RenderRequest,
    frames_directory: &Path,
    progress: &mut dyn FnMut(u32, u32) -> Result<()>,
) -> Result<RenderResult> {
    validate_request(request)?;
    let (revision, spec) = store.scene(&request.project_id, request.revision_id.as_deref())?;
    let scene = spec.resolve()?;
    let frame_count = scene
        .animation
        .as_ref()
        .map_or(1, |animation| animation.frame_count);
    let fps = scene
        .animation
        .as_ref()
        .map_or(1, |animation| animation.fps);
    let start_frame = request.start_frame.unwrap_or(0);
    let end_frame = request.end_frame.unwrap_or(frame_count - 1);
    if start_frame > end_frame || end_frame >= frame_count {
        bail!(
            "render frame range {start_frame}..={end_frame} is outside 0..{}",
            frame_count - 1
        );
    }
    let expected_manifest = SequenceManifest {
        version: 1,
        project_id: request.project_id.clone(),
        revision_id: revision.id.clone(),
        scene_hash: revision.scene_hash.clone(),
        width: scene.config.render.width,
        height: scene.config.render.height,
        fps,
        frame_count,
        created_unix_ms: unix_time_ms(),
    };
    fs::create_dir_all(frames_directory).with_context(|| {
        format!(
            "could not create render output {}",
            frames_directory.display()
        )
    })?;
    let manifest_path = frames_directory.join("sequence.json");
    let sequence = prepare_sequence_manifest(
        &manifest_path,
        &expected_manifest,
        request.resume,
        request.overwrite,
    )?;

    let requested = (start_frame..=end_frame).collect::<Vec<_>>();
    let mut pending = Vec::new();
    let mut skipped = Vec::new();
    for frame_index in requested {
        let path = frame_path(frames_directory, frame_index);
        if !path.exists() || request.overwrite {
            pending.push(frame_index);
        } else if request.resume {
            validate_png(&path, sequence.width, sequence.height)?;
            skipped.push(frame_index);
        } else {
            bail!(
                "output frame {} already exists; enable resume or overwrite",
                path.display()
            );
        }
    }

    let total_started = Instant::now();
    let mut environment = None;
    if !pending.is_empty() {
        let mut session = FrameRenderSession::new(RendererPolicy {
            allow_software: request.allow_software,
            adapter: request.adapter.clone(),
            gpu_duty_cycle: request.gpu_duty_cycle,
        })?;
        session
            .render_frames(
                &scene,
                &pending,
                None,
                |start| {
                    environment = Some(start.environment.clone());
                    Ok(())
                },
                &mut *progress,
                |rendered| {
                    let image = RgbaImage::from_raw(
                        rendered.image.width(),
                        rendered.image.height(),
                        rendered.image.pixels().to_vec(),
                    )
                    .context("renderer returned an invalid RGBA image")?;
                    save_png_atomic(&frame_path(frames_directory, rendered.frame.index), &image)
                },
            )
            .context("could not execute final render frames")?;
    }

    let mut sequence_artifact =
        Artifact::from_file("frame-sequence-manifest", "application/json", manifest_path)?;
    sequence_artifact.metadata.insert(
        "frames_directory".to_owned(),
        serde_json::Value::String(frames_directory.display().to_string()),
    );
    sequence_artifact.metadata.insert(
        "frame_count".to_owned(),
        serde_json::json!(sequence.frame_count),
    );
    Ok(RenderResult {
        project_id: request.project_id.clone(),
        revision_id: revision.id,
        sequence,
        environment,
        frames_directory: frames_directory.to_owned(),
        rendered_frames: pending,
        skipped_frames: skipped,
        sequence_artifact,
        total_seconds: total_started.elapsed().as_secs_f64(),
    })
}

pub fn read_sequence_manifest(frames_directory: &Path) -> Result<SequenceManifest> {
    let path = frames_directory.join("sequence.json");
    let bytes = fs::read(&path)
        .with_context(|| format!("could not read sequence manifest {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("sequence manifest {} is invalid", path.display()))
}

pub fn frame_path(directory: &Path, frame_index: u32) -> PathBuf {
    directory.join(format!("frame_{frame_index:06}.png"))
}

pub fn validate_png(path: &Path, width: u32, height: u32) -> Result<()> {
    let image = image::open(path)
        .with_context(|| format!("could not decode sequence frame {}", path.display()))?;
    if image.width() != width || image.height() != height {
        bail!(
            "sequence frame {} is {}x{}, expected {}x{}",
            path.display(),
            image.width(),
            image.height(),
            width,
            height
        );
    }
    Ok(())
}

fn validate_request(request: &RenderRequest) -> Result<()> {
    if request.resume && request.overwrite {
        bail!("render resume and overwrite are mutually exclusive");
    }
    if let Some(percent) = request.gpu_duty_cycle
        && (!percent.is_finite() || !(1.0..=100.0).contains(&percent))
    {
        bail!("render gpu_duty_cycle must be in 1.0..=100.0 percent");
    }
    Ok(())
}

fn prepare_sequence_manifest(
    path: &Path,
    expected: &SequenceManifest,
    resume: bool,
    overwrite: bool,
) -> Result<SequenceManifest> {
    if path.exists() && !overwrite {
        let bytes = fs::read(path)?;
        let actual: SequenceManifest = serde_json::from_slice(&bytes)
            .with_context(|| format!("sequence manifest {} is invalid", path.display()))?;
        if !resume {
            bail!(
                "sequence manifest {} already exists; enable resume or overwrite",
                path.display()
            );
        }
        if actual.project_id != expected.project_id
            || actual.revision_id != expected.revision_id
            || actual.scene_hash != expected.scene_hash
            || actual.width != expected.width
            || actual.height != expected.height
            || actual.fps != expected.fps
            || actual.frame_count != expected.frame_count
        {
            bail!(
                "sequence manifest does not match revision {}; refusing to mix frames",
                expected.revision_id
            );
        }
        return Ok(actual);
    }
    let bytes = serde_json::to_vec_pretty(expected)?;
    write_atomic(path, &bytes)?;
    Ok(expected.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_manifest_rejects_a_different_revision() {
        let directory = std::env::temp_dir().join(format!(
            "fractal-sequence-manifest-{}-{}",
            std::process::id(),
            unix_time_ms()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("sequence.json");
        let base = SequenceManifest {
            version: 1,
            project_id: "alchemy".to_owned(),
            revision_id: "rev-a".to_owned(),
            scene_hash: "aaa".to_owned(),
            width: 100,
            height: 50,
            fps: 30,
            frame_count: 10,
            created_unix_ms: 1,
        };
        prepare_sequence_manifest(&path, &base, false, false).unwrap();
        let changed = SequenceManifest {
            revision_id: "rev-b".to_owned(),
            scene_hash: "bbb".to_owned(),
            ..base
        };
        assert!(prepare_sequence_manifest(&path, &changed, true, false).is_err());
        fs::remove_dir_all(directory).unwrap();
    }
}
