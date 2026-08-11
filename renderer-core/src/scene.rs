use crate::{
    CameraConfig, ExponentialDivePath, FractalConfig, LightConfig, MIN_QUAD_CAMERA_DISTANCE,
    MandelboxConfig, PathTarget, Precision, Qf32, QfVec3, QualityConfig, RenderConfig,
    RenderSettings, TargetPicker, TargetSearchConfig,
};

const MANDELBOX_LIGHT: [f32; 3] = [2.0, 1.0, 1.0];
const FALLBACK_TARGET: PathTarget = PathTarget {
    point: QfVec3::new(
        Qf32::from_f32(2.212_921),
        Qf32::from_f32(1.099_011),
        Qf32::from_f32(0.307_275),
    ),
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
            overview_distance: Qf32::from_f32(11.0),
            minimum_distance: Qf32::from_f32(5.0e-5),
            overview_duration: 4.0,
            dive_duration: 23.0,
        };
        let distance = dive.distance_qf_at(0.0);
        let camera_position = target.point - QfVec3::from_f64(target.view_direction) * distance;

        Self {
            precision: Precision::F32,
            camera: CameraConfig {
                position: QfVec3::from_f32(camera_position.to_f32()),
                target: QfVec3::from_f32(target.point.to_f32()),
                up: [0.0, 0.0, 1.0],
                vertical_fov_degrees: 30.0,
                aperture_radius: 0.0,
                focus_distance: 11.0,
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
            quality: QualityConfig::default(),
            seed,
        }
    }

    /// Portfolio Mandelbox preset with an analytic boundary target and a
    /// quad-float camera separation. Intended as the precision-safe input to
    /// Phase 3 paths.
    #[must_use]
    pub fn mandelbox_quad(seed: u32, camera_distance: Qf32) -> Option<Self> {
        if camera_distance <= Qf32::ZERO
            || !camera_distance.is_finite()
            || camera_distance.to_f64() < MIN_QUAD_CAMERA_DISTANCE
        {
            return None;
        }
        let fractal = MandelboxConfig::default();
        // The positive axial boundary is exactly twice the box-fold limit.
        // Keeping this analytic point avoids a tiny sphere-tracing overshoot
        // becoming larger than the entire camera offset at extreme zoom.
        let target = PathTarget {
            point: QfVec3::new(
                Qf32::from_f32(2.0 * fractal.fold_limit),
                Qf32::ZERO,
                Qf32::ZERO,
            ),
            view_direction: [-1.0, 0.0, 0.0],
        };
        let camera_position =
            target.point - QfVec3::from_f64(target.view_direction) * camera_distance;
        let mut config = Self {
            precision: Precision::QuadFloat,
            camera: CameraConfig {
                position: camera_position,
                target: target.point,
                up: [0.0, 0.0, 1.0],
                vertical_fov_degrees: 30.0,
                aperture_radius: 0.0,
                focus_distance: camera_distance.to_f32(),
            },
            fractal: FractalConfig::Mandelbox(fractal),
            light: LightConfig {
                direction: MANDELBOX_LIGHT,
            },
            render: RenderSettings {
                width: 640,
                height: 360,
                max_steps: 128,
                max_distance: 1.0,
                epsilon: 1.0e-7,
                step_safety: 0.9,
                pixel_epsilon_multiplier: 1.5,
            },
            quality: QualityConfig::default(),
            seed,
        };
        config.tune_mandelbox_quad_zoom(camera_distance).ok()?;
        Some(config)
    }

    /// Retunes the cancellation-sensitive values that vary over a deep zoom.
    /// Geometry, lighting, resolution, and the ray-step budget remain owned by
    /// the caller, making this suitable for both presets and animation paths.
    pub fn tune_mandelbox_quad_zoom(&mut self, camera_distance: Qf32) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.precision == Precision::QuadFloat,
            "quad zoom tuning requires quad-float precision"
        );
        anyhow::ensure!(
            camera_distance > Qf32::ZERO
                && camera_distance.is_finite()
                && camera_distance.to_f64() >= MIN_QUAD_CAMERA_DISTANCE,
            "quad-float Mandelbox camera distance must be finite and at least {MIN_QUAD_CAMERA_DISTANCE:e}"
        );
        let FractalConfig::Mandelbox(fractal) = &mut self.fractal else {
            anyhow::bail!("quad zoom tuning is currently supported only for Mandelbox");
        };
        let distance_f32 = camera_distance.to_f32();
        anyhow::ensure!(
            distance_f32.is_finite() && distance_f32 > 0.0 && (distance_f32 * 4.0).is_finite(),
            "quad-float camera distance is outside the supported f32 exponent range"
        );
        fractal.iterations = quad_iterations_for_distance(camera_distance, fractal.scale);
        self.render.max_distance = distance_f32 * 4.0;
        self.render.epsilon = (distance_f32 * 1.0e-7).max(1.0e-30);
        let old_focus_distance = self.camera.focus_distance;
        let zoom_scale = if old_focus_distance.is_finite() && old_focus_distance > 0.0 {
            distance_f32 / old_focus_distance
        } else {
            1.0
        };
        self.camera.focus_distance = distance_f32;
        self.camera.aperture_radius *= zoom_scale;
        self.quality.ambient_occlusion.radius *= zoom_scale;
        self.quality.soft_shadow.max_distance *= zoom_scale;
        self.quality.reflection.max_distance *= zoom_scale;
        Ok(())
    }
}

