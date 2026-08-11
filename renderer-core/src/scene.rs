use crate::{
    CameraConfig, ExponentialDivePath, FractalConfig, LightConfig, MandelboxConfig, PathTarget,
    RenderConfig, RenderSettings, TargetPicker, TargetSearchConfig,
};

const MANDELBOX_LIGHT: [f32; 3] = [2.0, 1.0, 1.0];
const FALLBACK_TARGET: PathTarget = PathTarget {
    point: [2.212_921, 1.099_011, 0.307_275],
    view_direction: [-0.836_792, 0.260_298, -0.481_689],
};

impl RenderConfig {
    /// Portfolio Mandelbox preset, including the deterministic CPU target
    /// search used by the JavaScript page's opening shot.
    #[must_use]
    pub fn mandelbox(seed: u32) -> Self {
        let fractal = MandelboxConfig::default();
        let picker = TargetPicker::new(
            &fractal,
            TargetSearchConfig {
                bound_radius: fractal.bound_radius,
                hit_epsilon: 1.0e-6,
                max_steps: 800,
                attempts: 96,
                aim_jitter: 0.35,
            },
            MANDELBOX_LIGHT.map(f64::from),
        );
        let target = picker
            .pick_origin_gap(seed, [1.0, 0.0, 0.0])
            .unwrap_or(FALLBACK_TARGET);
        let dive = ExponentialDivePath {
            overview_distance: 11.0,
            minimum_distance: 5.0e-5,
            overview_duration: 4.0,
            dive_duration: 23.0,
        };
        let distance = dive.distance_at(0.0);
        let camera_position = [
            target.point[0] - target.view_direction[0] * distance,
            target.point[1] - target.view_direction[1] * distance,
            target.point[2] - target.view_direction[2] * distance,
        ];

        Self {
            camera: CameraConfig {
                position: camera_position.map(|value| value as f32),
                target: target.point.map(|value| value as f32),
                up: [0.0, 0.0, 1.0],
                vertical_fov_degrees: 30.0,
            },
            fractal: FractalConfig::Mandelbox(fractal),
            light: LightConfig {
                direction: MANDELBOX_LIGHT,
            },
            render: RenderSettings {
                width: 640,
                height: 360,
                max_steps: 280,
                max_distance: 100.0,
                epsilon: 5.0e-8,
                step_safety: 0.93,
                pixel_epsilon_multiplier: 1.5,
            },
            seed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portfolio_mandelbox_scene_is_valid_and_reproducible() {
        let first = RenderConfig::mandelbox(12_345);
        let second = RenderConfig::mandelbox(12_345);
        first.validate().expect("Mandelbox preset must be valid");
        assert_eq!(first.camera.position, second.camera.position);
        assert_eq!(first.camera.target, second.camera.target);
        assert_eq!(first.fractal.kind(), crate::FractalKind::Mandelbox);
    }
}
