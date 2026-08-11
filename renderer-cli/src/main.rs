mod output;
mod video;

use std::{path::Path, path::PathBuf, process::ExitCode, time::Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use fractal_renderer_core::{
    AnimationConfig, AnimationPath, FractalConfig, FractalKind, MIN_QUAD_CAMERA_DISTANCE,
    MandelboxConfig, MandelbulbConfig, Precision, Qf32, RenderConfig, Renderer, RendererOptions,
    VideoConfig, adapter_is_software, load_scene,
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
    pollster::block_on(Renderer::new_with_options(
        config,
        RendererOptions {
            allow_software_adapter: request.allow_software,
            adapter_name: request.adapter.clone(),
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
