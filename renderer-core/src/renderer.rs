use std::{
    borrow::Cow,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::{RenderConfig, ToneMappingOperator, shader};

const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const BYTES_PER_PIXEL: u32 = 4;
/// Bounds one GPU submission so expensive offline scenes do not trip the
/// operating system's graphics watchdog. Fragment positions remain global
/// because scissoring does not change the render target coordinate system.
const MAX_FRAGMENT_INVOCATIONS_PER_SUBMISSION: u32 = 131_072;
/// A duty-cycle cap needs shorter submissions so it can yield to other GPU
/// clients regularly instead of alternating multi-second bursts and sleeps.
const MAX_THROTTLED_FRAGMENT_INVOCATIONS_PER_SUBMISSION: u32 = 16_384;

/// Controls which adapter may be used by [`Renderer`].
#[derive(Clone, Debug, Default)]
pub struct RendererOptions {
    /// Permit a CPU/software rasterizer such as llvmpipe.
    ///
    /// This is disabled by default so that a render never silently claims to
    /// be GPU accelerated while actually running on the CPU.
    pub allow_software_adapter: bool,
    /// Case-insensitive substring used to select a particular adapter.
    ///
    /// When omitted, `WGPU_ADAPTER_NAME` is used if it is set. Otherwise the
    /// highest-ranked hardware adapter is selected.
    pub adapter_name: Option<String>,
    /// Average fraction of wall time this renderer may keep the GPU busy.
    ///
    /// Values are in `0.01..=1.0`. Each completed render strip is followed by
    /// a proportional host-side pause. This bounds this renderer's average
    /// duty cycle; it does not control instantaneous hardware utilization or
    /// account for unrelated GPU processes.
    pub gpu_duty_cycle: Option<f64>,
}

impl RendererOptions {
    fn validate(&self) -> Result<()> {
        if let Some(duty_cycle) = self.gpu_duty_cycle
            && (!duty_cycle.is_finite() || !(0.01..=1.0).contains(&duty_cycle))
        {
            return Err(anyhow!("GPU duty cycle must be finite and in 0.01..=1.0"));
        }
        Ok(())
    }
}

/// Returns whether an adapter is known to be backed by software rendering.
#[must_use]
pub fn adapter_is_software(info: &wgpu::AdapterInfo) -> bool {
    if info.device_type == wgpu::DeviceType::Cpu {
        return true;
    }

    // Some drivers have historically reported `Other` instead of `Cpu`.
    // Keep this list deliberately narrow to avoid rejecting virtualized GPUs.
    let name = info.name.to_ascii_lowercase();
    [
        "llvmpipe",
        "lavapipe",
        "swiftshader",
        "software rasterizer",
        "microsoft basic render",
    ]
    .iter()
    .any(|software_name| name.contains(software_name))
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct RenderUniforms {
    resolution_time_frame: [f32; 4],
    camera_position_fov: [f32; 4],
    camera_target: [f32; 4],
    fractal_primary: [f32; 4],
    render_params: [f32; 4],
    limits: [u32; 4],
    light_direction: [f32; 4],
    camera_up: [f32; 4],
    camera_position_qf_x: [f32; 4],
    camera_position_qf_y: [f32; 4],
    camera_position_qf_z: [f32; 4],
    camera_target_qf_x: [f32; 4],
    camera_target_qf_y: [f32; 4],
    camera_target_qf_z: [f32; 4],
    // x: aperture radius, y: focus distance
    camera_lens: [f32; 4],
    // x: AO radius, y: AO strength
    ambient_occlusion: [f32; 4],
    // x: angular radius (radians), y: maximum trace distance
    soft_shadow: [f32; 4],
    // x: strength, y: roughness, z: maximum trace distance
    reflection: [f32; 4],
    // x: exposure stops, y: white point, z: enabled, w: operator
    tone_mapping: [f32; 4],
    // x: brightness, y: contrast, z: gamma, w: saturation
    image_adjustments: [f32; 4],
    // x: exposure stops, y: contrast, z: saturation, w: gamma
    post_process: [f32; 4],
    // x: enabled, y: vignette strength
    post_process_effects: [f32; 4],
    // x: samples, y: AO steps, z: shadow steps, w: reflection steps
    quality_limits: [u32; 4],
}

impl RenderUniforms {
    fn new(config: &RenderConfig, frame_index: u32, time_seconds: f32) -> Self {
        let camera_position = config.camera.position.to_f32();
        let camera_target = config.camera.target.to_f32();
        Self {
            resolution_time_frame: [
                config.render.width as f32,
                config.render.height as f32,
                time_seconds,
                frame_index as f32,
            ],
            camera_position_fov: [
                camera_position[0],
                camera_position[1],
                camera_position[2],
                config.camera.vertical_fov_degrees.to_radians(),
            ],
            camera_target: [camera_target[0], camera_target[1], camera_target[2], 0.0],
            fractal_primary: config.fractal.shader_parameters(),
            render_params: [
                config.render.epsilon,
                config.render.max_distance,
                config.render.step_safety,
                config.render.pixel_epsilon_multiplier,
            ],
            limits: [
                config.fractal.iterations(),
                config.render.max_steps,
                config.seed,
                0,
            ],
            light_direction: [
                config.light.direction[0],
                config.light.direction[1],
                config.light.direction[2],
                0.0,
            ],
            camera_up: [
                config.camera.up[0],
                config.camera.up[1],
                config.camera.up[2],
                0.0,
            ],
            camera_position_qf_x: config.camera.position.x.limbs(),
            camera_position_qf_y: config.camera.position.y.limbs(),
            camera_position_qf_z: config.camera.position.z.limbs(),
            camera_target_qf_x: config.camera.target.x.limbs(),
            camera_target_qf_y: config.camera.target.y.limbs(),
            camera_target_qf_z: config.camera.target.z.limbs(),
            camera_lens: [
                config.camera.aperture_radius,
                config.camera.focus_distance,
                0.0,
                0.0,
            ],
            ambient_occlusion: [
                config.quality.ambient_occlusion.radius,
                config.quality.ambient_occlusion.strength,
                0.0,
                0.0,
            ],
            soft_shadow: [
                config
                    .quality
                    .soft_shadow
                    .angular_radius_degrees
                    .to_radians(),
                config.quality.soft_shadow.max_distance,
                0.0,
                0.0,
            ],
            reflection: [
                config.quality.reflection.strength,
                config.quality.reflection.roughness,
                config.quality.reflection.max_distance,
                0.0,
            ],
            tone_mapping: [
                config.quality.tone_mapping.exposure_stops,
                config.quality.tone_mapping.white_point,
                u32::from(config.quality.tone_mapping.enabled) as f32,
                match config.quality.tone_mapping.operator {
                    ToneMappingOperator::ExtendedReinhard => 0.0,
                    ToneMappingOperator::Mandelbulber => 1.0,
                },
            ],
            image_adjustments: [
                config.quality.tone_mapping.brightness,
                config.quality.tone_mapping.contrast,
                config.quality.tone_mapping.gamma,
                config.quality.tone_mapping.saturation,
            ],
            post_process: [
                config.quality.post_process.exposure_stops,
                config.quality.post_process.contrast,
                config.quality.post_process.saturation,
                config.quality.post_process.gamma,
            ],
            post_process_effects: [
                u32::from(config.quality.post_process.enabled) as f32,
                config.quality.post_process.vignette_strength,
                0.0,
                0.0,
            ],
            quality_limits: [
                config.quality.samples_per_pixel,
                config.quality.ambient_occlusion.max_steps,
                config.quality.soft_shadow.max_steps,
                config.quality.reflection.max_steps,
            ],
        }
    }
}

/// An uncompressed, tightly packed RGBA8 render result.
#[derive(Debug)]
pub struct RenderedImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

/// Pixel-space portion of the configured camera viewport to render.
///
/// Camera projection still uses the complete render resolution, so a region
/// is a true crop of the final image rather than a resized or reframed view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl RenderRegion {
    #[must_use]
    pub const fn full(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    fn validate(self, width: u32, height: u32) -> Result<()> {
        let right = self.x.checked_add(self.width);
        let bottom = self.y.checked_add(self.height);
        if self.width == 0
            || self.height == 0
            || right.is_none_or(|right| right > width)
            || bottom.is_none_or(|bottom| bottom > height)
        {
            return Err(anyhow!(
                "render region x={}, y={}, width={}, height={} is outside {}x{}",
                self.x,
                self.y,
                self.width,
                self.height,
                width,
                height
            ));
        }
        Ok(())
    }
}

impl RenderedImage {
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
}

/// Reusable offscreen wgpu renderer.
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_info: wgpu::AdapterInfo,
    config: RenderConfig,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    render_pipeline: wgpu::RenderPipeline,
    target: wgpu::Texture,
    readback_buffer: wgpu::Buffer,
    padded_bytes_per_row: u32,
    gpu_duty_cycle: Option<f64>,
}

impl Renderer {
    /// Initializes a headless adapter, validates WGSL, and allocates resources.
    pub async fn new(config: RenderConfig) -> Result<Self> {
        Self::new_with_options(config, RendererOptions::default()).await
    }

    /// Lists every adapter exposed by the enabled wgpu backends.
    ///
    /// `WGPU_BACKEND` is honored, so this can also be used to compare Vulkan
    /// and OpenGL/GLES driver paths on Linux and WSL.
    pub async fn available_adapters() -> Vec<wgpu::AdapterInfo> {
        let (instance, backends) = create_instance();
        instance
            .enumerate_adapters(backends)
            .await
            .into_iter()
            .map(|adapter| adapter.get_info())
            .collect()
    }

    /// Initializes the renderer with an explicit adapter policy.
    pub async fn new_with_options(config: RenderConfig, options: RendererOptions) -> Result<Self> {
        config.validate().context("invalid render configuration")?;
        options.validate().context("invalid renderer options")?;

        let (instance, backends) = create_instance();
        let adapter = select_adapter(&instance, backends, &options).await?;
        let adapter_info = adapter.get_info();
        let adapter_limits = adapter.limits();
        if config.render.width > adapter_limits.max_texture_dimension_2d
            || config.render.height > adapter_limits.max_texture_dimension_2d
        {
            return Err(anyhow!(
                "requested resolution {}x{} exceeds adapter '{}' 2D texture limit {}",
                config.render.width,
                config.render.height,
                adapter_info.name,
                adapter_limits.max_texture_dimension_2d
            ));
        }

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("fractal-renderer-device"),
                required_features: wgpu::Features::empty(),
                // Asking for the adapter's reported limits also supports older
                // headless Vulkan implementations whose attachment count is
                // below the modern WebGPU default. This renderer only uses one.
                required_limits: adapter_limits,
                ..Default::default()
            })
            .await
            .with_context(|| format!("failed to open GPU adapter {}", adapter_info.name))?;

        let uniforms = RenderUniforms::new(&config, 0, 0.0);
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("render-uniform-buffer"),
            contents: bytemuck::bytes_of(&uniforms),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("render-bind-group-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("render-bind-group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("raymarch-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader_source = shader::fractal_source(&config.fractal, config.precision)
            .context("could not generate the fractal WGSL module")?;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fractal-shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Owned(shader_source)),
        });
        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("raymarch-render-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: TARGET_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        if let Some(error) = error_scope.pop().await {
            return Err(anyhow!(
                "WGSL shader or render pipeline validation failed: {error}"
            ));
        }

        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen-render-target"),
            size: wgpu::Extent3d {
                width: config.render.width,
                height: config.render.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let padded_bytes_per_row = padded_bytes_per_row(config.render.width);
        let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pixel-readback-buffer"),
            size: u64::from(padded_bytes_per_row) * u64::from(config.render.height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        Ok(Self {
            device,
            queue,
            adapter_info,
            config,
            uniform_buffer,
            bind_group,
            render_pipeline,
            target,
            readback_buffer,
            padded_bytes_per_row,
            gpu_duty_cycle: options.gpu_duty_cycle,
        })
    }

    #[must_use]
    pub fn adapter_info(&self) -> &wgpu::AdapterInfo {
        &self.adapter_info
    }

    /// Reports whether the selected adapter is backed by hardware.
    #[must_use]
    pub fn is_hardware_accelerated(&self) -> bool {
        !adapter_is_software(&self.adapter_info)
    }

    /// Configured average GPU duty-cycle cap, as a fraction in `0.01..=1.0`.
    #[must_use]
    pub const fn gpu_duty_cycle(&self) -> Option<f64> {
        self.gpu_duty_cycle
    }

    /// Reports whether a frame can reuse this renderer's pipeline and
    /// allocations. Uniform-only changes such as camera, light, and tone
    /// mapping return true; resolution, precision, or DSL AST changes do not.
    #[must_use]
    pub fn supports_config(&self, config: &RenderConfig) -> bool {
        config.render.width == self.config.render.width
            && config.render.height == self.config.render.height
            && config.precision == self.config.precision
            && config.fractal.shader_compatible_with(&self.config.fractal)
    }

    /// Renders one deterministic frame and reads it back from GPU memory.
    pub fn render_frame(&self, frame_index: u32, time_seconds: f32) -> Result<RenderedImage> {
        self.render_frame_with_config(&self.config, frame_index, time_seconds)
    }

    /// Renders a frame with updated uniforms while reusing the pipeline,
    /// texture, and readback allocation created from the initial config.
    ///
    /// Animation may change camera, DE parameters, lighting, and march limits;
    /// shader selection and resolution must remain compatible.
    pub fn render_frame_with_config(
        &self,
        config: &RenderConfig,
        frame_index: u32,
        time_seconds: f32,
    ) -> Result<RenderedImage> {
        self.render_region_with_config(
            config,
            frame_index,
            time_seconds,
            RenderRegion::full(config.render.width, config.render.height),
        )
    }

    /// Renders only a rectangular crop while retaining the complete camera
    /// projection. Fragment work outside the region is skipped.
    pub fn render_region_with_config(
        &self,
        config: &RenderConfig,
        frame_index: u32,
        time_seconds: f32,
        region: RenderRegion,
    ) -> Result<RenderedImage> {
        if !time_seconds.is_finite() || time_seconds < 0.0 {
            return Err(anyhow!("frame time must be finite and non-negative"));
        }
        config
            .validate()
            .context("invalid per-frame render configuration")?;
        if !self.supports_config(config) {
            return Err(anyhow!(
                "per-frame resolution, fractal shader configuration, and precision must match the renderer pipeline"
            ));
        }
        region
            .validate(config.render.width, config.render.height)
            .context("invalid render region")?;

        let uniforms = RenderUniforms::new(config, frame_index, time_seconds);
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let view = self
            .target
            .create_view(&wgpu::TextureViewDescriptor::default());
        let fragment_limit = if self.gpu_duty_cycle.is_some_and(|limit| limit < 1.0) {
            MAX_THROTTLED_FRAGMENT_INVOCATIONS_PER_SUBMISSION
        } else {
            MAX_FRAGMENT_INVOCATIONS_PER_SUBMISSION
        };
        for (strip_index, (strip_y, strip_height)) in
            render_strips_with_limit(region.width, region.height, fragment_limit)
                .into_iter()
                .enumerate()
        {
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("raymarch-strip-command-encoder"),
                });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    // `None` avoids debug-marker entry points on old headless
                    // Vulkan loaders while preserving resource labels elsewhere.
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: if strip_index == 0 {
                                wgpu::LoadOp::Clear(wgpu::Color::BLACK)
                            } else {
                                wgpu::LoadOp::Load
                            },
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.render_pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_scissor_rect(region.x, region.y + strip_y, region.width, strip_height);
                pass.draw(0..3, 0..1);
            }
            let gpu_work_started = Instant::now();
            self.queue.submit([encoder.finish()]);
            self.device
                .poll(wgpu::PollType::wait_indefinitely())
                .context("failed while waiting for a GPU render strip")?;
            if let Some(duty_cycle) = self.gpu_duty_cycle {
                thread::sleep(throttle_delay(gpu_work_started.elapsed(), duty_cycle));
            }
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("raymarch-readback-command-encoder"),
            });
        encoder.copy_texture_to_buffer(
            self.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_bytes_per_row),
                    rows_per_image: Some(self.config.render.height),
                },
            },
            self.target.size(),
        );
        self.queue.submit([encoder.finish()]);

        let slice = self.readback_buffer.slice(..);
        let (sender, receiver) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .context("failed while waiting for GPU readback")?;
        receiver
            .recv()
            .context("GPU readback callback ended unexpectedly")?
            .context("GPU readback buffer mapping failed")?;

        let mapped = slice
            .get_mapped_range()
            .context("GPU readback buffer was not mapped")?;
        let region_bytes_per_row = region.width as usize * BYTES_PER_PIXEL as usize;
        let region_start = region.x as usize * BYTES_PER_PIXEL as usize;
        let region_end = region_start + region_bytes_per_row;
        let mut pixels = Vec::with_capacity(region_bytes_per_row * region.height as usize);
        for row in mapped
            .chunks_exact(self.padded_bytes_per_row as usize)
            .skip(region.y as usize)
            .take(region.height as usize)
        {
            pixels.extend_from_slice(&row[region_start..region_end]);
        }
        drop(mapped);
        self.readback_buffer.unmap();

        Ok(RenderedImage {
            width: region.width,
            height: region.height,
            pixels,
        })
    }
}

