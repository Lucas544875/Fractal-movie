mod output;
mod video;

use std::{
    collections::BTreeSet,
    fs,
    path::Path,
    path::PathBuf,
    process::ExitCode,
    str::FromStr,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use fractal_renderer_core::{
    AnimationConfig, AnimationFrame, AnimationPath, FractalConfig, FractalKind,
    MIN_QUAD_CAMERA_DISTANCE, MandelboxConfig, MandelbulbConfig, Precision, Qf32, RenderConfig,
    RenderRegion, Renderer, RendererOptions, VideoConfig, adapter_is_software, load_scene,
};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum FractalName {
    Mandelbulb,
    Mandelbox,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum PrecisionName {
    F32,
    QuadFloat,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum PreviewProfile {
    Composition,
    #[default]
    Lookdev,
    Proof,
    Final,
}

impl PreviewProfile {
    const fn label(self) -> &'static str {
        match self {
            Self::Composition => "composition",
            Self::Lookdev => "lookdev",
            Self::Proof => "proof",
            Self::Final => "final",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct NormalizedRegion {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl FromStr for NormalizedRegion {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        let components = value
            .split(',')
            .map(str::trim)
            .map(str::parse::<f64>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| "region must contain four comma-separated numbers".to_owned())?;
        let [x, y, width, height] = components.as_slice() else {
            return Err("region must be x,y,width,height".to_owned());
        };
        let region = Self {
            x: *x,
            y: *y,
            width: *width,
            height: *height,
        };
        region.validate().map_err(|error| error.to_string())?;
        Ok(region)
    }
}

impl NormalizedRegion {
    fn validate(self) -> Result<()> {
        if [self.x, self.y, self.width, self.height]
            .into_iter()
            .any(|value| !value.is_finite())
            || self.x < 0.0
            || self.y < 0.0
            || self.width <= 0.0
            || self.height <= 0.0
            || self.x + self.width > 1.0
            || self.y + self.height > 1.0
        {
            bail!(
                "preview region must be finite, positive, and contained in normalized 0.0..=1.0 coordinates"
            );
        }
        Ok(())
    }

    fn to_render_region(self, width: u32, height: u32) -> RenderRegion {
        let x = (self.x * f64::from(width)).floor() as u32;
        let y = (self.y * f64::from(height)).floor() as u32;
        let right = ((self.x + self.width) * f64::from(width)).ceil() as u32;
        let bottom = ((self.y + self.height) * f64::from(height)).ceil() as u32;
        RenderRegion {
            x: x.min(width - 1),
            y: y.min(height - 1),
            width: right.clamp(x + 1, width) - x,
            height: bottom.clamp(y + 1, height) - y,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "fractal-render",
    version,
    about = "Headless wgpu fractal renderer"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Command {
    /// Render a static PNG or a scene-defined PNG sequence.
    Render {
        /// Versioned YAML scene file. Omit to use a built-in preset.
        #[arg(value_name = "SCENE")]
        scene: Option<PathBuf>,

        /// Static PNG path, or output directory for an animated scene.
        #[arg(long)]
        output: Option<PathBuf>,

        /// Render only this zero-based frame from an animated scene.
        #[arg(long, value_name = "INDEX")]
        frame: Option<u32>,

        /// Skip valid existing PNG frames and continue an animated sequence.
        #[arg(long, conflicts_with = "overwrite")]
        resume: bool,

        /// Replace existing output PNG files.
        #[arg(long, conflicts_with = "resume")]
        overwrite: bool,

        /// Encode the complete animation sequence with FFmpeg.
        #[arg(long, conflicts_with = "no_video")]
        video: bool,

        /// Disable a scene-defined video encode.
        #[arg(long)]
        no_video: bool,

        /// Output video path. Defaults to the frame directory with .mp4.
        #[arg(long, value_name = "FILE", conflicts_with = "no_video")]
        video_output: Option<PathBuf>,

        /// Override the FFmpeg video encoder, for example libx264.
        #[arg(long, value_name = "NAME", conflicts_with = "no_video")]
        video_codec: Option<String>,

        /// Override the encoded pixel format, for example yuv420p.
        #[arg(long, value_name = "FORMAT", conflicts_with = "no_video")]
        video_pixel_format: Option<String>,

        /// Override the codec constant-quality value.
        #[arg(long, value_name = "VALUE", conflicts_with = "no_video")]
        video_crf: Option<u8>,

        /// Override the codec preset.
        #[arg(long, value_name = "NAME", conflicts_with = "no_video")]
        video_preset: Option<String>,

        /// Enable MP4/MOV fast-start metadata relocation.
        #[arg(long, conflicts_with_all = ["no_video", "no_video_faststart"])]
        video_faststart: bool,

        /// Disable fast-start metadata relocation.
        #[arg(long, conflicts_with = "no_video")]
        no_video_faststart: bool,

        /// Replace an existing video without replacing PNG frames.
        #[arg(long, conflicts_with = "no_video")]
        video_overwrite: bool,

        /// FFmpeg executable or path.
        #[arg(long, value_name = "PATH", conflicts_with = "no_video")]
        ffmpeg: Option<PathBuf>,

        /// Override the scene's fractal using its default parameters.
        #[arg(long, value_enum)]
        fractal: Option<FractalName>,

        /// Override coordinate precision.
        #[arg(long, value_enum)]
        precision: Option<PrecisionName>,

        /// Quad-float preset camera distance, parsed as an exact decimal.
        #[arg(long, value_name = "DECIMAL")]
        camera_distance: Option<Qf32>,

        /// Override the deterministic scene seed.
        #[arg(long)]
        seed: Option<u32>,

        /// Override image width in pixels.
        #[arg(long)]
        width: Option<u32>,

        /// Override image height in pixels.
        #[arg(long)]
        height: Option<u32>,

        /// Limit this renderer's average GPU duty cycle, as a percentage.
        #[arg(long, value_name = "PERCENT")]
        gpu_duty_cycle: Option<f64>,

        /// Permit CPU/software rendering when no hardware GPU is available.
        #[arg(long)]
        allow_software: bool,

        /// Select an adapter by a case-insensitive name substring.
        #[arg(long, value_name = "NAME")]
        adapter: Option<String>,
    },

    /// Rapidly render representative frames without modifying the scene.
    Preview {
        /// Versioned YAML scene file to preview.
        #[arg(value_name = "SCENE")]
        scene: PathBuf,

        /// Preview quality and fidelity tradeoff.
        #[arg(long, value_enum, default_value_t)]
        profile: PreviewProfile,

        /// Render one zero-based animation frame.
        #[arg(long, value_name = "INDEX", conflicts_with = "frames")]
        frame: Option<u32>,

        /// Render comma-separated animation frames. Defaults to five key views.
        #[arg(
            long,
            value_name = "INDICES",
            value_delimiter = ',',
            conflicts_with = "frame"
        )]
        frames: Vec<u32>,

        /// Re-render after the scene file changes. Outputs are replaced.
        #[arg(long)]
        watch: bool,

        /// Render a normalized crop as x,y,width,height while preserving projection.
        #[arg(long, value_name = "X,Y,W,H")]
        region: Option<NormalizedRegion>,

        /// Preview output directory.
        #[arg(long)]
        output: Option<PathBuf>,

        /// Override preview viewport width after applying the profile.
        #[arg(long)]
        width: Option<u32>,

        /// Override preview viewport height after applying the profile.
        #[arg(long)]
        height: Option<u32>,

        /// Limit this renderer's average GPU duty cycle, as a percentage.
        #[arg(long, value_name = "PERCENT")]
        gpu_duty_cycle: Option<f64>,

        /// Permit CPU/software rendering when no hardware GPU is available.
        #[arg(long)]
        allow_software: bool,

        /// Select an adapter by a case-insensitive name substring.
        #[arg(long, value_name = "NAME")]
        adapter: Option<String>,
    },

    /// List wgpu adapters and report whether hardware acceleration is available.
    GpuInfo,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Render {
            scene,
            output,
            frame,
            resume,
            overwrite,
            video,
            no_video,
            video_output,
            video_codec,
            video_pixel_format,
            video_crf,
            video_preset,
            video_faststart,
            no_video_faststart,
            video_overwrite,
            ffmpeg,
            fractal,
            precision,
            camera_distance,
            seed,
            width,
            height,
            gpu_duty_cycle,
            allow_software,
            adapter,
        } => render(RenderRequest {
            scene,
            output,
            frame,
            resume,
            overwrite,
            video,
            no_video,
            video_output,
            video_codec,
            video_pixel_format,
            video_crf,
            video_preset,
            video_faststart,
            no_video_faststart,
            video_overwrite,
            ffmpeg,
            fractal,
            precision,
            camera_distance,
            seed,
            width,
            height,
            gpu_duty_cycle,
            allow_software,
            adapter,
        }),
        Command::Preview {
            scene,
            profile,
            frame,
            frames,
            watch,
            region,
            output,
            width,
            height,
            gpu_duty_cycle,
            allow_software,
            adapter,
        } => preview(PreviewRequest {
            scene,
            profile,
            frame,
            frames,
            watch,
            region,
            output,
            width,
            height,
            gpu_duty_cycle,
            allow_software,
            adapter,
        }),
        Command::GpuInfo => gpu_info(),
    }
}

#[derive(Default)]
struct RenderRequest {
    scene: Option<PathBuf>,
    output: Option<PathBuf>,
    frame: Option<u32>,
    resume: bool,
    overwrite: bool,
    video: bool,
    no_video: bool,
    video_output: Option<PathBuf>,
    video_codec: Option<String>,
    video_pixel_format: Option<String>,
    video_crf: Option<u8>,
    video_preset: Option<String>,
    video_faststart: bool,
    no_video_faststart: bool,
    video_overwrite: bool,
    ffmpeg: Option<PathBuf>,
    fractal: Option<FractalName>,
    precision: Option<PrecisionName>,
    camera_distance: Option<Qf32>,
    seed: Option<u32>,
    width: Option<u32>,
    height: Option<u32>,
    gpu_duty_cycle: Option<f64>,
    allow_software: bool,
    adapter: Option<String>,
}

struct PreviewRequest {
    scene: PathBuf,
    profile: PreviewProfile,
    frame: Option<u32>,
    frames: Vec<u32>,
    watch: bool,
    region: Option<NormalizedRegion>,
    output: Option<PathBuf>,
    width: Option<u32>,
    height: Option<u32>,
    gpu_duty_cycle: Option<f64>,
    allow_software: bool,
    adapter: Option<String>,
}

impl RenderRequest {
    fn has_video_configuration_option(&self) -> bool {
        self.video
            || self.video_output.is_some()
            || self.video_codec.is_some()
            || self.video_pixel_format.is_some()
            || self.video_crf.is_some()
            || self.video_preset.is_some()
            || self.video_faststart
            || self.no_video_faststart
            || self.ffmpeg.is_some()
    }

    fn has_any_video_option(&self) -> bool {
        self.no_video || self.video_overwrite || self.has_video_configuration_option()
    }
}

struct PreparedScene {
    name: String,
    config: RenderConfig,
    animation: Option<AnimationConfig>,
    video: Option<VideoConfig>,
    default_output: PathBuf,
}

fn resolve_video_job(
    scene_video: Option<&VideoConfig>,
    animation: &AnimationConfig,
    render_config: &RenderConfig,
    frames_directory: &Path,
    request: &RenderRequest,
) -> Result<Option<video::VideoJob>> {
    if request.no_video {
        return Ok(None);
    }
    let explicitly_requested = request.has_video_configuration_option();
    if request.frame.is_some() {
        if explicitly_requested || request.video_overwrite {
            bail!("video encoding requires the complete sequence and cannot be used with --frame");
        }
        return Ok(None);
    }
    if scene_video.is_none() && !explicitly_requested {
        if request.video_overwrite {
            bail!("--video-overwrite requires scene video settings or --video");
        }
        return Ok(None);
    }

    let mut config = scene_video.cloned().unwrap_or_default();
    if let Some(codec) = &request.video_codec {
        config.codec.clone_from(codec);
    }
    if let Some(pixel_format) = &request.video_pixel_format {
        config.pixel_format.clone_from(pixel_format);
    }
    if let Some(crf) = request.video_crf {
        config.crf = crf;
    }
    if let Some(preset) = &request.video_preset {
        config.preset.clone_from(preset);
    }
    if request.video_faststart {
        config.faststart = true;
    } else if request.no_video_faststart {
        config.faststart = false;
    }

    let output_path = request
        .video_output
        .clone()
        .unwrap_or_else(|| frames_directory.with_extension("mp4"));
    let job = video::VideoJob {
        ffmpeg: request
            .ffmpeg
            .clone()
            .unwrap_or_else(|| PathBuf::from("ffmpeg")),
        frames_directory: frames_directory.to_owned(),
        output_path,
        fps: animation.fps,
        frame_count: animation.frame_count,
        config,
        overwrite: request.overwrite || request.video_overwrite,
    };
    job.validate(render_config.render.width, render_config.render.height)
        .context("invalid effective video configuration")?;
    Ok(Some(job))
}

fn render(request: RenderRequest) -> Result<()> {
    gpu_duty_cycle_fraction(request.gpu_duty_cycle)?;
    let prepared = prepare_scene(
        request.scene.as_deref(),
        request.fractal,
        request.precision,
        request.camera_distance,
        request.seed,
        request.width,
        request.height,
    )?;
    let output_path = request
        .output
        .clone()
        .unwrap_or_else(|| prepared.default_output.clone());
    if prepared.animation.is_some() {
        render_animation(prepared, &request, &output_path)
    } else {
        render_static(prepared, &request, &output_path)
    }
}

fn preview(request: PreviewRequest) -> Result<()> {
    gpu_duty_cycle_fraction(request.gpu_duty_cycle)?;
    let mut renderer = None;
    render_preview_iteration(&request, &mut renderer)?;
    if !request.watch {
        return Ok(());
    }

    println!(
        "Watching {} for changes (Ctrl+C to stop)",
        request.scene.display()
    );
    let mut previous_contents = fs::read(&request.scene)
        .with_context(|| format!("could not monitor {}", request.scene.display()))?;
    loop {
        thread::sleep(Duration::from_millis(250));
        let Ok(contents) = fs::read(&request.scene) else {
            continue;
        };
        if contents == previous_contents {
            continue;
        }
        previous_contents = contents;
        println!("\nScene changed; refreshing preview...");
        if let Err(error) = render_preview_iteration(&request, &mut renderer) {
            eprintln!("preview refresh failed: {error:#}");
        }
    }
}

fn render_preview_iteration(
    request: &PreviewRequest,
    renderer: &mut Option<Renderer>,
) -> Result<()> {
    let mut prepared = prepare_scene(Some(&request.scene), None, None, None, None, None, None)?;
    apply_preview_profile(
        &mut prepared,
        request.profile,
        request.width,
        request.height,
    )?;
    let frame_indices =
        preview_frame_indices(prepared.animation.as_ref(), request.frame, &request.frames)?;
    let output_directory = request.output.clone().unwrap_or_else(|| {
        let directory = PathBuf::from("output")
            .join("preview")
            .join(&prepared.name)
            .join(request.profile.label());
        if request.region.is_some() {
            directory.join("crop")
        } else {
            directory
        }
    });
    let first_frame = preview_frame_config(&prepared, frame_indices[0])?;
    let rebuild_pipeline = renderer
        .as_ref()
        .is_none_or(|renderer| !renderer.supports_config(&first_frame.config));
    if rebuild_pipeline {
        *renderer = Some(create_renderer_with_options(
            first_frame.config.clone(),
            request.allow_software,
            request.adapter.clone(),
            request.gpu_duty_cycle,
        )?);
    }
    let renderer = renderer.as_ref().expect("preview renderer was initialized");

    println!("Scene: {}", prepared.name);
    println!("Preview profile: {}", request.profile.label());
    println!(
        "Pipeline: {}",
        if rebuild_pipeline {
            "rebuilt"
        } else {
            "reused"
        }
    );
    print_renderer_summary(renderer, &first_frame.config);
    let region = request.region.map_or_else(
        || {
            RenderRegion::full(
                first_frame.config.render.width,
                first_frame.config.render.height,
            )
        },
        |region| {
            region.to_render_region(
                first_frame.config.render.width,
                first_frame.config.render.height,
            )
        },
    );
    if request.region.is_some() {
        println!(
            "Preview crop: x={}, y={}, {}x{} within {}x{} viewport",
            region.x,
            region.y,
            region.width,
            region.height,
            first_frame.config.render.width,
            first_frame.config.render.height
        );
    }
    println!("Preview frames: {frame_indices:?}");
    println!("Output: {}", output_directory.display());

    for frame_index in frame_indices {
        let frame = if frame_index == first_frame.index {
            first_frame.clone()
        } else {
            preview_frame_config(&prepared, frame_index)?
        };
        let started = Instant::now();
        let image = renderer
            .render_region_with_config(
                &frame.config,
                frame.index,
                frame.time_seconds as f32,
                region,
            )
            .with_context(|| format!("failed to render preview frame {frame_index}"))?;
        let path = output_directory.join(format!("frame_{frame_index:06}.png"));
        output::save_png(&path, &image, true)
            .with_context(|| format!("failed to write preview {}", path.display()))?;
        println!(
            "Saved: {} ({:.3}s)",
            path.display(),
            started.elapsed().as_secs_f64()
        );
    }
    Ok(())
}

fn preview_frame_config(prepared: &PreparedScene, frame_index: u32) -> Result<AnimationFrame> {
    if let Some(animation) = &prepared.animation {
        return animation
            .sample(&prepared.config, frame_index)
            .with_context(|| format!("could not sample preview frame {frame_index}"));
    }
    if frame_index != 0 {
        bail!("static preview has only frame 0");
    }
    let camera_distance = (prepared.config.camera.target - prepared.config.camera.position)
        .length_squared()
        .sqrt();
    Ok(AnimationFrame {
        index: 0,
        time_seconds: 0.0,
        camera_distance,
        config: prepared.config.clone(),
    })
}

fn preview_frame_indices(
    animation: Option<&AnimationConfig>,
    frame: Option<u32>,
    frames: &[u32],
) -> Result<Vec<u32>> {
    let frame_count = animation.map_or(1, |animation| animation.frame_count);
    let requested = if let Some(frame) = frame {
        vec![frame]
    } else if !frames.is_empty() {
        frames.to_vec()
    } else if frame_count == 1 {
        vec![0]
    } else {
        let last = frame_count - 1;
        vec![0, last / 4, last / 2, last.saturating_mul(3) / 4, last]
    };
    let unique = requested.into_iter().collect::<BTreeSet<_>>();
    if let Some(invalid) = unique.iter().find(|&&frame| frame >= frame_count) {
        bail!(
            "preview frame {invalid} is outside this scene's frame range 0..{}",
            frame_count - 1
        );
    }
    Ok(unique.into_iter().collect())
}

fn apply_preview_profile(
    prepared: &mut PreparedScene,
    profile: PreviewProfile,
    width_override: Option<u32>,
    height_override: Option<u32>,
) -> Result<()> {
    let source_width = prepared.config.render.width;
    let source_height = prepared.config.render.height;
    let maximum_width = match profile {
        PreviewProfile::Composition => Some(320),
        PreviewProfile::Lookdev => Some(480),
        PreviewProfile::Proof => Some(810),
        PreviewProfile::Final => None,
    };
    if let Some(maximum_width) = maximum_width
        && source_width > maximum_width
    {
        prepared.config.render.width = maximum_width;
        prepared.config.render.height =
            scaled_dimension(source_height, source_width, maximum_width);
    }
    match (width_override, height_override) {
        (Some(width), Some(height)) => {
            prepared.config.render.width = width;
            prepared.config.render.height = height;
        }
        (Some(width), None) => {
            prepared.config.render.width = width;
            prepared.config.render.height = scaled_dimension(source_height, source_width, width);
        }
        (None, Some(height)) => {
            prepared.config.render.width = scaled_dimension(source_width, source_height, height);
            prepared.config.render.height = height;
        }
        (None, None) => {}
    }

    match profile {
        PreviewProfile::Composition => {
            prepared.config.quality.samples_per_pixel = 1;
            prepared.config.camera.aperture_radius = 0.0;
            prepared.config.render.max_steps = prepared.config.render.max_steps.min(256);
            prepared.config.quality.ambient_occlusion.max_steps = 0;
            prepared.config.quality.soft_shadow.max_steps = 0;
            prepared.config.quality.reflection.max_steps = 0;
        }
        PreviewProfile::Lookdev => {
            prepared.config.quality.samples_per_pixel =
                prepared.config.quality.samples_per_pixel.min(8);
            prepared.config.camera.aperture_radius = 0.0;
            prepared.config.quality.ambient_occlusion.max_steps =
                prepared.config.quality.ambient_occlusion.max_steps.min(24);
            prepared.config.quality.soft_shadow.max_steps =
                prepared.config.quality.soft_shadow.max_steps.min(32);
            prepared.config.quality.reflection.max_steps =
                prepared.config.quality.reflection.max_steps.min(16);
        }
        PreviewProfile::Proof => {
            prepared.config.quality.samples_per_pixel =
                prepared.config.quality.samples_per_pixel.min(32);
            prepared.config.quality.ambient_occlusion.max_steps =
                prepared.config.quality.ambient_occlusion.max_steps.min(48);
            prepared.config.quality.soft_shadow.max_steps =
                prepared.config.quality.soft_shadow.max_steps.min(64);
            prepared.config.quality.reflection.max_steps =
                prepared.config.quality.reflection.max_steps.min(32);
        }
        PreviewProfile::Final => {}
    }
    prepared
        .config
        .validate()
        .context("preview profile produced an invalid render configuration")?;
    if let Some(animation) = &prepared.animation {
        animation
            .validate(&prepared.config)
            .context("preview profile is incompatible with the animation")?;
    }
    Ok(())
}

fn scaled_dimension(value: u32, original: u32, resized: u32) -> u32 {
    ((u64::from(value) * u64::from(resized) + u64::from(original) / 2) / u64::from(original)).max(1)
        as u32
}

fn render_static(
    prepared: PreparedScene,
    request: &RenderRequest,
    output_path: &Path,
) -> Result<()> {
    if request.frame.is_some() {
        bail!("--frame requires a scene with an animation section");
    }
    if request.resume {
        bail!("--resume requires a scene with an animation section");
    }
    if request.has_any_video_option() {
        bail!("video options require a scene with an animation section");
    }
    ensure_output_is_available(output_path, request.overwrite)?;

    let scene_name = prepared.name;
    let config = prepared.config;
    let renderer = create_renderer(config.clone(), request)?;
    println!("Scene: {scene_name}");
    print_renderer_summary(&renderer, &config);
    println!("Frames: 1");
    println!("Rendering frame 1/1");

    let total_start = Instant::now();
    let frame_start = Instant::now();
    let image = renderer
        .render_frame(0, 0.0)
        .with_context(|| format!("failed to render {scene_name} frame 0"))?;
    let frame_time = frame_start.elapsed();
    println!("Frame render time: {:.3}s", frame_time.as_secs_f64());

    output::save_png(output_path, &image, request.overwrite)
        .with_context(|| format!("failed to write {}", output_path.display()))?;
    let total_time = total_start.elapsed();
    println!("Saved: {}", output_path.display());
    println!("Total render time: {:.3}s", total_time.as_secs_f64());
    println!("Average frame time: {:.3}s", frame_time.as_secs_f64());
    Ok(())
}

fn render_animation(
    prepared: PreparedScene,
    request: &RenderRequest,
    output_directory: &Path,
) -> Result<()> {
    let animation = prepared
        .animation
        .as_ref()
        .context("animated render was selected without an animation configuration")?;
    if output_directory.exists() && !output_directory.is_dir() {
        bail!(
            "animated output must be a directory, but {} is not",
            output_directory.display()
        );
    }
    let video_job = resolve_video_job(
        prepared.video.as_ref(),
        animation,
        &prepared.config,
        output_directory,
        request,
    )?;
    let ffmpeg_version = video_job
        .as_ref()
        .map(video::VideoJob::ffmpeg_version)
        .transpose()
        .context("FFmpeg is unavailable for the requested video encode")?;

    let requested_frames = if let Some(frame) = request.frame {
        if frame >= animation.frame_count {
            bail!(
                "--frame {frame} is outside this animation's frame range 0..{}",
                animation.frame_count
            );
        }
        vec![frame]
    } else {
        (0..animation.frame_count).collect()
    };
    let (pending_frames, skipped_frames) = plan_animation_outputs(
        &requested_frames,
        output_directory,
        prepared.config.render.width,
        prepared.config.render.height,
        request.resume,
        request.overwrite,
    )?;

    println!("Scene: {}", prepared.name);
    println!(
        "Animation: {} fps, {} frames",
        animation.fps, animation.frame_count
    );
    match &animation.path {
        AnimationPath::ExponentialDive(_) => println!("Path: exponential-dive"),
        AnimationPath::TargetOrbit(_) => {
            println!("Path: target-orbit (fixed-target conical orbit)")
        }
        AnimationPath::MultiTargetDive(path) => println!(
            "Path: multi-target-dive ({} preplanned targets)",
            path.target_count()
        ),
        AnimationPath::SurfaceFlyover(_) => {
            println!("Path: surface-flyover (DE normal + probed tangent)")
        }
    }
    println!("Output: {}", output_directory.display());
    if request.frame.is_some()
        && prepared.video.is_some()
        && !request.no_video
        && !request.has_video_configuration_option()
    {
        println!("Video: skipped because --frame renders only part of the sequence");
    }
    if let (Some(job), Some(version)) = (&video_job, &ffmpeg_version) {
        println!("FFmpeg: {version}");
        println!(
            "Video: {} (codec={}, pixel_format={}, crf={}, preset={})",
            job.output_path.display(),
            job.config.codec,
            job.config.pixel_format,
            job.config.crf,
            job.config.preset
        );
    }
    if skipped_frames > 0 {
        println!("Resume: skipped {skipped_frames} valid existing frame(s)");
    }
    if pending_frames.is_empty() {
        println!("Nothing to render; every requested frame is already complete.");
    } else {
        let initial_frame = animation.sample(&prepared.config, pending_frames[0])?;
        let renderer = create_renderer(initial_frame.config.clone(), request)?;
        print_renderer_summary(&renderer, &initial_frame.config);
        println!("Frames to render: {}", pending_frames.len());

        let total_start = Instant::now();
        let mut accumulated_frame_seconds = 0.0;
        for frame_index in pending_frames.iter().copied() {
            let sample = animation
                .sample(&prepared.config, frame_index)
                .with_context(|| format!("could not sample animation frame {frame_index}"))?;
            println!(
                "Rendering frame {}/{} (t={:.6}s, distance={:.6e})",
                frame_index + 1,
                animation.frame_count,
                sample.time_seconds,
                sample.camera_distance.to_f64()
            );
            let frame_start = Instant::now();
            let image = renderer
                .render_frame_with_config(&sample.config, sample.index, sample.time_seconds as f32)
                .with_context(|| {
                    format!("failed to render {} frame {frame_index}", prepared.name)
                })?;
            let frame_seconds = frame_start.elapsed().as_secs_f64();
            accumulated_frame_seconds += frame_seconds;
            println!("Frame render time: {frame_seconds:.3}s");

            let frame_path = animation_frame_path(output_directory, frame_index);
            output::save_png(&frame_path, &image, request.overwrite)
                .with_context(|| format!("failed to write {}", frame_path.display()))?;
            println!("Saved: {}", frame_path.display());
        }

        let total_seconds = total_start.elapsed().as_secs_f64();
        println!("Total render time: {total_seconds:.3}s");
        println!(
            "Average frame time: {:.3}s",
            accumulated_frame_seconds / pending_frames.len() as f64
        );
    }

    if let Some(job) = &video_job {
        println!(
            "Validating {} PNG frame(s) for FFmpeg",
            animation.frame_count
        );
        validate_complete_sequence(
            output_directory,
            animation.frame_count,
            prepared.config.render.width,
            prepared.config.render.height,
        )?;
        println!("Encoding video: {}", job.output_path.display());
        let encode_start = Instant::now();
        job.encode().context("video encoding failed")?;
        println!("Saved video: {}", job.output_path.display());
        println!(
            "Video encode time: {:.3}s",
            encode_start.elapsed().as_secs_f64()
        );
    }
    Ok(())
}

fn create_renderer(config: RenderConfig, request: &RenderRequest) -> Result<Renderer> {
    create_renderer_with_options(
        config,
        request.allow_software,
        request.adapter.clone(),
        request.gpu_duty_cycle,
    )
}

fn create_renderer_with_options(
    config: RenderConfig,
    allow_software: bool,
    adapter: Option<String>,
    gpu_duty_cycle: Option<f64>,
) -> Result<Renderer> {
    pollster::block_on(Renderer::new_with_options(
        config,
        RendererOptions {
            allow_software_adapter: allow_software,
            adapter_name: adapter,
            gpu_duty_cycle: gpu_duty_cycle_fraction(gpu_duty_cycle)?,
        },
    ))
    .context("could not initialize the offscreen renderer")
}

fn print_renderer_summary(renderer: &Renderer, config: &RenderConfig) {
    let adapter = renderer.adapter_info();
    println!(
        "Adapter: {} ({:?}, {:?})",
        adapter.name, adapter.backend, adapter.device_type
    );
    println!(
        "Acceleration: {}",
        if renderer.is_hardware_accelerated() {
            "hardware GPU"
        } else {
            "software (CPU)"
        }
    );
    if let Some(duty_cycle) = renderer.gpu_duty_cycle() {
        println!(
            "GPU duty-cycle cap: {:.1}% (average, strip-paced)",
            duty_cycle * 100.0
        );
    }
    println!("Fractal: {}", fractal_label(config.fractal.kind()));
    println!("Precision: {}", precision_label(config.precision));
    println!(
        "Resolution: {}x{}",
        config.render.width, config.render.height
    );
    println!(
        "Quality: {} spp, DOF={}, AO={}, soft-shadow={}, reflection={}, tone-mapping={}, post-process={}",
        config.quality.samples_per_pixel,
        enabled_label(config.camera.aperture_radius > 0.0),
        enabled_label(
            config.quality.ambient_occlusion.max_steps > 0
                && config.quality.ambient_occlusion.strength > 0.0
        ),
        enabled_label(config.quality.soft_shadow.max_steps > 0),
        enabled_label(
            config.quality.reflection.max_steps > 0 && config.quality.reflection.strength > 0.0
        ),
        enabled_label(config.quality.tone_mapping.enabled),
        enabled_label(config.quality.post_process.enabled),
    );
}

const fn enabled_label(enabled: bool) -> &'static str {
    if enabled { "on" } else { "off" }
}

fn gpu_duty_cycle_fraction(percent: Option<f64>) -> Result<Option<f64>> {
    let Some(percent) = percent else {
        return Ok(None);
    };
    if !percent.is_finite() || !(1.0..=100.0).contains(&percent) {
        bail!("--gpu-duty-cycle must be finite and in 1.0..=100.0 percent");
    }
    Ok(Some(percent / 100.0))
}

fn animation_frame_path(directory: &Path, frame_index: u32) -> PathBuf {
    directory.join(format!("frame_{frame_index:06}.png"))
}

fn ensure_output_is_available(path: &Path, overwrite: bool) -> Result<()> {
    if !path.exists() || overwrite {
        return Ok(());
    }
    bail!(
        "output {} already exists; pass --overwrite to replace it",
        path.display()
    )
}

fn plan_animation_outputs(
    requested_frames: &[u32],
    directory: &Path,
    width: u32,
    height: u32,
    resume: bool,
    overwrite: bool,
) -> Result<(Vec<u32>, usize)> {
    let mut pending = Vec::with_capacity(requested_frames.len());
    let mut skipped = 0;
    for &frame_index in requested_frames {
        let path = animation_frame_path(directory, frame_index);
        if !path.exists() || overwrite {
            pending.push(frame_index);
            continue;
        }
        if !resume {
            bail!(
                "output frame {} already exists; pass --resume to skip completed frames or --overwrite to replace them",
                path.display()
            );
        }
        let dimensions = output::png_dimensions(&path).with_context(|| {
            format!(
                "existing frame {} is not a valid resumable PNG; use --overwrite to replace it",
                path.display()
            )
        })?;
        if dimensions != (width, height) {
            bail!(
                "existing frame {} is {}x{}, expected {}x{}; use --overwrite to replace it",
                path.display(),
                dimensions.0,
                dimensions.1,
                width,
                height
            );
        }
        skipped += 1;
    }
    Ok((pending, skipped))
}

fn validate_complete_sequence(
    directory: &Path,
    frame_count: u32,
    width: u32,
    height: u32,
) -> Result<()> {
    for frame_index in 0..frame_count {
        let path = animation_frame_path(directory, frame_index);
        let dimensions = output::png_dimensions(&path).with_context(|| {
            format!(
                "animation frame {} is missing or invalid; PNG frames were preserved and FFmpeg was not started",
                path.display()
            )
        })?;
        if dimensions != (width, height) {
            bail!(
                "animation frame {} is {}x{}, expected {}x{}; PNG frames were preserved and FFmpeg was not started",
                path.display(),
                dimensions.0,
                dimensions.1,
                width,
                height
            );
        }
    }
    Ok(())
}

fn prepare_scene(
    scene_path: Option<&Path>,
    fractal_override: Option<FractalName>,
    precision_override: Option<PrecisionName>,
    camera_distance: Option<Qf32>,
    seed_override: Option<u32>,
    width_override: Option<u32>,
    height_override: Option<u32>,
) -> Result<PreparedScene> {
    let has_scene_file = scene_path.is_some();
    let (name, mut config, mut animation, video) = if let Some(path) = scene_path {
        if camera_distance.is_some() {
            bail!(
                "--camera-distance is available only with the built-in quad-float Mandelbox preset"
            );
        }
        let scene = load_scene(path)?;
        (scene.name, scene.config, scene.animation, scene.video)
    } else {
        let seed = seed_override.unwrap_or(12_345);
        let fractal = fractal_override.unwrap_or(FractalName::Mandelbulb);
        let precision = precision_override.unwrap_or(PrecisionName::F32);
        match (fractal, precision) {
            (FractalName::Mandelbox, PrecisionName::QuadFloat) => {
                let distance = camera_distance.unwrap_or_else(|| Qf32::from_f64(1.0e-12));
                let config = RenderConfig::mandelbox_quad(seed, distance)
                    .with_context(|| {
                        format!(
                            "built-in quad-float Mandelbox camera distance must be finite and at least {MIN_QUAD_CAMERA_DISTANCE:e}"
                        )
                    })?;
                ("mandelbox-quad".to_owned(), config, None, None)
            }
            (FractalName::Mandelbulb, PrecisionName::QuadFloat) => {
                bail!("quad-float precision is currently supported only for Mandelbox scenes");
            }
            (_, PrecisionName::F32) => {
                if camera_distance.is_some() {
                    bail!("--camera-distance requires --precision quad-float");
                }
                (
                    fractal.label().to_owned(),
                    fractal.built_in_config(seed),
                    None,
                    None,
                )
            }
        }
    };

    if has_scene_file {
        if let Some(fractal) = fractal_override {
            config.fractal = fractal.default_parameters();
        }
        if let Some(seed) = seed_override {
            config.seed = seed;
        }
        if let Some(precision) = precision_override {
            config.precision = precision.into();
        }
    }
    if let Some(width) = width_override {
        config.render.width = width;
    }
    if let Some(height) = height_override {
        config.render.height = height;
    }
    config
        .validate()
        .context("render configuration is invalid after applying CLI overrides")?;
    if let Some(animation) = &mut animation {
        if fractal_override.is_some() || seed_override.is_some() {
            animation
                .plan(&config)
                .context("could not replan automatic path after CLI overrides")?;
        }
        animation
            .validate(&config)
            .context("animation is invalid after applying CLI overrides")?;
    }
    let default_output = if animation.is_some() {
        PathBuf::from("output").join(&name)
    } else if has_scene_file {
        PathBuf::from("output")
            .join(&name)
            .join(format!("{name}.png"))
    } else {
        PathBuf::from("output/phase1").join(format!("{name}.png"))
    };
    Ok(PreparedScene {
        name,
        config,
        animation,
        video,
        default_output,
    })
}

impl FractalName {
    const fn label(self) -> &'static str {
        match self {
            Self::Mandelbulb => "mandelbulb",
            Self::Mandelbox => "mandelbox",
        }
    }

    fn built_in_config(self, seed: u32) -> RenderConfig {
        match self {
            Self::Mandelbulb => RenderConfig {
                seed,
                ..RenderConfig::default()
            },
            Self::Mandelbox => RenderConfig::mandelbox(seed),
        }
    }

    fn default_parameters(self) -> FractalConfig {
        match self {
            Self::Mandelbulb => FractalConfig::Mandelbulb(MandelbulbConfig::default()),
            Self::Mandelbox => FractalConfig::Mandelbox(MandelboxConfig::default()),
        }
    }
}

impl From<PrecisionName> for Precision {
    fn from(value: PrecisionName) -> Self {
        match value {
            PrecisionName::F32 => Self::F32,
            PrecisionName::QuadFloat => Self::QuadFloat,
        }
    }
}

const fn fractal_label(kind: FractalKind) -> &'static str {
    match kind {
        FractalKind::Mandelbulb => "mandelbulb",
        FractalKind::Mandelbox => "mandelbox",
        FractalKind::Dsl => "generated DSL",
    }
}

const fn precision_label(precision: Precision) -> &'static str {
    match precision {
        Precision::F32 => "f32",
        Precision::QuadFloat => "quad-float (4xf32)",
    }
}

fn gpu_info() -> Result<()> {
    let adapters = pollster::block_on(Renderer::available_adapters());
    if adapters.is_empty() {
        println!("Adapters: none");
        println!("Hardware acceleration: unavailable");
    } else {
        println!("Adapters:");
        for (index, adapter) in adapters.iter().enumerate() {
            let acceleration = if adapter_is_software(adapter) {
                "software"
            } else {
                "hardware"
            };
            println!(
                "  {}. {} ({:?}, {:?}, {acceleration})",
                index + 1,
                adapter.name,
                adapter.backend,
                adapter.device_type
            );
            if !adapter.driver.is_empty() || !adapter.driver_info.is_empty() {
                println!("     driver: {} {}", adapter.driver, adapter.driver_info);
            }
        }
        println!(
            "Hardware acceleration: {}",
            if adapters.iter().any(|adapter| !adapter_is_software(adapter)) {
                "available"
            } else {
                "unavailable"
            }
        );
    }

    if std::env::var_os("WSL_DISTRO_NAME").is_some() && !Path::new("/dev/dxg").exists() {
        println!(
            "WSL check: /dev/dxg is not visible; Windows GPU acceleration cannot be used here"
        );
    }
    if let Some(backends) = std::env::var_os("WGPU_BACKEND") {
        println!("WGPU_BACKEND: {}", backends.to_string_lossy());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use fractal_renderer_core::{AnimationPath, ExponentialDivePath};

    #[test]
    fn parses_and_validates_gpu_duty_cycle_percentages() {
        let cli =
            Cli::try_parse_from(["fractal-render", "render", "--gpu-duty-cycle", "37.5"]).unwrap();
        let Command::Render { gpu_duty_cycle, .. } = cli.command else {
            panic!("render subcommand must parse");
        };
        assert_eq!(
            gpu_duty_cycle_fraction(gpu_duty_cycle).unwrap(),
            Some(0.375)
        );
        assert_eq!(gpu_duty_cycle_fraction(None).unwrap(), None);
        for invalid in [0.0, 0.99, 100.01, f64::NAN, f64::INFINITY] {
            assert!(gpu_duty_cycle_fraction(Some(invalid)).is_err());
        }
    }

    #[test]
    fn preview_cli_accepts_profiles_frames_watch_and_regions() {
        let cli = Cli::try_parse_from([
            "fractal-render",
            "preview",
            "scene.yaml",
            "--profile",
            "proof",
            "--frames",
            "0,180,360",
            "--region",
            "0.25,0.2,0.5,0.6",
            "--watch",
        ])
        .unwrap();
        let Command::Preview {
            profile,
            frames,
            region,
            watch,
            ..
        } = cli.command
        else {
            panic!("preview subcommand must parse");
        };
        assert_eq!(profile, PreviewProfile::Proof);
        assert_eq!(frames, vec![0, 180, 360]);
        assert!(watch);
        assert_eq!(
            region.unwrap().to_render_region(1_620, 1_080),
            RenderRegion {
                x: 405,
                y: 216,
                width: 810,
                height: 648,
            }
        );
        assert!("0.8,0.0,0.3,1.0".parse::<NormalizedRegion>().is_err());
    }

    #[test]
    fn preview_selects_five_representative_animation_frames() {
        let animation = AnimationConfig {
            fps: 30,
            frame_count: 721,
            path: AnimationPath::ExponentialDive(ExponentialDivePath {
                overview_distance: Qf32::from_f32(1.0),
                minimum_distance: Qf32::from_f32(0.1),
                overview_duration: 0.0,
                dive_duration: 24.0,
            }),
        };
        assert_eq!(
            preview_frame_indices(Some(&animation), None, &[]).unwrap(),
            vec![0, 180, 360, 540, 720]
        );
        assert_eq!(
            preview_frame_indices(Some(&animation), None, &[360, 0, 360]).unwrap(),
            vec![0, 360]
        );
        assert!(preview_frame_indices(Some(&animation), Some(721), &[]).is_err());
        assert!(preview_frame_indices(None, Some(1), &[]).is_err());
    }

    #[test]
    fn preview_profiles_trade_quality_for_speed_without_changing_scene_source() {
        let mut source = RenderConfig::default();
        source.render.width = 1_620;
        source.render.height = 1_080;
        source.render.max_steps = 768;
        source.camera.aperture_radius = 0.015;
        source.quality.samples_per_pixel = 128;
        source.quality.ambient_occlusion.max_steps = 96;
        source.quality.soft_shadow.max_steps = 128;
        source.quality.reflection.max_steps = 64;

        let make_scene = || PreparedScene {
            name: "preview-test".to_owned(),
            config: source.clone(),
            animation: None,
            video: None,
            default_output: PathBuf::new(),
        };
        let mut composition = make_scene();
        apply_preview_profile(&mut composition, PreviewProfile::Composition, None, None).unwrap();
        assert_eq!(
            (
                composition.config.render.width,
                composition.config.render.height
            ),
            (320, 213)
        );
        assert_eq!(composition.config.quality.samples_per_pixel, 1);
        assert_eq!(composition.config.render.max_steps, 256);
        assert_eq!(composition.config.camera.aperture_radius, 0.0);

        let mut lookdev = make_scene();
        apply_preview_profile(&mut lookdev, PreviewProfile::Lookdev, None, None).unwrap();
        assert_eq!(
            (lookdev.config.render.width, lookdev.config.render.height),
            (480, 320)
        );
        assert_eq!(lookdev.config.quality.samples_per_pixel, 8);
        assert_eq!(lookdev.config.quality.ambient_occlusion.max_steps, 24);
        assert_eq!(lookdev.config.quality.soft_shadow.max_steps, 32);
        assert_eq!(lookdev.config.quality.reflection.max_steps, 16);

        let mut proof = make_scene();
        apply_preview_profile(&mut proof, PreviewProfile::Proof, None, None).unwrap();
        assert_eq!(
            (proof.config.render.width, proof.config.render.height),
            (810, 540)
        );
        assert_eq!(proof.config.quality.samples_per_pixel, 32);
        assert_eq!(proof.config.camera.aperture_radius, 0.015);

        let mut final_preview = make_scene();
        apply_preview_profile(&mut final_preview, PreviewProfile::Final, None, None).unwrap();
        assert_eq!(final_preview.config.render.width, 1_620);
        assert_eq!(final_preview.config.quality.samples_per_pixel, 128);
        assert_eq!(source.render.width, 1_620);
        assert_eq!(source.quality.samples_per_pixel, 128);
    }

    #[test]
    fn built_in_defaults_remain_backwards_compatible() {
        let scene =
            prepare_scene(None, None, None, None, None, None, None).expect("preset must be valid");
        assert_eq!(scene.name, "mandelbulb");
        assert_eq!(scene.config.seed, 12_345);
        assert_eq!(scene.config.render.width, 640);
        assert_eq!(
            scene.default_output,
            PathBuf::from("output/phase1/mandelbulb.png")
        );
    }

    #[test]
    fn scene_values_are_preserved_unless_explicitly_overridden() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../scenes/examples/mandelbox.yaml");
        let scene = prepare_scene(Some(&path), None, None, None, None, Some(800), None)
            .expect("example scene must be valid");
        assert_eq!(scene.name, "mandelbox");
        assert_eq!(scene.config.seed, 12_345);
        assert_eq!(scene.config.render.width, 800);
        assert_eq!(scene.config.render.height, 360);
        assert_eq!(scene.config.fractal.kind(), FractalKind::Mandelbox);
        assert_eq!(
            scene.default_output,
            PathBuf::from("output/mandelbox/mandelbox.png")
        );
    }

    #[test]
    fn explicit_fractal_and_seed_overrides_apply_to_scene_files() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../scenes/examples/mandelbox.yaml");
        let scene = prepare_scene(
            Some(&path),
            Some(FractalName::Mandelbulb),
            None,
            None,
            Some(99),
            None,
            None,
        )
        .expect("overridden scene must be valid");
        assert_eq!(scene.config.seed, 99);
        assert_eq!(scene.config.fractal.kind(), FractalKind::Mandelbulb);
    }

    #[test]
    fn built_in_quad_preset_accepts_exact_camera_distance() {
        let distance: Qf32 = "1e-12".parse().unwrap();
        let scene = prepare_scene(
            None,
            Some(FractalName::Mandelbox),
            Some(PrecisionName::QuadFloat),
            Some(distance),
            Some(7),
            Some(80),
            Some(45),
        )
        .expect("quad preset must be generated");
        assert_eq!(scene.config.precision, Precision::QuadFloat);
        assert_eq!(scene.config.seed, 7);
        assert_eq!(scene.config.render.width, 80);
        assert_ne!(scene.config.camera.position, scene.config.camera.target);
    }

    #[test]
    fn built_in_quad_preset_rejects_unverified_camera_depth() {
        let result = prepare_scene(
            None,
            Some(FractalName::Mandelbox),
            Some(PrecisionName::QuadFloat),
            Some("1e-27".parse().unwrap()),
            None,
            None,
            None,
        );
        let Err(error) = result else {
            panic!("depth beyond the measured limit must fail");
        };
        assert!(error.to_string().contains("at least 1e-26"));
    }

    #[test]
    fn animated_scene_uses_a_sequence_directory_and_supports_frame_sampling() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../scenes/examples/mandelbox-quad-zoom.yaml");
        let scene = prepare_scene(Some(&path), None, None, None, None, Some(80), Some(45))
            .expect("animation example must be valid");
        let animation = scene.animation.as_ref().expect("animation must be loaded");
        assert_eq!(animation.fps, 60);
        assert_eq!(animation.frame_count, 1_621);
        assert_eq!(
            scene.default_output,
            PathBuf::from("output/mandelbox-quad-zoom")
        );
        let final_frame = animation.sample(&scene.config, 1_620).unwrap();
        assert_eq!(final_frame.config.render.width, 80);
        assert_eq!(final_frame.config.render.height, 45);
        assert_ne!(
            final_frame.config.camera.position,
            final_frame.config.camera.target
        );
    }

    #[test]
    fn automatic_path_is_replanned_after_a_seed_override() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../scenes/examples/mandelbox-multi-target-dive.yaml");
        let original = prepare_scene(Some(&path), None, None, None, None, Some(80), Some(45))
            .expect("automatic scene must load");
        let overridden = prepare_scene(Some(&path), None, None, None, Some(99), Some(80), Some(45))
            .expect("seed override must replan the automatic path");
        let original_target = original
            .animation
            .as_ref()
            .unwrap()
            .sample(&original.config, 0)
            .unwrap()
            .config
            .camera
            .target;
        let overridden_target = overridden
            .animation
            .as_ref()
            .unwrap()
            .sample(&overridden.config, 0)
            .unwrap()
            .config
            .camera
            .target;
        assert_ne!(original_target, overridden_target);
    }

    #[test]
    fn effective_video_rejects_odd_yuv420p_resolution_overrides() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../scenes/examples/mandelbox-quad-zoom.yaml");
        let scene = prepare_scene(Some(&path), None, None, None, None, Some(80), Some(45)).unwrap();
        let request = RenderRequest::default();
        let error = resolve_video_job(
            scene.video.as_ref(),
            scene.animation.as_ref().unwrap(),
            &scene.config,
            Path::new("output/frames"),
            &request,
        )
        .expect_err("odd yuv420p dimensions must fail before rendering");
        assert!(error.to_string().contains("invalid effective video"));
    }

    #[test]
    fn resolves_scene_video_defaults_and_cli_overrides() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../scenes/examples/mandelbox-quad-zoom.yaml");
        let scene = prepare_scene(Some(&path), None, None, None, None, Some(80), Some(46)).unwrap();
        let request = RenderRequest {
            video_crf: Some(23),
            no_video_faststart: true,
            video_output: Some(PathBuf::from("/tmp/fractal-phase4-override.mp4")),
            ..RenderRequest::default()
        };
        let job = resolve_video_job(
            scene.video.as_ref(),
            scene.animation.as_ref().unwrap(),
            &scene.config,
            Path::new("/tmp/fractal-phase4-frames"),
            &request,
        )
        .unwrap()
        .expect("scene video must be enabled");
        assert_eq!(job.config.codec, "libx264");
        assert_eq!(job.config.crf, 23);
        assert!(!job.config.faststart);
        assert_eq!(
            job.output_path,
            PathBuf::from("/tmp/fractal-phase4-override.mp4")
        );
    }

    #[test]
    fn single_frame_safely_skips_implicit_video_but_rejects_explicit_encode() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../scenes/examples/mandelbox-quad-zoom.yaml");
        let scene = prepare_scene(Some(&path), None, None, None, None, Some(80), Some(46)).unwrap();
        let request = RenderRequest {
            frame: Some(0),
            ..RenderRequest::default()
        };
        assert!(
            resolve_video_job(
                scene.video.as_ref(),
                scene.animation.as_ref().unwrap(),
                &scene.config,
                Path::new("/tmp/fractal-phase4-frames"),
                &request,
            )
            .unwrap()
            .is_none()
        );

        let request = RenderRequest {
            frame: Some(0),
            video: true,
            ..RenderRequest::default()
        };
        assert!(
            resolve_video_job(
                scene.video.as_ref(),
                scene.animation.as_ref().unwrap(),
                &scene.config,
                Path::new("/tmp/fractal-phase4-frames"),
                &request,
            )
            .is_err()
        );
    }

    #[test]
    #[ignore = "requires a hardware GPU and FFmpeg"]
    fn renders_and_encodes_a_short_sequence_end_to_end() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "fractal-renderer-phase4-{}-{nonce}",
            std::process::id()
        ));
        let frames = root.join("frames");
        let mut config = RenderConfig::mandelbox_quad(12_345, Qf32::from_f32(11.0)).unwrap();
        config.render.width = 64;
        config.render.height = 36;
        let prepared = PreparedScene {
            name: "phase4-smoke".to_owned(),
            config,
            animation: Some(AnimationConfig {
                fps: 2,
                frame_count: 3,
                path: AnimationPath::ExponentialDive(ExponentialDivePath {
                    overview_distance: Qf32::from_f32(11.0),
                    minimum_distance: Qf32::from_f64(1.0e-14),
                    overview_duration: 0.0,
                    dive_duration: 1.0,
                }),
            }),
            video: Some(VideoConfig {
                preset: "ultrafast".to_owned(),
                ..VideoConfig::default()
            }),
            default_output: frames.clone(),
        };

        render_animation(prepared, &RenderRequest::default(), &frames).unwrap();
        for frame_index in 0..3_u32 {
            assert!(animation_frame_path(&frames, frame_index).exists());
        }
        let movie = frames.with_extension("mp4");
        assert!(std::fs::metadata(movie).unwrap().len() > 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sequence_frame_names_are_ffmpeg_compatible() {
        assert_eq!(
            animation_frame_path(Path::new("frames"), 120),
            PathBuf::from("frames/frame_000120.png")
        );
    }

    #[test]
    fn resume_skips_only_complete_frames_at_the_expected_resolution() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "fractal-renderer-resume-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let existing = animation_frame_path(&directory, 0);
        image::save_buffer_with_format(
            &existing,
            &[0_u8; 4 * 4 * 3],
            4,
            3,
            image::ColorType::Rgba8,
            image::ImageFormat::Png,
        )
        .unwrap();

        let (pending, skipped) =
            plan_animation_outputs(&[0, 1], &directory, 4, 3, true, false).unwrap();
        assert_eq!(pending, vec![1]);
        assert_eq!(skipped, 1);
        assert!(validate_complete_sequence(&directory, 2, 4, 3).is_err());
        assert!(plan_animation_outputs(&[0], &directory, 8, 3, true, false).is_err());
        assert!(plan_animation_outputs(&[0], &directory, 4, 3, false, false).is_err());

        let encoded = std::fs::read(&existing).unwrap();
        std::fs::write(&existing, &encoded[..encoded.len() / 2]).unwrap();
        assert!(
            plan_animation_outputs(&[0], &directory, 4, 3, true, false).is_err(),
            "resume must reject a truncated PNG even if its header is present"
        );
        assert_eq!(
            plan_animation_outputs(&[0], &directory, 4, 3, false, true)
                .unwrap()
                .0,
            vec![0]
        );

        std::fs::remove_dir_all(directory).unwrap();
    }
}
