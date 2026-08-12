use std::{fs, path::Path, str::FromStr};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::{
    AmbientOcclusionConfig, AnimationConfig, AnimationPath, CameraConfig, DslFractalConfig,
    DslMaterial, DslPaletteStop, ExponentialDivePath, FractalConfig, LightConfig, MandelboxConfig,
    MandelbulbConfig, MultiTargetDivePath, OrbitTransform, PostProcessConfig, Precision, Qf32,
    QfVec3, QualityConfig, ReflectionConfig, RenderConfig, RenderSettings, SoftShadowConfig,
    SurfaceFlyoverPath, TargetOrbitPath, TargetSearchConfig, ToneMappingConfig,
    ToneMappingOperator, VideoConfig,
};

pub const CURRENT_SCENE_VERSION: u32 = 1;

/// A validated scene loaded from the versioned YAML format.
#[derive(Clone, Debug)]
pub struct LoadedScene {
    pub name: String,
    pub config: RenderConfig,
    pub animation: Option<AnimationConfig>,
    pub video: Option<VideoConfig>,
}

impl LoadedScene {
    /// Serializes the scene using the current schema version.
    pub fn to_yaml(&self) -> Result<String> {
        let document = SceneDocument::from(self);
        serde_yaml_ng::to_string(&document).context("could not serialize scene as YAML")
    }
}

/// Loads and validates a scene file.
pub fn load_scene(path: impl AsRef<Path>) -> Result<LoadedScene> {
    let path = path.as_ref();
    let yaml = fs::read_to_string(path)
        .with_context(|| format!("could not read scene file {}", path.display()))?;
    parse_scene(&yaml).with_context(|| format!("invalid scene file {}", path.display()))
}

