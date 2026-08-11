//! Headless GPU renderer for distance-estimated 3D fractals.

mod config;
mod renderer;
mod shader;

pub use config::{CameraConfig, FractalConfig, LightConfig, RenderConfig, RenderSettings};
pub use renderer::{RenderedImage, Renderer, RendererOptions, adapter_is_software};
pub use wgpu::{AdapterInfo, Backend, DeviceType};