fn render_strips_with_limit(
    width: u32,
    height: u32,
    max_fragment_invocations: u32,
) -> Vec<(u32, u32)> {
    let strip_height = (max_fragment_invocations / width).max(1);
    (0..height)
        .step_by(strip_height as usize)
        .map(|y| (y, strip_height.min(height - y)))
        .collect()
}

fn throttle_delay(active_time: Duration, duty_cycle: f64) -> Duration {
    if duty_cycle >= 1.0 || active_time.is_zero() {
        return Duration::ZERO;
    }
    active_time.mul_f64((1.0 - duty_cycle) / duty_cycle)
}

fn create_instance() -> (wgpu::Instance, wgpu::Backends) {
    let descriptor = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
    let backends = descriptor.backends;
    (wgpu::Instance::new(descriptor), backends)
}

async fn select_adapter(
    instance: &wgpu::Instance,
    backends: wgpu::Backends,
    options: &RendererOptions,
) -> Result<wgpu::Adapter> {
    let adapters = instance.enumerate_adapters(backends).await;
    let detected = adapters
        .iter()
        .map(|adapter| adapter.get_info())
        .collect::<Vec<_>>();
    let name_filter = options
        .adapter_name
        .clone()
        .or_else(|| std::env::var("WGPU_ADAPTER_NAME").ok())
        .filter(|name| !name.trim().is_empty());

    let mut matching = adapters
        .into_iter()
        .filter(|adapter| {
            name_filter.as_ref().is_none_or(|filter| {
                adapter
                    .get_info()
                    .name
                    .to_ascii_lowercase()
                    .contains(&filter.to_ascii_lowercase())
            })
        })
        .collect::<Vec<_>>();

    if matching.is_empty() {
        let requested = name_filter
            .as_deref()
            .map(|name| format!(" matching '{name}'"))
            .unwrap_or_default();
        return Err(anyhow!(
            "no compatible wgpu adapter{requested} was found; detected adapters: {}",
            adapter_summary(&detected)
        ));
    }

    if !options.allow_software_adapter {
        let matching_infos = matching
            .iter()
            .map(|adapter| adapter.get_info())
            .collect::<Vec<_>>();
        matching.retain(|adapter| !adapter_is_software(&adapter.get_info()));
        if matching.is_empty() {
            return Err(anyhow!(
                "GPU acceleration is unavailable: only software adapters were found ({}). \
                 Fix the system GPU/driver exposure, or pass --allow-software only when CPU \
                 rendering is intentional",
                adapter_summary(&matching_infos)
            ));
        }
    }

    matching
        .into_iter()
        .max_by_key(|adapter| adapter_rank(&adapter.get_info()))
        .context("no compatible wgpu adapter remained after applying the selection policy")
}

