use anyhow::{Context, Result, bail};
use fractal_renderer_core::{
    AnimationFrame, DistanceEstimator, FractalConfig, LoadedScene, PathTarget, TargetPicker,
    TargetSearchConfig,
};
use serde::{Deserialize, Serialize};

use crate::ProjectStore;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RouteInspectRequest {
    pub project_id: String,
    #[serde(default)]
    pub revision_id: Option<String>,
    #[serde(default)]
    pub frames: Vec<u32>,
    #[serde(default = "default_route_sample_count")]
    pub validation_samples: u32,
    #[serde(default = "default_clearance_ratio")]
    pub minimum_clearance_ratio: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RouteSample {
    pub frame_index: u32,
    pub time_seconds: f64,
    pub position: [f64; 3],
    pub target: [f64; 3],
    pub up: [f32; 3],
    pub camera_distance: f64,
    pub camera_clearance: f64,
    pub clearance_ratio: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RouteInspection {
    pub project_id: String,
    pub revision_id: String,
    pub frame_count: u32,
    pub representative_samples: Vec<RouteSample>,
    pub minimum_clearance_ratio: f64,
    pub minimum_clearance_frame: u32,
    pub valid: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetSearchMode {
    #[default]
    Best,
    Random,
    OriginGap,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TargetSearchRequest {
    pub project_id: String,
    #[serde(default)]
    pub revision_id: Option<String>,
    #[serde(default = "default_target_candidate_count")]
    pub candidate_count: u32,
    #[serde(default)]
    pub seed: Option<u32>,
    #[serde(default)]
    pub mode: TargetSearchMode,
    #[serde(default = "default_approach_direction")]
    pub approach_direction: [f64; 3],
    #[serde(default)]
    pub bound_radius: Option<f64>,
    #[serde(default)]
    pub hit_epsilon: Option<f64>,
    #[serde(default)]
    pub max_steps: Option<u32>,
    #[serde(default)]
    pub attempts: Option<u32>,
    #[serde(default)]
    pub aim_jitter: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TargetCandidate {
    pub id: String,
    pub seed: u32,
    pub point: [f64; 3],
    pub normal: [f64; 3],
    pub view_direction: [f64; 3],
    pub suggested_camera_position: [f64; 3],
    pub camera_clearance: f64,
    pub lighting_alignment: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TargetSearchResult {
    pub project_id: String,
    pub revision_id: String,
    pub search: TargetSearchConfigReport,
    pub candidates: Vec<TargetCandidate>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TargetSearchConfigReport {
    pub bound_radius: f64,
    pub hit_epsilon: f64,
    pub max_steps: u32,
    pub attempts: u32,
    pub aim_jitter: f64,
}

pub fn inspect_route(
    store: &ProjectStore,
    request: &RouteInspectRequest,
) -> Result<RouteInspection> {
    if request.validation_samples < 2 || request.validation_samples > 10_000 {
        bail!("route validation_samples must be in 2..=10000");
    }
    if !request.minimum_clearance_ratio.is_finite() || request.minimum_clearance_ratio < 0.0 {
        bail!("route minimum_clearance_ratio must be finite and non-negative");
    }
    let (revision, spec) = store.scene(&request.project_id, request.revision_id.as_deref())?;
    let scene = spec.resolve()?;
    let frame_count = scene
        .animation
        .as_ref()
        .map_or(1, |animation| animation.frame_count);
    let representative_frames = if request.frames.is_empty() {
        representative_frames(frame_count)
    } else {
        validate_frames(&request.frames, frame_count)?
    };
    let representative_samples = representative_frames
        .into_iter()
        .map(|frame| sample_route(&scene, frame))
        .collect::<Result<Vec<_>>>()?;
    let validation_frames = evenly_spaced_frames(frame_count, request.validation_samples);
    let mut minimum = (f64::INFINITY, 0_u32);
    for frame in validation_frames {
        let sample = sample_route(&scene, frame)?;
        if sample.clearance_ratio < minimum.0 {
            minimum = (sample.clearance_ratio, frame);
        }
    }
    let valid = minimum.0 >= request.minimum_clearance_ratio;
    let warnings = if valid {
        Vec::new()
    } else {
        vec![format!(
            "camera clearance ratio reaches {:.6e} at frame {}, below requested {:.6e}",
            minimum.0, minimum.1, request.minimum_clearance_ratio
        )]
    };
    Ok(RouteInspection {
        project_id: request.project_id.clone(),
        revision_id: revision.id,
        frame_count,
        representative_samples,
        minimum_clearance_ratio: minimum.0,
        minimum_clearance_frame: minimum.1,
        valid,
        warnings,
    })
}

pub fn search_targets(
    store: &ProjectStore,
    request: &TargetSearchRequest,
) -> Result<TargetSearchResult> {
    if request.candidate_count == 0 || request.candidate_count > 64 {
        bail!("target candidate_count must be in 1..=64");
    }
    let (revision, spec) = store.scene(&request.project_id, request.revision_id.as_deref())?;
    let scene = spec.resolve()?;
    if scene.config.precision != fractal_renderer_core::Precision::F32 {
        bail!("target candidate search currently requires f32 precision");
    }
    let default_bound = match &scene.config.fractal {
        FractalConfig::Mandelbox(config) => config.bound_radius,
        FractalConfig::Mandelbulb(_) => 4.0,
        FractalConfig::Dsl(_) => 4.2,
    };
    let search = TargetSearchConfig {
        bound_radius: request.bound_radius.unwrap_or(default_bound),
        hit_epsilon: request
            .hit_epsilon
            .unwrap_or(f64::from(scene.config.render.epsilon).max(1.0e-7)),
        max_steps: request
            .max_steps
            .unwrap_or(scene.config.render.max_steps.max(800)),
        attempts: request.attempts.unwrap_or(128),
        aim_jitter: request.aim_jitter.unwrap_or(0.25),
    };
    search.validate()?;
    let total_attempts = u64::from(request.candidate_count) * u64::from(search.attempts);
    if total_attempts > 4_096 {
        bail!(
            "target search requests {total_attempts} candidate attempts, bounded maximum is 4096"
        );
    }
    let base_seed = request.seed.unwrap_or(scene.config.seed);
    let camera_distance = (scene.config.camera.target - scene.config.camera.position)
        .length_squared()
        .sqrt()
        .to_f64();
    let light = scene.config.light.direction.map(f64::from);
    let mut candidates = Vec::with_capacity(request.candidate_count as usize);
    for index in 0..request.candidate_count {
        let seed = base_seed.wrapping_add(index.wrapping_add(1).wrapping_mul(0x9e37_79b9));
        let target = match &scene.config.fractal {
            FractalConfig::Mandelbulb(config) => find_target(
                config,
                search,
                light,
                seed,
                request.mode,
                request.approach_direction,
            ),
            FractalConfig::Mandelbox(config) => find_target(
                config,
                search,
                light,
                seed,
                request.mode,
                request.approach_direction,
            ),
            FractalConfig::Dsl(config) => find_target(
                config,
                search,
                light,
                seed,
                request.mode,
                request.approach_direction,
            ),
        };
        let Some((target, normal)) = target else {
            continue;
        };
        let point = target.point.to_f64();
        let suggested_camera_position =
            subtract(point, scale(target.view_direction, camera_distance));
        let camera_clearance =
            distance_estimate(&scene.config.fractal, suggested_camera_position).abs();
        candidates.push(TargetCandidate {
            id: format!("target-{:03}", index + 1),
            seed,
            point,
            normal,
            view_direction: target.view_direction,
            suggested_camera_position,
            camera_clearance,
            lighting_alignment: dot(normal, light).max(0.0),
        });
    }
    let warnings = if candidates.len() < request.candidate_count as usize {
        vec![format!(
            "target search produced {}/{} requested candidates",
            candidates.len(),
            request.candidate_count
        )]
    } else {
        Vec::new()
    };
    Ok(TargetSearchResult {
        project_id: request.project_id.clone(),
        revision_id: revision.id,
        search: TargetSearchConfigReport {
            bound_radius: search.bound_radius,
            hit_epsilon: search.hit_epsilon,
            max_steps: search.max_steps,
            attempts: search.attempts,
            aim_jitter: search.aim_jitter,
        },
        candidates,
        warnings,
    })
}

fn find_target<D: DistanceEstimator>(
    estimator: &D,
    search: TargetSearchConfig,
    light: [f64; 3],
    seed: u32,
    mode: TargetSearchMode,
    approach_direction: [f64; 3],
) -> Option<(PathTarget, [f64; 3])> {
    let picker = TargetPicker::new(estimator, search, light);
    let target = match mode {
        TargetSearchMode::Best => picker.pick_best(seed),
        TargetSearchMode::Random => picker.pick_random(seed),
        TargetSearchMode::OriginGap => picker.pick_origin_gap(seed, approach_direction),
    }?;
    let normal = picker
        .surface_normal(target.point.to_f64(), search.hit_epsilon * 8.0)
        .unwrap_or_else(|| scale(target.view_direction, -1.0));
    Some((target, normal))
}

fn sample_route(scene: &LoadedScene, frame_index: u32) -> Result<RouteSample> {
    let frame = sample_frame(scene, frame_index)?;
    let position = frame.config.camera.position.to_f64();
    let target = frame.config.camera.target.to_f64();
    let camera_distance = frame.camera_distance.to_f64();
    let camera_clearance = distance_estimate(&frame.config.fractal, position).abs();
    Ok(RouteSample {
        frame_index,
        time_seconds: frame.time_seconds,
        position,
        target,
        up: frame.config.camera.up,
        camera_distance,
        camera_clearance,
        clearance_ratio: camera_clearance / camera_distance.max(1.0e-30),
    })
}

fn sample_frame(scene: &LoadedScene, frame_index: u32) -> Result<AnimationFrame> {
    if let Some(animation) = &scene.animation {
        return animation
            .sample(&scene.config, frame_index)
            .with_context(|| format!("could not sample route frame {frame_index}"));
    }
    if frame_index != 0 {
        bail!("static scene has only frame 0");
    }
    let distance = (scene.config.camera.target - scene.config.camera.position)
        .length_squared()
        .sqrt();
    Ok(AnimationFrame {
        index: 0,
        time_seconds: 0.0,
        camera_distance: distance,
        config: scene.config.clone(),
    })
}

fn distance_estimate(fractal: &FractalConfig, point: [f64; 3]) -> f64 {
    match fractal {
        FractalConfig::Mandelbulb(config) => config.distance_estimate(point),
        FractalConfig::Mandelbox(config) => config.distance_estimate(point),
        FractalConfig::Dsl(config) => config.distance_estimate(point),
    }
}

fn scale(value: [f64; 3], factor: f64) -> [f64; 3] {
    [value[0] * factor, value[1] * factor, value[2] * factor]
}

fn subtract(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn dot(left: [f64; 3], right: [f64; 3]) -> f64 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn representative_frames(frame_count: u32) -> Vec<u32> {
    if frame_count == 1 {
        return vec![0];
    }
    let last = frame_count - 1;
    vec![0, last / 4, last / 2, last.saturating_mul(3) / 4, last]
}

fn validate_frames(frames: &[u32], frame_count: u32) -> Result<Vec<u32>> {
    let mut frames = frames.to_vec();
    frames.sort_unstable();
    frames.dedup();
    if let Some(frame) = frames.iter().find(|&&frame| frame >= frame_count) {
        bail!(
            "route frame {frame} is outside this scene's frame range 0..{}",
            frame_count - 1
        );
    }
    Ok(frames)
}

fn evenly_spaced_frames(frame_count: u32, sample_count: u32) -> Vec<u32> {
    if frame_count == 1 {
        return vec![0];
    }
    let count = sample_count.min(frame_count);
    let last = u64::from(frame_count - 1);
    let denominator = u64::from(count - 1);
    (0..count)
        .map(|index| ((u64::from(index) * last + denominator / 2) / denominator) as u32)
        .collect()
}

const fn default_route_sample_count() -> u32 {
    121
}

const fn default_clearance_ratio() -> f64 {
    1.0e-4
}

const fn default_target_candidate_count() -> u32 {
    5
}

const fn default_approach_direction() -> [f64; 3] {
    [1.0, 0.0, 0.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_samples_include_route_endpoints() {
        let frames = evenly_spaced_frames(721, 7);
        assert_eq!(frames.first(), Some(&0));
        assert_eq!(frames.last(), Some(&720));
    }
}