fn quad_iterations_for_distance(camera_distance: Qf32, scale: f32) -> u32 {
    let distance = camera_distance.to_f64();
    let detail_iterations = if distance >= 1.0 {
        0
    } else {
        ((1.0 / distance).ln() / f64::from(scale.abs()).ln()).ceil() as u32
    };
    detail_iterations.saturating_add(5).clamp(16, 96)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HighPrecisionDistanceEstimator;

    #[test]
    fn portfolio_mandelbox_scene_is_valid_and_reproducible() {
        let first = RenderConfig::mandelbox(12_345);
        let second = RenderConfig::mandelbox(12_345);
        first.validate().expect("Mandelbox preset must be valid");
        assert_eq!(first.camera.position, second.camera.position);
        assert_eq!(first.camera.target, second.camera.target);
        assert_eq!(first.fractal.kind(), crate::FractalKind::Mandelbox);
    }

    #[test]
    fn quad_scene_retains_camera_separation_below_f32_ulp() {
        let distance = Qf32::from_f64(1.0e-12);
        let scene = RenderConfig::mandelbox_quad(12_345, distance)
            .expect("reference scene must be generated");
        let repeated =
            RenderConfig::mandelbox_quad(12_345, distance).expect("scene must be reproducible");
        scene.validate().expect("quad preset must be valid");
        assert_eq!(scene.camera.position, repeated.camera.position);
        assert_eq!(scene.camera.target, repeated.camera.target);
        assert_ne!(scene.camera.position, scene.camera.target);
        assert_eq!(
            scene.camera.position.to_f32(),
            scene.camera.target.to_f32(),
            "the test zoom must actually exceed absolute f32 coordinate precision"
        );
        let separation = (scene.camera.target - scene.camera.position)
            .length_squared()
            .sqrt();
        assert!((separation - distance).abs() < Qf32::from_f64(1.0e-18));
    }

    #[test]
    fn quad_scene_scales_iterations_with_zoom_depth() {
        let near = RenderConfig::mandelbox_quad(12_345, Qf32::from_f64(1.0e-12)).unwrap();
        let deep = RenderConfig::mandelbox_quad(12_345, Qf32::from_f64(1.0e-24)).unwrap();
        assert_eq!(near.fractal.iterations(), 41);
        assert_eq!(deep.fractal.iterations(), 76);
        assert_eq!(deep.render.max_steps, 128);
    }

    #[test]
    fn quad_scene_uses_exact_boundary_and_enforces_measured_depth() {
        let distance = Qf32::from_f64(MIN_QUAD_CAMERA_DISTANCE);
        let scene = RenderConfig::mandelbox_quad(12_345, distance).unwrap();
        let boundary = Qf32::from_f32(2.0 * MandelboxConfig::default().fold_limit);
        assert_eq!(scene.camera.target.x, boundary);
        assert_eq!(scene.camera.position.x - scene.camera.target.x, distance);
        assert_eq!(scene.fractal.iterations(), 82);
        let FractalConfig::Mandelbox(fractal) = &scene.fractal else {
            unreachable!();
        };
        let distance_ratio =
            (fractal.distance_estimate_qf(scene.camera.position) / distance).to_f64();
        assert!(
            (0.5..=2.0).contains(&distance_ratio),
            "deep DE/camera ratio {distance_ratio} is not conservative enough"
        );
        assert!(RenderConfig::mandelbox_quad(12_345, Qf32::from_f64(1.0e-27)).is_none());
    }
}