fn adapter_summary(infos: &[wgpu::AdapterInfo]) -> String {
    if infos.is_empty() {
        return "none".to_owned();
    }

    infos
        .iter()
        .map(|info| format!("{} ({:?}, {:?})", info.name, info.backend, info.device_type))
        .collect::<Vec<_>>()
        .join(", ")
}

fn adapter_rank(info: &wgpu::AdapterInfo) -> (u8, u8) {
    let device_rank = match info.device_type {
        wgpu::DeviceType::DiscreteGpu => 5,
        wgpu::DeviceType::IntegratedGpu => 4,
        wgpu::DeviceType::VirtualGpu => 3,
        wgpu::DeviceType::Other => 2,
        wgpu::DeviceType::Cpu => 1,
    };
    let backend_rank = match info.backend {
        wgpu::Backend::Vulkan | wgpu::Backend::Metal | wgpu::Backend::Dx12 => 2,
        wgpu::Backend::Gl => 1,
        _ => 0,
    };
    (device_rank, backend_rank)
}

fn padded_bytes_per_row(width: u32) -> u32 {
    let unpadded = width * BYTES_PER_PIXEL;
    let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    unpadded.div_ceil(alignment) * alignment
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::{AnimationConfig, AnimationPath, ExponentialDivePath, Precision, Qf32};

    #[test]
    fn row_pitch_is_aligned_without_truncating_pixels() {
        assert_eq!(padded_bytes_per_row(64), 256);
        assert_eq!(padded_bytes_per_row(65), 512);
        assert_eq!(padded_bytes_per_row(640), 2_560);
    }

    #[test]
    fn render_region_rejects_empty_overflowing_and_out_of_bounds_crops() {
        assert!(RenderRegion::full(640, 360).validate(640, 360).is_ok());
        assert!(
            RenderRegion {
                x: 160,
                y: 90,
                width: 320,
                height: 180,
            }
            .validate(640, 360)
            .is_ok()
        );
        for invalid in [
            RenderRegion {
                x: 0,
                y: 0,
                width: 0,
                height: 1,
            },
            RenderRegion {
                x: 639,
                y: 0,
                width: 2,
                height: 1,
            },
            RenderRegion {
                x: u32::MAX,
                y: 0,
                width: 2,
                height: 1,
            },
        ] {
            assert!(invalid.validate(640, 360).is_err());
        }
    }

    #[test]
    fn render_height_is_split_into_watchdog_safe_strips() {
        assert_eq!(
            render_strips_with_limit(1_620, 1_080, MAX_FRAGMENT_INVOCATIONS_PER_SUBMISSION),
            vec![
                (0, 80),
                (80, 80),
                (160, 80),
                (240, 80),
                (320, 80),
                (400, 80),
                (480, 80),
                (560, 80),
                (640, 80),
                (720, 80),
                (800, 80),
                (880, 80),
                (960, 80),
                (1_040, 40),
            ]
        );
        assert_eq!(
            render_strips_with_limit(256, 256, MAX_FRAGMENT_INVOCATIONS_PER_SUBMISSION),
            vec![(0, 256)]
        );
        assert_eq!(
            render_strips_with_limit(200_000, 2, MAX_FRAGMENT_INVOCATIONS_PER_SUBMISSION),
            vec![(0, 1), (1, 1)]
        );
    }

    #[test]
    fn throttled_rendering_uses_shorter_strips() {
        let strips = render_strips_with_limit(
            1_620,
            1_080,
            MAX_THROTTLED_FRAGMENT_INVOCATIONS_PER_SUBMISSION,
        );
        assert_eq!(strips.len(), 108);
        assert_eq!(strips.first(), Some(&(0, 10)));
        assert_eq!(strips.last(), Some(&(1_070, 10)));
    }

    #[test]
    fn throttle_delay_enforces_the_requested_average_duty_cycle() {
        assert_eq!(
            throttle_delay(Duration::from_millis(100), 0.4),
            Duration::from_millis(150)
        );
        assert_eq!(
            throttle_delay(Duration::from_millis(100), 0.25),
            Duration::from_millis(300)
        );
        assert_eq!(
            throttle_delay(Duration::from_millis(100), 1.0),
            Duration::ZERO
        );
    }

    #[test]
    fn renderer_options_reject_invalid_duty_cycles() {
        assert!(RendererOptions::default().validate().is_ok());
        for duty_cycle in [0.0, 0.009, 1.001, f64::NAN, f64::INFINITY] {
            let options = RendererOptions {
                gpu_duty_cycle: Some(duty_cycle),
                ..RendererOptions::default()
            };
            assert!(options.validate().is_err());
        }
    }

    #[test]
    fn identifies_cpu_and_known_software_adapters() {
        let cpu = wgpu::AdapterInfo::new(wgpu::DeviceType::Cpu, wgpu::Backend::Vulkan);
        assert!(adapter_is_software(&cpu));

        let mut mislabeled = wgpu::AdapterInfo::new(wgpu::DeviceType::Other, wgpu::Backend::Vulkan);
        mislabeled.name = "llvmpipe (LLVM 12.0.0)".to_owned();
        assert!(adapter_is_software(&mislabeled));

        let mut virtual_gpu =
            wgpu::AdapterInfo::new(wgpu::DeviceType::VirtualGpu, wgpu::Backend::Gl);
        virtual_gpu.name = "D3D12 (NVIDIA GeForce)".to_owned();
        assert!(!adapter_is_software(&virtual_gpu));
    }

    #[test]
    fn ranks_hardware_before_software() {
        let discrete = wgpu::AdapterInfo::new(wgpu::DeviceType::DiscreteGpu, wgpu::Backend::Vulkan);
        let integrated = wgpu::AdapterInfo::new(wgpu::DeviceType::IntegratedGpu, wgpu::Backend::Gl);
        let cpu = wgpu::AdapterInfo::new(wgpu::DeviceType::Cpu, wgpu::Backend::Vulkan);

        assert!(adapter_rank(&discrete) > adapter_rank(&integrated));
        assert!(adapter_rank(&integrated) > adapter_rank(&cpu));
    }

    #[test]
    #[ignore = "requires a hardware GPU"]
    fn region_render_matches_the_same_pixels_from_a_full_frame() {
        let mut config = RenderConfig::default();
        config.render.width = 64;
        config.render.height = 36;
        let renderer = pollster::block_on(Renderer::new(config.clone())).expect("GPU renderer");
        let full = renderer.render_frame(0, 0.0).expect("full render");
        let region = RenderRegion {
            x: 16,
            y: 9,
            width: 32,
            height: 18,
        };
        let crop = renderer
            .render_region_with_config(&config, 0, 0.0, region)
            .expect("region render");
        let full_row_bytes = full.width() as usize * BYTES_PER_PIXEL as usize;
        let crop_row_bytes = crop.width() as usize * BYTES_PER_PIXEL as usize;
        let crop_start = region.x as usize * BYTES_PER_PIXEL as usize;
        let mut expected = Vec::with_capacity(crop.pixels().len());
        for row in full
            .pixels()
            .chunks_exact(full_row_bytes)
            .skip(region.y as usize)
            .take(region.height as usize)
        {
            expected.extend_from_slice(&row[crop_start..crop_start + crop_row_bytes]);
        }
        assert_eq!(crop.pixels(), expected);
    }

    #[test]
    #[ignore = "requires a hardware GPU"]
    fn quad_float_overview_matches_f32_golden() {
        let mut ordinary = RenderConfig::mandelbox(12_345);
        ordinary.render.width = 96;
        ordinary.render.height = 54;
        let mut precise = ordinary.clone();
        precise.precision = Precision::QuadFloat;

        let ordinary_renderer = pollster::block_on(Renderer::new(ordinary)).expect("f32 renderer");
        let precise_renderer = pollster::block_on(Renderer::new(precise)).expect("quad renderer");
        let ordinary_image = ordinary_renderer.render_frame(0, 0.0).expect("f32 image");
        let precise_image = precise_renderer.render_frame(0, 0.0).expect("quad image");
        let normalized_rmse = normalized_rmse(ordinary_image.pixels(), precise_image.pixels());
        assert!(
            normalized_rmse < 1.0e-3,
            "overview regression RMSE {normalized_rmse} exceeds tolerance"
        );
    }

    #[test]
    #[ignore = "requires a hardware GPU"]
    fn quad_float_reaches_measured_depth_at_regression_resolution() {
        let mut config =
            RenderConfig::mandelbox_quad(12_345, Qf32::from_f64(1.0e-26)).expect("limit scene");
        assert_eq!(
            config.camera.position.to_f32(),
            config.camera.target.to_f32()
        );
        config.render.width = 320;
        config.render.height = 180;
        let renderer = pollster::block_on(Renderer::new(config)).expect("quad renderer");
        let image = renderer.render_frame(0, 0.0).expect("deep image");
        let non_black = image
            .pixels()
            .chunks_exact(4)
            .filter(|pixel| pixel[..3].iter().any(|channel| *channel != 0))
            .count();
        let unique_colors = unique_rgb_colors(image.pixels());
        assert!(
            non_black > 50_000,
            "deep quad scene rendered only {non_black} surface pixels"
        );
        assert!(
            unique_colors >= 16,
            "deep quad scene produced only {unique_colors} RGB colors"
        );
    }

    #[test]
    #[ignore = "requires a hardware GPU"]
    fn quad_float_animation_reuses_renderer_across_zoom_depths() {
        let mut base =
            RenderConfig::mandelbox_quad(12_345, Qf32::from_f32(11.0)).expect("overview scene");
        base.render.width = 320;
        base.render.height = 180;
        let animation = AnimationConfig {
            fps: 1,
            frame_count: 3,
            path: AnimationPath::ExponentialDive(ExponentialDivePath {
                overview_distance: Qf32::from_f32(11.0),
                minimum_distance: Qf32::from_f64(1.0e-26),
                overview_duration: 0.0,
                dive_duration: 2.0,
            }),
        };
        animation.validate(&base).expect("animation must be valid");
        let initial = animation.sample(&base, 0).unwrap();
        let renderer = pollster::block_on(Renderer::new(initial.config)).expect("quad renderer");

        for frame_index in 0..animation.frame_count {
            let sample = animation.sample(&base, frame_index).unwrap();
            let image = renderer
                .render_frame_with_config(&sample.config, frame_index, sample.time_seconds as f32)
                .expect("animated frame");
            let unique_colors = unique_rgb_colors(image.pixels());
            assert!(
                unique_colors >= 16,
                "animation frame {frame_index} produced only {unique_colors} RGB colors"
            );
        }
    }

    #[test]
    #[ignore = "requires a hardware GPU"]
    fn generated_dsl_shader_renders_with_phase_five_effects() {
        let mut config = crate::parse_scene(include_str!(
            "../../scenes/examples/twisted-mandelbox-dsl.yaml"
        ))
        .expect("DSL example")
        .config;
        config.render.width = 96;
        config.render.height = 54;
        config.quality.post_process.enabled = true;
        config.quality.post_process.exposure_stops = 0.25;
        config.quality.post_process.contrast = 1.1;
        config.quality.post_process.saturation = 0.9;
        config.quality.post_process.gamma = 1.05;
        config.quality.post_process.vignette_strength = 0.15;
        let renderer = pollster::block_on(Renderer::new(config)).expect("DSL renderer");
        let image = renderer.render_frame(0, 0.0).expect("DSL image");
        let unique_colors = unique_rgb_colors(image.pixels());
        assert!(
            unique_colors >= 32,
            "generated DSL shader produced only {unique_colors} RGB colors"
        );
    }

    fn unique_rgb_colors(pixels: &[u8]) -> usize {
        pixels
            .chunks_exact(4)
            .map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect::<BTreeSet<_>>()
            .len()
    }

    fn normalized_rmse(left: &[u8], right: &[u8]) -> f64 {
        assert_eq!(left.len(), right.len());
        let squared_error = left
            .iter()
            .zip(right)
            .map(|(left, right)| {
                let difference = f64::from(*left) - f64::from(*right);
                difference * difference
            })
            .sum::<f64>();
        (squared_error / left.len() as f64).sqrt() / 255.0
    }
}
