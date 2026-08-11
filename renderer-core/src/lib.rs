//! Headless GPU renderer for distance-estimated 3D fractals.

mod config;
mod fractal;
mod path;
mod precision;
mod renderer;
mod scene;
mod scene_file;
mod shader;

pub use config::{
    CameraConfig, FractalConfig, FractalKind, LightConfig, MIN_QUAD_CAMERA_DISTANCE,
    MandelboxConfig, MandelbulbConfig, Precision, RenderConfig, RenderSettings,
};
pub use fractal::{DistanceEstimator, HighPrecisionDistanceEstimator};
pub use path::{ExponentialDivePath, PathTarget, TargetPicker, TargetSearchConfig};
pub use precision::{Qf32, QfParseError, QfVec3};
pub use renderer::{RenderedImage, Renderer, RendererOptions, adapter_is_software};
pub use scene_file::{CURRENT_SCENE_VERSION, LoadedScene, load_scene, parse_scene};
pub use wgpu::{AdapterInfo, Backend, DeviceType};
