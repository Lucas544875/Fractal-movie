//! Headless GPU renderer for distance-estimated 3D fractals.

mod animation;
mod config;
mod dsl;
mod fractal;
mod path;
mod precision;
mod renderer;
mod scene;
mod scene_file;
mod shader;
mod video;

pub use animation::{
    AnimationConfig, AnimationFrame, AnimationPath, MAX_ANIMATION_FPS, MAX_ANIMATION_FRAMES,
};
pub use config::{
    AmbientOcclusionConfig, CameraConfig, FractalConfig, FractalKind, LightConfig,
    MAX_SAMPLES_PER_PIXEL, MAX_SECONDARY_RAY_STEPS, MIN_QUAD_CAMERA_DISTANCE, MandelboxConfig,
    MandelbulbConfig, Precision, QualityConfig, ReflectionConfig, RenderConfig, RenderSettings,
    SoftShadowConfig, ToneMappingConfig, ToneMappingOperator,
};
pub use dsl::{
    DslFractalConfig, DslMaterial, DslPaletteStop, MAX_DSL_COLOR_ITERATIONS, MAX_DSL_PALETTE_STOPS,
    MAX_DSL_TRANSFORMS, OrbitTransform,
};
pub use fractal::{DistanceEstimator, HighPrecisionDistanceEstimator};
pub use path::{ExponentialDivePath, PathTarget, TargetPicker, TargetSearchConfig};
pub use precision::{Qf32, QfParseError, QfVec3};
pub use renderer::{RenderedImage, Renderer, RendererOptions, adapter_is_software};
pub use scene_file::{CURRENT_SCENE_VERSION, LoadedScene, load_scene, parse_scene};
pub use video::{MAX_VIDEO_CRF, VideoConfig};
pub use wgpu::{AdapterInfo, Backend, DeviceType};
