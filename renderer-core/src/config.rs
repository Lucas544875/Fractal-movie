use anyhow::{Result, bail};

/// Hard safety ceilings shared by validation and the bounded WGSL loops.
pub const MAX_IMAGE_DIMENSION: u32 = 8_192;
pub const MAX_PIXEL_COUNT: u64 = 33_554_432;
pub const MAX_RAY_STEPS: u32 = 1_024;
pub const MAX_FRACTAL_ITERATIONS: u32 = 64;

#[derive(Clone, Debug)]
pub struct CameraConfig {
    pub position: [f32; 3],
    pub target: [f32; 3],
    pub vertical_fov_degrees: f32,
}

#[derive(Clone, Debug)]
pub struct FractalConfig {
    pub power: f32,
    pub iterations: u32,
    pub bailout: f32,
}

#[derive(Clone, Debug)]
pub struct LightConfig {
    /// Unit direction from the surface towards the directional light.
    pub direction: [f32; 3],
}

#[derive(Clone, Debug)]
pub struct RenderSettings {
    pub width: u32,
    pub height: u32,
    pub max_steps: u32,
    pub max_distance: f32,
    pub epsilon: f32,
}

/// Complete in-memory render description used by the Phase 1 renderer.
///
/// Phase 2 will deserialize this model from a versioned scene file rather than
/// changing the GPU renderer API.
#[derive(Clone, Debug)]
pub struct RenderConfig {
    pub camera: CameraConfig,
    pub fractal: FractalConfig,
    pub light: LightConfig,
    pub render: RenderSettings,
    pub seed: u32,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            camera: CameraConfig {
                position: [2.5, 1.7, 2.5],
                target: [0.0, 0.0, 0.0],
                vertical_fov_degrees: 38.0,
            },
            fractal: FractalConfig {
                power: 8.0,
                iterations: 16,
                bailout: 4.0,
            },
            light: LightConfig {
                direction: [-0.45, 0.75, 0.55],
            },
            render: RenderSettings {
                width: 640,
                height: 360,
                max_steps: 256,
                max_distance: 100.0,
                epsilon: 0.0001,
            },
            seed: 12_345,
        }
    }
}

impl RenderConfig {
    /// Validates CPU-side inputs before allocating GPU resources.
    pub fn validate(&self) -> Result<()> {
        let render = &self.render;
        if render.width == 0 || render.height == 0 {
            bail!("render dimensions must be greater than zero");
        }
        if render.width > MAX_IMAGE_DIMENSION || render.height > MAX_IMAGE_DIMENSION {
            bail!("render dimensions must not exceed {MAX_IMAGE_DIMENSION} on either axis");
        }
        let pixel_count = u64::from(render.width) * u64::from(render.height);
        if pixel_count > MAX_PIXEL_COUNT {
            bail!("render pixel count {pixel_count} exceeds safety limit {MAX_PIXEL_COUNT}");
        }
        if render.max_steps == 0 || render.max_steps > MAX_RAY_STEPS {
            bail!("max_steps must be in 1..={MAX_RAY_STEPS}");
        }
        if self.fractal.iterations == 0 || self.fractal.iterations > MAX_FRACTAL_ITERATIONS {
            bail!("fractal iterations must be in 1..={MAX_FRACTAL_ITERATIONS}");
        }
        finite_positive("fractal power", self.fractal.power)?;
        if self.fractal.power < 2.0 || self.fractal.power > 32.0 {
            bail!("fractal power must be in 2.0..=32.0");
        }
        finite_positive("fractal bailout", self.fractal.bailout)?;
        finite_positive("max_distance", render.max_distance)?;
        finite_positive("epsilon", render.epsilon)?;
        if render.epsilon >= 0.1 {
            bail!("epsilon must be less than 0.1");
        }
        let fov = self.camera.vertical_fov_degrees;
        if !fov.is_finite() || !(1.0..179.0).contains(&fov) {
            bail!("vertical camera FOV must be finite and in 1.0..179.0 degrees");
        }
        finite_vector("camera position", self.camera.position)?;
        finite_vector("camera target", self.camera.target)?;
        finite_vector("light direction", self.light.direction)?;
        if squared_distance(self.camera.position, self.camera.target) < 1.0e-8 {
            bail!("camera position and target must not be equal");
        }
        if squared_length(self.light.direction) < 1.0e-8 {
            bail!("light direction must not be zero");
        }
        Ok(())
    }
}

fn finite_positive(name: &str, value: f32) -> Result<()> {
    if !value.is_finite() || value <= 0.0 {
        bail!("{name} must be finite and greater than zero");
    }
    Ok(())
}

fn finite_vector(name: &str, value: [f32; 3]) -> Result<()> {
    if value.iter().any(|component| !component.is_finite()) {
        bail!("{name} must contain only finite values");
    }
    Ok(())
}

fn squared_length(value: [f32; 3]) -> f32 {
    value.iter().map(|component| component * component).sum()
}

fn squared_distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    squared_length([a[0] - b[0], a[1] - b[1], a[2] - b[2]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        RenderConfig::default()
            .validate()
            .expect("the built-in Phase 1 scene must remain valid");
    }

    #[test]
    fn rejects_unbounded_gpu_work() {
        let mut config = RenderConfig::default();
        config.render.max_steps = MAX_RAY_STEPS + 1;
        assert!(config.validate().is_err());

        config = RenderConfig::default();
        config.fractal.iterations = MAX_FRACTAL_ITERATIONS + 1;
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_non_finite_values() {
        let mut config = RenderConfig::default();
        config.fractal.power = f32::NAN;
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_degenerate_camera() {
        let mut config = RenderConfig::default();
        config.camera.target = config.camera.position;
        assert!(config.validate().is_err());
    }
}