/// Parses and validates a scene from a YAML string.
pub fn parse_scene(yaml: &str) -> Result<LoadedScene> {
    let document: SceneDocument =
        serde_yaml_ng::from_str(yaml).context("YAML does not match the scene schema")?;
    document.try_into()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneDocument {
    version: u32,
    name: String,
    #[serde(default)]
    precision: ScenePrecision,
    seed: u32,
    camera: SceneCamera,
    fractal: SceneFractal,
    light: SceneLight,
    render: SceneRender,
    #[serde(default)]
    quality: SceneQuality,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    animation: Option<SceneAnimation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    video: Option<SceneVideo>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ScenePrecision {
    #[default]
    F32,
    QuadFloat,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneCamera {
    position: [SceneScalar; 3],
    target: [SceneScalar; 3],
    up: [f32; 3],
    vertical_fov_degrees: f32,
    #[serde(default)]
    aperture_radius: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    focus_distance: Option<f32>,
}

/// Coordinate scalars accept convenient YAML numbers, exact decimal strings,
/// or an exact four-limb expansion emitted by `LoadedScene::to_yaml`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum SceneScalar {
    Number(f64),
    Decimal(String),
    Expansion([f32; 4]),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", content = "parameters", rename_all = "kebab-case")]
enum SceneFractal {
    Mandelbulb(SceneMandelbulb),
    Mandelbox(SceneMandelbox),
    Dsl(SceneDslFractal),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneMandelbulb {
    power: f32,
    iterations: u32,
    bailout: f32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneMandelbox {
    scale: f32,
    min_radius_squared: f32,
    fixed_radius_squared: f32,
    fold_limit: f32,
    iterations: u32,
    bound_radius: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneDslFractal {
    iterations: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    orbit_period: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    color_iterations: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bailout: Option<f32>,
    normal_epsilon: f32,
    orbit: Vec<SceneOrbitTransform>,
    #[serde(default)]
    material: SceneDslMaterial,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
enum SceneOrbitTransform {
    AmazingSurfFold {
        start_iteration: u32,
        stop_iteration: u32,
        limits: [f32; 2],
        minimum_radius_squared: f32,
        scale: f32,
        rotation_degrees: [f32; 3],
    },
    MandelboxJuliaFold {
        start_iteration: u32,
        stop_iteration: u32,
        fold_limit: f32,
        min_radius_squared: f32,
        fixed_radius_squared: f32,
        scale: f32,
        constant: [f32; 3],
        rotation_degrees: [f32; 3],
    },
    BoxFold {
        limit: f32,
    },
    SphereFold {
        min_radius_squared: f32,
        fixed_radius_squared: f32,
    },
    ScaleAddPoint {
        scale: f32,
    },
    ScaleAddConstant {
        scale: f32,
        constant: [f32; 3],
    },
    Rotate {
        axis: [f32; 3],
        degrees: f32,
    },
    Translate {
        offset: [f32; 3],
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct SceneDslMaterial {
    base_color: [f32; 3],
    accent_color: [f32; 3],
    specular_color: [f32; 3],
    background_bottom: [f32; 3],
    background_top: [f32; 3],
    color_frequency: f32,
    surface_palette: Vec<SceneDslPaletteStop>,
    orbit_palette_weight: f32,
    palette_offset: f32,
    camera_palette_weight: f32,
    normal_palette_weight: f32,
    ambient_strength: f32,
    diffuse_strength: f32,
    specular_strength: f32,
    shininess: f32,
    metallic_specular_strength: f32,
    metallic_shininess: f32,
    rim_strength: f32,
    fog_density: f32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneDslPaletteStop {
    position: f32,
    color: [f32; 3],
}

impl Default for SceneDslMaterial {
    fn default() -> Self {
        Self::from(&DslMaterial::default())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneLight {
    direction: [f32; 3],
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneRender {
    width: u32,
    height: u32,
    max_steps: u32,
    max_distance: f32,
    epsilon: f32,
    step_safety: f32,
    pixel_epsilon_multiplier: f32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct SceneQuality {
    samples_per_pixel: u32,
    ambient_occlusion: SceneAmbientOcclusion,
    soft_shadow: SceneSoftShadow,
    reflection: SceneReflection,
    tone_mapping: SceneToneMapping,
    post_process: ScenePostProcess,
}

impl Default for SceneQuality {
    fn default() -> Self {
        Self::from(&QualityConfig::default())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct SceneAmbientOcclusion {
    max_steps: u32,
    radius: f32,
    strength: f32,
}

impl Default for SceneAmbientOcclusion {
    fn default() -> Self {
        Self::from(&QualityConfig::default().ambient_occlusion)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct SceneSoftShadow {
    max_steps: u32,
    angular_radius_degrees: f32,
    max_distance: f32,
}

impl Default for SceneSoftShadow {
    fn default() -> Self {
        Self::from(&QualityConfig::default().soft_shadow)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct SceneReflection {
    max_steps: u32,
    max_distance: f32,
    strength: f32,
    roughness: f32,
}

impl Default for SceneReflection {
    fn default() -> Self {
        Self::from(&QualityConfig::default().reflection)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct SceneToneMapping {
    enabled: bool,
    operator: SceneToneMappingOperator,
    exposure_stops: f32,
    white_point: f32,
    brightness: f32,
    contrast: f32,
    gamma: f32,
    saturation: f32,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum SceneToneMappingOperator {
    #[default]
    ExtendedReinhard,
    Mandelbulber,
}

impl Default for SceneToneMapping {
    fn default() -> Self {
        Self::from(&QualityConfig::default().tone_mapping)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct ScenePostProcess {
    enabled: bool,
    exposure_stops: f32,
    contrast: f32,
    saturation: f32,
    gamma: f32,
    vignette_strength: f32,
}

impl Default for ScenePostProcess {
    fn default() -> Self {
        Self::from(&QualityConfig::default().post_process)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneAnimation {
    fps: u32,
    frame_count: u32,
    path: SceneAnimationPath,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", content = "parameters", rename_all = "kebab-case")]
enum SceneAnimationPath {
    ExponentialDive(SceneExponentialDive),
    TargetOrbit(SceneTargetOrbit),
    MultiTargetDive(SceneMultiTargetDive),
    SurfaceFlyover(SceneSurfaceFlyover),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneExponentialDive {
    overview_distance: SceneScalar,
    minimum_distance: SceneScalar,
    overview_duration: f64,
    dive_duration: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneTargetOrbit {
    radius: SceneScalar,
    duration: f64,
    revolutions: f64,
    axis: [f64; 3],
    cone_angle_degrees: f64,
    #[serde(default)]
    start_angle_degrees: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneMultiTargetDive {
    overview_distance: SceneScalar,
    minimum_distance: SceneScalar,
    overview_duration: f64,
    dive_duration: f64,
    transition_duration: f64,
    #[serde(default)]
    search: SceneTargetSearch,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneSurfaceFlyover {
    camera_height: f64,
    travel_distance: f64,
    duration: f64,
    look_ahead: f64,
    travel_direction: [f64; 3],
    normal_epsilon: f64,
    #[serde(default)]
    search: SceneTargetSearch,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct SceneTargetSearch {
    bound_radius: f64,
    hit_epsilon: f64,
    max_steps: u32,
    attempts: u32,
    aim_jitter: f64,
}

impl Default for SceneTargetSearch {
    fn default() -> Self {
        Self {
            bound_radius: 4.2,
            hit_epsilon: 1.0e-6,
            max_steps: 800,
            attempts: 128,
            aim_jitter: 0.25,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct SceneVideo {
    codec: String,
    pixel_format: String,
    crf: u8,
    preset: String,
    faststart: bool,
}

impl Default for SceneVideo {
    fn default() -> Self {
        let config = VideoConfig::default();
        Self::from(&config)
    }
}

impl TryFrom<SceneDocument> for LoadedScene {
    type Error = anyhow::Error;

    fn try_from(document: SceneDocument) -> Result<Self> {
        if document.version != CURRENT_SCENE_VERSION {
            bail!(
                "unsupported scene version {}; this build supports version {}",
                document.version,
                CURRENT_SCENE_VERSION
            );
        }
        validate_scene_name(&document.name)?;
        let precision = match document.precision {
            ScenePrecision::F32 => Precision::F32,
            ScenePrecision::QuadFloat => Precision::QuadFloat,
        };
        let camera_position = parse_coordinate(document.camera.position, precision)
            .context("invalid camera position")?;
        let camera_target =
            parse_coordinate(document.camera.target, precision).context("invalid camera target")?;
        let focus_distance = document.camera.focus_distance.unwrap_or_else(|| {
            (camera_target - camera_position)
                .length_squared()
                .sqrt()
                .to_f32()
        });

        let fractal = match document.fractal {
            SceneFractal::Mandelbulb(config) => FractalConfig::Mandelbulb(MandelbulbConfig {
                power: config.power,
                iterations: config.iterations,
                bailout: config.bailout,
            }),
            SceneFractal::Mandelbox(config) => FractalConfig::Mandelbox(MandelboxConfig {
                scale: config.scale,
                min_radius_squared: config.min_radius_squared,
                fixed_radius_squared: config.fixed_radius_squared,
                fold_limit: config.fold_limit,
                iterations: config.iterations,
                bound_radius: config.bound_radius,
            }),
            SceneFractal::Dsl(config) => FractalConfig::Dsl(DslFractalConfig {
                iterations: config.iterations,
                orbit_period: config.orbit_period,
                color_iterations: config.color_iterations.unwrap_or(config.iterations),
                bailout: config.bailout,
                normal_epsilon: config.normal_epsilon,
                orbit: config.orbit.into_iter().map(OrbitTransform::from).collect(),
                material: DslMaterial::from(config.material),
            }),
        };
        let config = RenderConfig {
            precision,
            camera: CameraConfig {
                position: camera_position,
                target: camera_target,
                up: document.camera.up,
                vertical_fov_degrees: document.camera.vertical_fov_degrees,
                aperture_radius: document.camera.aperture_radius,
                focus_distance,
            },
            fractal,
            light: LightConfig {
                direction: document.light.direction,
            },
            render: RenderSettings {
                width: document.render.width,
                height: document.render.height,
                max_steps: document.render.max_steps,
                max_distance: document.render.max_distance,
                epsilon: document.render.epsilon,
                step_safety: document.render.step_safety,
                pixel_epsilon_multiplier: document.render.pixel_epsilon_multiplier,
            },
            quality: QualityConfig::from(document.quality),
            seed: document.seed,
        };
        config
            .validate()
            .context("scene configuration is invalid")?;

        let mut animation = document
            .animation
            .map(AnimationConfig::try_from)
            .transpose()
            .context("invalid animation configuration")?;
        if let Some(animation) = &mut animation {
            animation
                .plan(&config)
                .context("automatic path planning failed")?;
            animation
                .validate(&config)
                .context("animation configuration is invalid")?;
        }
        let video = document.video.map(VideoConfig::from);
        if let Some(video) = &video {
            if animation.is_none() {
                bail!("video configuration requires an animation section");
            }
            video
                .validate_dimensions(config.render.width, config.render.height)
                .context("video configuration is invalid")?;
        }

        Ok(Self {
            name: document.name,
            config,
            animation,
            video,
        })
    }
}

impl From<&LoadedScene> for SceneDocument {
    fn from(scene: &LoadedScene) -> Self {
        let fractal = match &scene.config.fractal {
            FractalConfig::Mandelbulb(config) => SceneFractal::Mandelbulb(SceneMandelbulb {
                power: config.power,
                iterations: config.iterations,
                bailout: config.bailout,
            }),
            FractalConfig::Mandelbox(config) => SceneFractal::Mandelbox(SceneMandelbox {
                scale: config.scale,
                min_radius_squared: config.min_radius_squared,
                fixed_radius_squared: config.fixed_radius_squared,
                fold_limit: config.fold_limit,
                iterations: config.iterations,
                bound_radius: config.bound_radius,
            }),
            FractalConfig::Dsl(config) => SceneFractal::Dsl(SceneDslFractal {
                iterations: config.iterations,
                orbit_period: config.orbit_period,
                color_iterations: (config.color_iterations != config.iterations)
                    .then_some(config.color_iterations),
                bailout: config.bailout,
                normal_epsilon: config.normal_epsilon,
                orbit: config.orbit.iter().map(SceneOrbitTransform::from).collect(),
                material: SceneDslMaterial::from(&config.material),
            }),
        };
        Self {
            version: CURRENT_SCENE_VERSION,
            name: scene.name.clone(),
            precision: match scene.config.precision {
                Precision::F32 => ScenePrecision::F32,
                Precision::QuadFloat => ScenePrecision::QuadFloat,
            },
            seed: scene.config.seed,
            camera: SceneCamera {
                position: serialize_coordinate(
                    scene.config.camera.position,
                    scene.config.precision,
                ),
                target: serialize_coordinate(scene.config.camera.target, scene.config.precision),
                up: scene.config.camera.up,
                vertical_fov_degrees: scene.config.camera.vertical_fov_degrees,
                aperture_radius: scene.config.camera.aperture_radius,
                focus_distance: Some(scene.config.camera.focus_distance),
            },
            fractal,
            light: SceneLight {
                direction: scene.config.light.direction,
            },
            render: SceneRender {
                width: scene.config.render.width,
                height: scene.config.render.height,
                max_steps: scene.config.render.max_steps,
                max_distance: scene.config.render.max_distance,
                epsilon: scene.config.render.epsilon,
                step_safety: scene.config.render.step_safety,
                pixel_epsilon_multiplier: scene.config.render.pixel_epsilon_multiplier,
            },
            quality: SceneQuality::from(&scene.config.quality),
            animation: scene.animation.as_ref().map(SceneAnimation::from),
            video: scene.video.as_ref().map(SceneVideo::from),
        }
    }
}

impl From<SceneOrbitTransform> for OrbitTransform {
    fn from(transform: SceneOrbitTransform) -> Self {
        match transform {
            SceneOrbitTransform::AmazingSurfFold {
                start_iteration,
                stop_iteration,
                limits,
                minimum_radius_squared,
                scale,
                rotation_degrees,
            } => Self::AmazingSurfFold {
                start_iteration,
                stop_iteration,
                limits,
                minimum_radius_squared,
                scale,
                rotation_degrees,
            },
            SceneOrbitTransform::MandelboxJuliaFold {
                start_iteration,
                stop_iteration,
                fold_limit,
                min_radius_squared,
                fixed_radius_squared,
                scale,
                constant,
                rotation_degrees,
            } => Self::MandelboxJuliaFold {
                start_iteration,
                stop_iteration,
                fold_limit,
                min_radius_squared,
                fixed_radius_squared,
                scale,
                constant,
                rotation_degrees,
            },
            SceneOrbitTransform::BoxFold { limit } => Self::BoxFold { limit },
            SceneOrbitTransform::SphereFold {
                min_radius_squared,
                fixed_radius_squared,
            } => Self::SphereFold {
                min_radius_squared,
                fixed_radius_squared,
            },
            SceneOrbitTransform::ScaleAddPoint { scale } => Self::ScaleAddPoint { scale },
            SceneOrbitTransform::ScaleAddConstant { scale, constant } => {
                Self::ScaleAddConstant { scale, constant }
            }
            SceneOrbitTransform::Rotate { axis, degrees } => Self::Rotate { axis, degrees },
            SceneOrbitTransform::Translate { offset } => Self::Translate { offset },
        }
    }
}

impl From<&OrbitTransform> for SceneOrbitTransform {
    fn from(transform: &OrbitTransform) -> Self {
        match transform {
            OrbitTransform::AmazingSurfFold {
                start_iteration,
                stop_iteration,
                limits,
                minimum_radius_squared,
                scale,
                rotation_degrees,
            } => Self::AmazingSurfFold {
                start_iteration: *start_iteration,
                stop_iteration: *stop_iteration,
                limits: *limits,
                minimum_radius_squared: *minimum_radius_squared,
                scale: *scale,
                rotation_degrees: *rotation_degrees,
            },
            OrbitTransform::MandelboxJuliaFold {
                start_iteration,
                stop_iteration,
                fold_limit,
                min_radius_squared,
                fixed_radius_squared,
                scale,
                constant,
                rotation_degrees,
            } => Self::MandelboxJuliaFold {
                start_iteration: *start_iteration,
                stop_iteration: *stop_iteration,
                fold_limit: *fold_limit,
                min_radius_squared: *min_radius_squared,
                fixed_radius_squared: *fixed_radius_squared,
                scale: *scale,
                constant: *constant,
                rotation_degrees: *rotation_degrees,
            },
            OrbitTransform::BoxFold { limit } => Self::BoxFold { limit: *limit },
            OrbitTransform::SphereFold {
                min_radius_squared,
                fixed_radius_squared,
            } => Self::SphereFold {
                min_radius_squared: *min_radius_squared,
                fixed_radius_squared: *fixed_radius_squared,
            },
            OrbitTransform::ScaleAddPoint { scale } => Self::ScaleAddPoint { scale: *scale },
            OrbitTransform::ScaleAddConstant { scale, constant } => Self::ScaleAddConstant {
                scale: *scale,
                constant: *constant,
            },
            OrbitTransform::Rotate { axis, degrees } => Self::Rotate {
                axis: *axis,
                degrees: *degrees,
            },
            OrbitTransform::Translate { offset } => Self::Translate { offset: *offset },
        }
    }
}

impl From<SceneDslMaterial> for DslMaterial {
    fn from(material: SceneDslMaterial) -> Self {
        Self {
            base_color: material.base_color,
            accent_color: material.accent_color,
            specular_color: material.specular_color,
            background_bottom: material.background_bottom,
            background_top: material.background_top,
            color_frequency: material.color_frequency,
            surface_palette: material
                .surface_palette
                .into_iter()
                .map(|stop| DslPaletteStop {
                    position: stop.position,
                    color: stop.color,
                })
                .collect(),
            orbit_palette_weight: material.orbit_palette_weight,
            palette_offset: material.palette_offset,
            camera_palette_weight: material.camera_palette_weight,
            normal_palette_weight: material.normal_palette_weight,
            ambient_strength: material.ambient_strength,
            diffuse_strength: material.diffuse_strength,
            specular_strength: material.specular_strength,
            shininess: material.shininess,
            metallic_specular_strength: material.metallic_specular_strength,
            metallic_shininess: material.metallic_shininess,
            rim_strength: material.rim_strength,
            fog_density: material.fog_density,
        }
    }
}

impl From<&DslMaterial> for SceneDslMaterial {
    fn from(material: &DslMaterial) -> Self {
        Self {
            base_color: material.base_color,
            accent_color: material.accent_color,
            specular_color: material.specular_color,
            background_bottom: material.background_bottom,
            background_top: material.background_top,
            color_frequency: material.color_frequency,
            surface_palette: material
                .surface_palette
                .iter()
                .map(|stop| SceneDslPaletteStop {
                    position: stop.position,
                    color: stop.color,
                })
                .collect(),
            orbit_palette_weight: material.orbit_palette_weight,
            palette_offset: material.palette_offset,
            camera_palette_weight: material.camera_palette_weight,
            normal_palette_weight: material.normal_palette_weight,
            ambient_strength: material.ambient_strength,
            diffuse_strength: material.diffuse_strength,
            specular_strength: material.specular_strength,
            shininess: material.shininess,
            metallic_specular_strength: material.metallic_specular_strength,
            metallic_shininess: material.metallic_shininess,
            rim_strength: material.rim_strength,
            fog_density: material.fog_density,
        }
    }
}

impl From<SceneQuality> for QualityConfig {
    fn from(quality: SceneQuality) -> Self {
        Self {
            samples_per_pixel: quality.samples_per_pixel,
            ambient_occlusion: AmbientOcclusionConfig {
                max_steps: quality.ambient_occlusion.max_steps,
                radius: quality.ambient_occlusion.radius,
                strength: quality.ambient_occlusion.strength,
            },
            soft_shadow: SoftShadowConfig {
                max_steps: quality.soft_shadow.max_steps,
                angular_radius_degrees: quality.soft_shadow.angular_radius_degrees,
                max_distance: quality.soft_shadow.max_distance,
            },
            reflection: ReflectionConfig {
                max_steps: quality.reflection.max_steps,
                max_distance: quality.reflection.max_distance,
                strength: quality.reflection.strength,
                roughness: quality.reflection.roughness,
            },
            tone_mapping: ToneMappingConfig {
                enabled: quality.tone_mapping.enabled,
                operator: match quality.tone_mapping.operator {
                    SceneToneMappingOperator::ExtendedReinhard => {
                        ToneMappingOperator::ExtendedReinhard
                    }
                    SceneToneMappingOperator::Mandelbulber => ToneMappingOperator::Mandelbulber,
                },
                exposure_stops: quality.tone_mapping.exposure_stops,
                white_point: quality.tone_mapping.white_point,
                brightness: quality.tone_mapping.brightness,
                contrast: quality.tone_mapping.contrast,
                gamma: quality.tone_mapping.gamma,
                saturation: quality.tone_mapping.saturation,
            },
            post_process: PostProcessConfig {
                enabled: quality.post_process.enabled,
                exposure_stops: quality.post_process.exposure_stops,
                contrast: quality.post_process.contrast,
                saturation: quality.post_process.saturation,
                gamma: quality.post_process.gamma,
                vignette_strength: quality.post_process.vignette_strength,
            },
        }
    }
}

impl From<&QualityConfig> for SceneQuality {
    fn from(quality: &QualityConfig) -> Self {
        Self {
            samples_per_pixel: quality.samples_per_pixel,
            ambient_occlusion: SceneAmbientOcclusion::from(&quality.ambient_occlusion),
            soft_shadow: SceneSoftShadow::from(&quality.soft_shadow),
            reflection: SceneReflection::from(&quality.reflection),
            tone_mapping: SceneToneMapping::from(&quality.tone_mapping),
            post_process: ScenePostProcess::from(&quality.post_process),
        }
    }
}

impl From<&AmbientOcclusionConfig> for SceneAmbientOcclusion {
    fn from(config: &AmbientOcclusionConfig) -> Self {
        Self {
            max_steps: config.max_steps,
            radius: config.radius,
            strength: config.strength,
        }
    }
}

impl From<&SoftShadowConfig> for SceneSoftShadow {
    fn from(config: &SoftShadowConfig) -> Self {
        Self {
            max_steps: config.max_steps,
            angular_radius_degrees: config.angular_radius_degrees,
            max_distance: config.max_distance,
        }
    }
}

impl From<&ReflectionConfig> for SceneReflection {
    fn from(config: &ReflectionConfig) -> Self {
        Self {
            max_steps: config.max_steps,
            max_distance: config.max_distance,
            strength: config.strength,
            roughness: config.roughness,
        }
    }
}

impl From<&ToneMappingConfig> for SceneToneMapping {
    fn from(config: &ToneMappingConfig) -> Self {
        Self {
            enabled: config.enabled,
            operator: match config.operator {
                ToneMappingOperator::ExtendedReinhard => SceneToneMappingOperator::ExtendedReinhard,
                ToneMappingOperator::Mandelbulber => SceneToneMappingOperator::Mandelbulber,
            },
            exposure_stops: config.exposure_stops,
            white_point: config.white_point,
            brightness: config.brightness,
            contrast: config.contrast,
            gamma: config.gamma,
            saturation: config.saturation,
        }
    }
}

impl From<&PostProcessConfig> for ScenePostProcess {
    fn from(config: &PostProcessConfig) -> Self {
        Self {
            enabled: config.enabled,
            exposure_stops: config.exposure_stops,
            contrast: config.contrast,
            saturation: config.saturation,
            gamma: config.gamma,
            vignette_strength: config.vignette_strength,
        }
    }
}

impl From<SceneVideo> for VideoConfig {
    fn from(video: SceneVideo) -> Self {
        Self {
            codec: video.codec,
            pixel_format: video.pixel_format,
            crf: video.crf,
            preset: video.preset,
            faststart: video.faststart,
        }
    }
}

impl From<&VideoConfig> for SceneVideo {
    fn from(video: &VideoConfig) -> Self {
        Self {
            codec: video.codec.clone(),
            pixel_format: video.pixel_format.clone(),
            crf: video.crf,
            preset: video.preset.clone(),
            faststart: video.faststart,
        }
    }
}

impl TryFrom<SceneAnimation> for AnimationConfig {
    type Error = anyhow::Error;

    fn try_from(animation: SceneAnimation) -> Result<Self> {
        let path = match animation.path {
            SceneAnimationPath::ExponentialDive(path) => {
                AnimationPath::ExponentialDive(ExponentialDivePath {
                    overview_distance: parse_scalar(path.overview_distance)
                        .context("invalid overview_distance")?,
                    minimum_distance: parse_scalar(path.minimum_distance)
                        .context("invalid minimum_distance")?,
                    overview_duration: path.overview_duration,
                    dive_duration: path.dive_duration,
                })
            }
            SceneAnimationPath::TargetOrbit(path) => AnimationPath::TargetOrbit(TargetOrbitPath {
                radius: parse_scalar(path.radius).context("invalid radius")?,
                duration: path.duration,
                revolutions: path.revolutions,
                axis: path.axis,
                cone_angle_degrees: path.cone_angle_degrees,
                start_angle_degrees: path.start_angle_degrees,
            }),
            SceneAnimationPath::MultiTargetDive(path) => {
                AnimationPath::MultiTargetDive(MultiTargetDivePath::new(
                    parse_scalar(path.overview_distance).context("invalid overview_distance")?,
                    parse_scalar(path.minimum_distance).context("invalid minimum_distance")?,
                    path.overview_duration,
                    path.dive_duration,
                    path.transition_duration,
                    TargetSearchConfig::from(path.search),
                ))
            }
            SceneAnimationPath::SurfaceFlyover(path) => {
                AnimationPath::SurfaceFlyover(SurfaceFlyoverPath::new(
                    path.camera_height,
                    path.travel_distance,
                    path.duration,
                    path.look_ahead,
                    path.travel_direction,
                    path.normal_epsilon,
                    TargetSearchConfig::from(path.search),
                ))
            }
        };
        Ok(Self {
            fps: animation.fps,
            frame_count: animation.frame_count,
            path,
        })
    }
}

impl From<&AnimationConfig> for SceneAnimation {
    fn from(animation: &AnimationConfig) -> Self {
        let path = match &animation.path {
            AnimationPath::ExponentialDive(path) => {
                SceneAnimationPath::ExponentialDive(SceneExponentialDive {
                    overview_distance: SceneScalar::Expansion(path.overview_distance.limbs()),
                    minimum_distance: SceneScalar::Expansion(path.minimum_distance.limbs()),
                    overview_duration: path.overview_duration,
                    dive_duration: path.dive_duration,
                })
            }
            AnimationPath::TargetOrbit(path) => SceneAnimationPath::TargetOrbit(SceneTargetOrbit {
                radius: SceneScalar::Expansion(path.radius.limbs()),
                duration: path.duration,
                revolutions: path.revolutions,
                axis: path.axis,
                cone_angle_degrees: path.cone_angle_degrees,
                start_angle_degrees: path.start_angle_degrees,
            }),
            AnimationPath::MultiTargetDive(path) => {
                SceneAnimationPath::MultiTargetDive(SceneMultiTargetDive {
                    overview_distance: SceneScalar::Expansion(path.overview_distance.limbs()),
                    minimum_distance: SceneScalar::Expansion(path.minimum_distance.limbs()),
                    overview_duration: path.overview_duration,
                    dive_duration: path.dive_duration,
                    transition_duration: path.transition_duration,
                    search: SceneTargetSearch::from(path.search),
                })
            }
            AnimationPath::SurfaceFlyover(path) => {
                SceneAnimationPath::SurfaceFlyover(SceneSurfaceFlyover {
                    camera_height: path.camera_height,
                    travel_distance: path.travel_distance,
                    duration: path.duration,
                    look_ahead: path.look_ahead,
                    travel_direction: path.travel_direction,
                    normal_epsilon: path.normal_epsilon,
                    search: SceneTargetSearch::from(path.search),
                })
            }
        };
        Self {
            fps: animation.fps,
            frame_count: animation.frame_count,
            path,
        }
    }
}

impl From<SceneTargetSearch> for TargetSearchConfig {
    fn from(search: SceneTargetSearch) -> Self {
        Self {
            bound_radius: search.bound_radius,
            hit_epsilon: search.hit_epsilon,
            max_steps: search.max_steps,
            attempts: search.attempts,
            aim_jitter: search.aim_jitter,
        }
    }
}

impl From<TargetSearchConfig> for SceneTargetSearch {
    fn from(search: TargetSearchConfig) -> Self {
        Self {
            bound_radius: search.bound_radius,
            hit_epsilon: search.hit_epsilon,
            max_steps: search.max_steps,
            attempts: search.attempts,
            aim_jitter: search.aim_jitter,
        }
    }
}

fn parse_coordinate(values: [SceneScalar; 3], precision: Precision) -> Result<QfVec3> {
    let [x, y, z] = values.map(parse_scalar);
    let coordinate = QfVec3::new(x?, y?, z?);
    if precision == Precision::F32 {
        Ok(QfVec3::from_f32(coordinate.to_f32()))
    } else {
        Ok(coordinate)
    }
}

fn parse_scalar(value: SceneScalar) -> Result<Qf32> {
    match value {
        SceneScalar::Number(value) => Ok(Qf32::from_f64(value)),
        SceneScalar::Decimal(value) => Qf32::from_str(&value)
            .with_context(|| format!("invalid high-precision decimal '{value}'")),
        SceneScalar::Expansion(limbs) => {
            if limbs.iter().any(|limb| !limb.is_finite()) {
                bail!("quad-float expansion must contain only finite limbs");
            }
            Ok(Qf32::from_limbs(limbs))
        }
    }
}

fn serialize_coordinate(value: QfVec3, precision: Precision) -> [SceneScalar; 3] {
    value.components().map(|component| match precision {
        Precision::F32 => SceneScalar::Number(f64::from(component.to_f32())),
        Precision::QuadFloat => SceneScalar::Expansion(component.limbs()),
    })
}

fn validate_scene_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        bail!("scene name must contain 1..=64 characters");
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(anyhow!(
            "scene name may contain only ASCII letters, digits, '-' and '_'"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DistanceEstimator, FractalKind};

    const MANDELBULB: &str = include_str!("../../scenes/examples/mandelbulb.yaml");
    const MANDELBULB_TARGET_ORBIT: &str =
        include_str!("../../scenes/examples/mandelbulb-target-orbit.yaml");
    const MANDELBOX: &str = include_str!("../../scenes/examples/mandelbox.yaml");
    const MANDELBOX_QUAD_DEEP: &str =
        include_str!("../../scenes/examples/mandelbox-quad-deep.yaml");
    const MANDELBOX_QUAD_ZOOM: &str =
        include_str!("../../scenes/examples/mandelbox-quad-zoom.yaml");
    const TWISTED_MANDELBOX_DSL: &str =
        include_str!("../../scenes/examples/twisted-mandelbox-dsl.yaml");
    const MANDELBOX_FIRST_DESCENT_YOUTUBE: &str =
        include_str!("../../scenes/examples/mandelbox-first-descent-youtube.yaml");
    const ALCHEMY_PSEUDO_KLEINIAN: &str =
        include_str!("../../scenes/examples/alchemy-pseudo-kleinian.yaml");
    const ALCHEMY_PSEUDO_KLEINIAN_TARGET_ORBIT: &str =
        include_str!("../../scenes/examples/alchemy-pseudo-kleinian-target-orbit.yaml");
    const MANDELBOX_MULTI_TARGET_DIVE: &str =
        include_str!("../../scenes/examples/mandelbox-multi-target-dive.yaml");
    const MANDELBOX_SURFACE_FLYOVER: &str =
        include_str!("../../scenes/examples/mandelbox-surface-flyover.yaml");

    #[test]
    fn parses_both_example_scenes() {
        let bulb = parse_scene(MANDELBULB).expect("Mandelbulb example must parse");
        assert_eq!(bulb.name, "mandelbulb");
        assert_eq!(bulb.config.fractal.kind(), FractalKind::Mandelbulb);

        let box_scene = parse_scene(MANDELBOX).expect("Mandelbox example must parse");
        assert_eq!(box_scene.name, "mandelbox");
        assert_eq!(box_scene.config.fractal.kind(), FractalKind::Mandelbox);
        assert_eq!(box_scene.config.quality.samples_per_pixel, 16);
        assert_eq!(box_scene.config.camera.aperture_radius, 0.12);
        assert!(box_scene.config.quality.tone_mapping.enabled);

        let dsl = parse_scene(TWISTED_MANDELBOX_DSL).expect("DSL example must parse");
        assert_eq!(dsl.name, "twisted-mandelbox-dsl");
        assert_eq!(dsl.config.fractal.kind(), FractalKind::Dsl);
        let FractalConfig::Dsl(program) = &dsl.config.fractal else {
            unreachable!();
        };
        assert_eq!(program.orbit.len(), 4);
        assert!(program.generate_wgsl().is_ok());
    }

    #[test]
    fn scene_round_trip_preserves_render_config() {
        let original = parse_scene(MANDELBOX).expect("example must parse");
        let yaml = original.to_yaml().expect("scene must serialize");
        let reparsed = parse_scene(&yaml).expect("serialized scene must parse");
        assert_eq!(reparsed.name, original.name);
        assert_eq!(
            reparsed.config.camera.position,
            original.config.camera.position
        );
        assert_eq!(reparsed.config.camera.target, original.config.camera.target);
        assert_eq!(reparsed.config.render.width, original.config.render.width);
        assert_eq!(
            reparsed.config.quality.samples_per_pixel,
            original.config.quality.samples_per_pixel
        );
        assert_eq!(
            reparsed.config.camera.focus_distance,
            original.config.camera.focus_distance
        );
        assert_eq!(
            reparsed.config.fractal.kind(),
            original.config.fractal.kind()
        );
    }

    #[test]
    fn rejects_unknown_version_and_fields() {
        let wrong_version = MANDELBULB.replacen("version: 1", "version: 99", 1);
        let error = parse_scene(&wrong_version).expect_err("unknown version must fail");
        assert!(error.to_string().contains("unsupported scene version"));

        let unknown_field = format!("{MANDELBULB}\nunknown: true\n");
        let error = parse_scene(&unknown_field).expect_err("unknown field must fail");
        assert!(error.to_string().contains("scene schema"));
    }

    #[test]
    fn dsl_scene_round_trips_and_rejects_raw_shader_fields() {
        let scene = parse_scene(TWISTED_MANDELBOX_DSL).expect("DSL example must parse");
        let yaml = scene.to_yaml().expect("DSL scene must serialize");
        let reparsed = parse_scene(&yaml).expect("serialized DSL scene must parse");
        let (FractalConfig::Dsl(original), FractalConfig::Dsl(round_trip)) =
            (&scene.config.fractal, &reparsed.config.fractal)
        else {
            panic!("DSL kind must survive round trip");
        };
        assert_eq!(round_trip, original);

        let injected = TWISTED_MANDELBOX_DSL.replacen(
            "iterations: 18",
            "iterations: 18\n    wgsl: 'fn map() {}'",
            1,
        );
        let error = parse_scene(&injected).expect_err("raw WGSL fields must be rejected");
        assert!(error.to_string().contains("scene schema"));
    }

    #[test]
    fn publication_scene_preserves_deep_zoom_effect_scale() {
        let scene = parse_scene(MANDELBOX_FIRST_DESCENT_YOUTUBE)
            .expect("YouTube publication scene must parse");
        let animation = scene.animation.as_ref().expect("animation must be present");
        let video = scene.video.as_ref().expect("video must be configured");

        assert_eq!(scene.name, "mandelbox-first-descent-youtube");
        assert_eq!(scene.config.fractal.kind(), FractalKind::Dsl);
        assert_eq!(
            (scene.config.render.width, scene.config.render.height),
            (2560, 1440)
        );
        assert_eq!(scene.config.quality.samples_per_pixel, 32);
        assert_eq!((animation.fps, animation.frame_count), (60, 1_441));
        assert_eq!(video.crf, 14);

        let final_frame = animation
            .sample(&scene.config, animation.frame_count - 1)
            .expect("final publication frame must be representable");
        let deep_distance = final_frame.camera_distance.to_f32();
        assert!((deep_distance - 2.0e-5).abs() < 1.0e-11);
        assert!((final_frame.config.camera.focus_distance - deep_distance).abs() < 1.0e-11);
        assert!(
            (final_frame.config.camera.aperture_radius / deep_distance - 0.035 / 11.0).abs()
                < 1.0e-6
        );
        assert!(
            (final_frame.config.quality.ambient_occlusion.radius / deep_distance - 1.10 / 11.0)
                .abs()
                < 1.0e-6
        );
    }

    #[test]
    fn alchemy_scene_preserves_the_mandelbulber_hybrid_schedule() {
        let scene =
            parse_scene(ALCHEMY_PSEUDO_KLEINIAN).expect("Alchemy scene must parse and validate");
        let FractalConfig::Dsl(fractal) = &scene.config.fractal else {
            panic!("Alchemy scene must use the typed DSL");
        };

        assert_eq!(scene.name, "alchemy-pseudo-kleinian");
        assert_eq!(fractal.iterations, 125);
        assert_eq!(fractal.orbit_period, Some(120));
        assert_eq!(fractal.color_iterations, 500);
        assert_eq!(fractal.orbit.len(), 2);
        assert_eq!(fractal.material.surface_palette.len(), 13);
        assert_eq!(scene.config.render.width, 1620);
        assert_eq!(scene.config.render.height, 1080);
        assert_eq!(scene.config.quality.samples_per_pixel, 128);
        assert!((scene.config.camera.aperture_radius - 0.015).abs() < 1.0e-7);
        assert!((scene.config.camera.focus_distance - 0.778_781_06).abs() < 1.0e-7);
        assert_eq!(
            scene.config.quality.tone_mapping.operator,
            ToneMappingOperator::Mandelbulber
        );
        assert!((scene.config.quality.tone_mapping.brightness - 1.2).abs() < 1.0e-7);
        assert!((scene.config.quality.tone_mapping.contrast - 1.08).abs() < 1.0e-7);
        assert!((scene.config.quality.tone_mapping.gamma - 1.4).abs() < 1.0e-7);
        assert!((scene.config.quality.tone_mapping.saturation - 0.82).abs() < 1.0e-7);
        assert!(scene.config.quality.post_process.enabled);
        assert!((scene.config.quality.post_process.exposure_stops - 0.05).abs() < 1.0e-7);
        assert!((scene.config.quality.post_process.contrast - 0.96).abs() < 1.0e-7);
        assert!((scene.config.quality.post_process.saturation - 0.92).abs() < 1.0e-7);
        assert!((scene.config.quality.post_process.gamma - 1.0).abs() < 1.0e-7);
        assert!((scene.config.quality.post_process.vignette_strength - 0.08).abs() < 1.0e-7);

        let source = fractal.generate_wgsl().expect("hybrid WGSL must generate");
        assert!(source.contains("scheduled_iteration = iteration % 120u"));
        assert!(source.contains("scheduled_iteration >= 0u && scheduled_iteration < 20u"));
        assert!(source.contains("scheduled_iteration >= 20u && scheduled_iteration < 120u"));
        assert!(source.contains("dsl_orbit(p, 500u, false).color_coordinate"));
        assert!(source.contains("auxiliary_color += select"));

        let target_distance =
            fractal.distance_estimate(scene.config.camera.target.to_f32().map(f64::from));
        assert!(target_distance.is_finite() && target_distance < 1.0e-7);

        let yaml = scene.to_yaml().expect("Alchemy scene must serialize");
        let reparsed = parse_scene(&yaml).expect("serialized Alchemy scene must parse");
        assert_eq!(reparsed.config.fractal.kind(), FractalKind::Dsl);
    }

    #[test]
    fn alchemy_target_orbit_preserves_the_static_scene_look() {
        let static_scene = parse_scene(ALCHEMY_PSEUDO_KLEINIAN)
            .expect("static Alchemy scene must parse and validate");
        let orbit_scene = parse_scene(ALCHEMY_PSEUDO_KLEINIAN_TARGET_ORBIT)
            .expect("Alchemy target-orbit scene must parse and validate");

        let (FractalConfig::Dsl(static_fractal), FractalConfig::Dsl(orbit_fractal)) =
            (&static_scene.config.fractal, &orbit_scene.config.fractal)
        else {
            panic!("both Alchemy scenes must use the typed DSL");
        };
        assert_eq!(orbit_fractal, static_fractal);
        assert_eq!(
            orbit_scene.config.light.direction,
            static_scene.config.light.direction
        );
        assert_eq!(
            orbit_scene.config.camera.target,
            static_scene.config.camera.target
        );
        assert_eq!(
            orbit_scene.config.camera.vertical_fov_degrees,
            static_scene.config.camera.vertical_fov_degrees
        );
        assert_eq!(
            orbit_scene.config.quality.samples_per_pixel,
            static_scene.config.quality.samples_per_pixel
        );
        assert_eq!(
            orbit_scene.config.quality.tone_mapping.operator,
            ToneMappingOperator::Mandelbulber
        );

        let animation = orbit_scene
            .animation
            .as_ref()
            .expect("orbit example must be animated");
        let AnimationPath::TargetOrbit(path) = &animation.path else {
            panic!("Alchemy animation must use target-orbit");
        };
        assert_eq!((animation.fps, animation.frame_count), (30, 721));
        assert_eq!(path.cone_angle_degrees, 90.0);
        assert!((path.revolutions - 50.0 / 360.0).abs() < 1.0e-10);
        for (axis, camera_up) in path.axis.into_iter().zip(orbit_scene.config.camera.up) {
            assert!((axis - f64::from(camera_up)).abs() < 1.0e-7);
        }
        let first = animation.sample(&orbit_scene.config, 0).unwrap();
        let last = animation.sample(&orbit_scene.config, 720).unwrap();
        assert_eq!(first.config.camera.target, orbit_scene.config.camera.target);
        assert_ne!(first.config.camera.position, last.config.camera.position);
        let first_direction = (first.config.camera.position - first.config.camera.target)
            .normalized_to_f32()
            .unwrap();
        let last_direction = (last.config.camera.position - last.config.camera.target)
            .normalized_to_f32()
            .unwrap();
        let direction_dot = first_direction
            .into_iter()
            .zip(last_direction)
            .map(|(left, right)| f64::from(left) * f64::from(right))
            .sum::<f64>();
        assert!((direction_dot - 50_f64.to_radians().cos()).abs() < 1.0e-6);
        for (sampled, original) in first
            .config
            .camera
            .position
            .to_f32()
            .into_iter()
            .zip(static_scene.config.camera.position.to_f32())
        {
            assert!((sampled - original).abs() < 1.0e-6);
        }
    }

    #[test]
    fn rejects_quad_float_for_unsupported_fractals() {
        let yaml = MANDELBULB.replacen("precision: f32", "precision: quad-float", 1);
        let error = parse_scene(&yaml).expect_err("Mandelbulb has no quad-float shader");
        assert!(error.to_string().contains("scene configuration is invalid"));
    }

    #[test]
    fn parses_exact_quad_float_coordinates_for_mandelbox() {
        let yaml = MANDELBOX
            .replacen("precision: f32", "precision: quad-float", 1)
            .replacen(
                "position: [11.417633, -1.764267, 5.605854]",
                "position: [\"11.4176330000000000000000000001\", -1.764267, 5.605854]",
                1,
            );
        let scene = parse_scene(&yaml).expect("quad-float Mandelbox must parse");
        assert_eq!(scene.config.precision, Precision::QuadFloat);
        let residual = scene.config.camera.position.x
            - Qf32::from_str("11.417633").expect("baseline must parse");
        assert!(residual > Qf32::ZERO);
    }

    #[test]
    fn deep_scene_requires_quad_float_to_preserve_its_camera() {
        let scene = parse_scene(MANDELBOX_QUAD_DEEP).expect("deep scene must parse");
        assert_ne!(scene.config.camera.position, scene.config.camera.target);
        assert_eq!(
            scene.config.camera.position.to_f32(),
            scene.config.camera.target.to_f32()
        );
        let serialized = scene.to_yaml().expect("quad scene must serialize");
        let reparsed = parse_scene(&serialized).expect("quad scene must round trip");
        assert_eq!(reparsed.config.precision, Precision::QuadFloat);
        assert_eq!(
            reparsed.config.camera.position,
            scene.config.camera.position
        );
        assert_eq!(reparsed.config.camera.target, scene.config.camera.target);

        let f32_yaml = MANDELBOX_QUAD_DEEP.replacen("quad-float", "f32", 1);
        let error = parse_scene(&f32_yaml).expect_err("f32 must collapse the deep camera");
        assert!(error.to_string().contains("scene configuration is invalid"));
    }

    #[test]
    fn parses_and_round_trips_quad_float_animation() {
        let scene = parse_scene(MANDELBOX_QUAD_ZOOM).expect("zoom scene must parse");
        let animation = scene.animation.as_ref().expect("animation must be present");
        let video = scene
            .video
            .as_ref()
            .expect("video settings must be present");
        assert_eq!(animation.fps, 60);
        assert_eq!(animation.frame_count, 1_621);
        assert_eq!(video, &VideoConfig::default());
        let FractalConfig::Mandelbox(fractal) = &scene.config.fractal else {
            unreachable!();
        };
        assert_eq!(
            scene.config.camera.target.x,
            Qf32::from_f32(2.0 * fractal.fold_limit)
        );
        let final_frame = animation
            .sample(&scene.config, animation.frame_count - 1)
            .expect("final animation frame must be representable");
        assert_eq!(final_frame.time_seconds, 27.0);
        assert_eq!(
            final_frame.camera_distance,
            Qf32::from_str("1e-26").unwrap()
        );
        let deep_distance = final_frame.camera_distance.to_f32();
        assert_eq!(final_frame.config.camera.focus_distance, deep_distance);
        assert!(
            (final_frame.config.camera.aperture_radius / deep_distance - 0.06 / 11.0).abs()
                < 1.0e-6
        );
        assert!(
            (final_frame.config.quality.ambient_occlusion.radius / deep_distance - 1.25 / 11.0)
                .abs()
                < 1.0e-6
        );

        let yaml = scene.to_yaml().expect("animation scene must serialize");
        let reparsed = parse_scene(&yaml).expect("serialized animation must parse");
        let reparsed_animation = reparsed
            .animation
            .expect("animation must survive round trip");
        assert_eq!(reparsed.video, Some(VideoConfig::default()));
        let reparsed_final = reparsed_animation
            .sample(&reparsed.config, reparsed_animation.frame_count - 1)
            .unwrap();
        assert_eq!(reparsed_final.camera_distance, final_frame.camera_distance);
    }

    #[test]
    fn automatic_multi_target_dive_plans_and_switches_targets() {
        let scene = parse_scene(MANDELBOX_MULTI_TARGET_DIVE)
            .expect("multi-target animation must plan and validate");
        let animation = scene.animation.as_ref().expect("animation must exist");
        let AnimationPath::MultiTargetDive(path) = &animation.path else {
            panic!("scene must retain the multi-target path kind");
        };
        assert_eq!(path.target_count(), 3);

        let first = animation.sample(&scene.config, 0).unwrap();
        let blackout = animation.sample(&scene.config, 285).unwrap();
        let second = animation.sample(&scene.config, 300).unwrap();
        assert_ne!(first.config.camera.target, second.config.camera.target);
        assert!(blackout.config.quality.post_process.enabled);
        assert!(blackout.config.quality.post_process.exposure_stops <= -15.9);

        let yaml = scene.to_yaml().expect("automatic path must serialize");
        let reparsed = parse_scene(&yaml).expect("serialized automatic path must replan");
        let reparsed_first = reparsed
            .animation
            .as_ref()
            .unwrap()
            .sample(&reparsed.config, 0)
            .unwrap();
        assert_eq!(
            reparsed_first.config.camera.target,
            first.config.camera.target
        );
    }

    #[test]
    fn automatic_surface_flyover_keeps_motion_in_the_tangent_plane() {
        let scene =
            parse_scene(MANDELBOX_SURFACE_FLYOVER).expect("surface flyover must plan and validate");
        let animation = scene.animation.as_ref().expect("animation must exist");
        let AnimationPath::SurfaceFlyover(path) = &animation.path else {
            panic!("scene must retain the surface-flyover path kind");
        };
        let start = animation.sample(&scene.config, 0).unwrap();
        let end = animation.sample(&scene.config, 540).unwrap();
        let displacement = end.config.camera.position.to_f64();
        let origin = start.config.camera.position.to_f64();
        let displacement = [
            displacement[0] - origin[0],
            displacement[1] - origin[1],
            displacement[2] - origin[2],
        ];
        let normal_motion = displacement[0] * path.normal[0]
            + displacement[1] * path.normal[1]
            + displacement[2] * path.normal[2];
        assert!(normal_motion.abs() < 1.0e-6);
        assert!((start.camera_distance.to_f64() - end.camera_distance.to_f64()).abs() < 1.0e-9);
        assert_ne!(start.config.camera.position, end.config.camera.position);
    }

    #[test]
    fn target_orbit_scene_round_trips_and_preserves_its_cone() {
        let scene = parse_scene(MANDELBULB_TARGET_ORBIT)
            .expect("target-orbit example must parse and validate");
        let animation = scene.animation.as_ref().expect("animation must exist");
        let AnimationPath::TargetOrbit(path) = &animation.path else {
            panic!("scene must retain the target-orbit path kind");
        };
        assert!((path.radius.to_f64() - 4.2).abs() < 1.0e-12);
        assert_eq!(path.cone_angle_degrees, 35.0);

        let start = animation.sample(&scene.config, 0).unwrap();
        let quarter = animation.sample(&scene.config, 90).unwrap();
        let end = animation.sample(&scene.config, 360).unwrap();
        assert_eq!(start.config.camera.target, quarter.config.camera.target);
        assert_ne!(start.config.camera.position, quarter.config.camera.position);
        assert_eq!(start.config.camera.position, end.config.camera.position);

        let yaml = scene.to_yaml().expect("target orbit must serialize");
        let reparsed = parse_scene(&yaml).expect("serialized target orbit must parse");
        let reparsed_quarter = reparsed
            .animation
            .as_ref()
            .unwrap()
            .sample(&reparsed.config, 90)
            .unwrap();
        assert_eq!(
            reparsed_quarter.config.camera.position,
            quarter.config.camera.position
        );
        assert_eq!(reparsed_quarter.config.camera.up, quarter.config.camera.up);
    }

    #[test]
    fn rejects_video_without_an_animation() {
        let yaml = format!("{MANDELBULB}\nvideo: {{}}\n");
        let error = parse_scene(&yaml).expect_err("static scenes cannot request video encoding");
        assert!(error.to_string().contains("video configuration requires"));
    }

    #[test]
    fn video_section_accepts_documented_defaults() {
        let (scene_without_video, _) = MANDELBOX_QUAD_ZOOM
            .split_once("\nvideo:\n")
            .expect("example has a video section");
        let yaml = format!("{scene_without_video}\nvideo: {{}}\n");
        let scene = parse_scene(&yaml).expect("empty video section must use defaults");
        assert_eq!(scene.video, Some(VideoConfig::default()));
    }

    #[test]
    fn rejects_animation_beyond_the_measured_quad_depth() {
        let yaml = MANDELBOX_QUAD_ZOOM.replacen(
            "minimum_distance: \"1e-26\"",
            "minimum_distance: \"1e-27\"",
            1,
        );
        let error = parse_scene(&yaml).expect_err("unverified depth must fail");
        assert!(
            error
                .to_string()
                .contains("animation configuration is invalid")
        );
    }

    #[test]
    fn reports_domain_validation_errors() {
        let yaml = MANDELBULB.replacen("width: 640", "width: 0", 1);
        let error = parse_scene(&yaml).expect_err("zero width must fail");
        assert!(error.to_string().contains("scene configuration is invalid"));
    }
}
