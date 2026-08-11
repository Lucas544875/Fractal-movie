//! Headless GPU renderer for distance-estimated 3D fractals.

mod config;
mod fractal;
mod path;
mod renderer;
mod scene;
mod shader;

pub use config::{
    CameraConfig, FractalConfig, FractalKind, LightConfig, MandelboxConfig, MandelbulbConfig,
    RenderConfig, RenderSettings,
};
pub use fractal::DistanceEstimator;
pub use path::{ExponentialDivePath, PathTarget, TargetPicker, TargetSearchConfig};
pub use renderer::{RenderedImage, Renderer, RendererOptions, adapter_is_software};
pub use wgpu::{AdapterInfo, Backend, DeviceType};
