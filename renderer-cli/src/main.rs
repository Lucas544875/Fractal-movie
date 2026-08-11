mod output;

use std::{path::Path, path::PathBuf, process::ExitCode, time::Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use fractal_renderer_core::{
    FractalConfig, FractalKind, MIN_QUAD_CAMERA_DISTANCE, MandelboxConfig, MandelbulbConfig,
    Precision, Qf32, RenderConfig, Renderer, RendererOptions, adapter_is_software, load_scene,
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
enum Command {
    /// Render a YAML scene or built-in preset to one PNG file.
    Render {
        /// Versioned YAML scene file. Omit to use a built-in preset.
        #[arg(value_name = "SCENE")]
        scene: Option<PathBuf>,

        /// Output PNG path.
        #[arg(long)]
        output: Option<PathBuf>,

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

struct RenderRequest {
    scene: Option<PathBuf>,
    output: Option<PathBuf>,
    fractal: Option<FractalName>,
    precision: Option<PrecisionName>,
    camera_distance: Option<Qf32>,
    seed: Option<u32>,
    width: Option<u32>,
    height: Option<u32>,
    allow_software: bool,
    adapter: Option<String>,
}

struct PreparedScene {
    name: String,
    config: RenderConfig,
    default_output: PathBuf,
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
    let scene_name = prepared.name;
    let config = prepared.config;
    let output_path = request.output.unwrap_or(prepared.default_output);
    let fractal_kind = config.fractal.kind();
    let precision = config.precision;
    let width = config.render.width;
    let height = config.render.height;

    let renderer = pollster::block_on(Renderer::new_with_options(
        config,
        RendererOptions {
            allow_software_adapter: request.allow_software,
            adapter_name: request.adapter,
        },
    ))
    .context("could not initialize the offscreen renderer")?;
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
    println!("Scene: {scene_name}");
    println!("Fractal: {}", fractal_label(fractal_kind));
    println!("Precision: {}", precision_label(precision));
    println!("Resolution: {width}x{height}");
    println!("Frames: 1");
    println!("Rendering frame 1/1");

    let total_start = Instant::now();
    let frame_start = Instant::now();
    let image = renderer
        .render_frame(0, 0.0)
        .with_context(|| format!("failed to render {scene_name} frame 0"))?;
    let frame_time = frame_start.elapsed();
    println!("Frame render time: {:.3}s", frame_time.as_secs_f64());

    output::save_png(&output_path, &image)
        .with_context(|| format!("failed to write {}", output_path.display()))?;
    let total_time = total_start.elapsed();
    println!("Saved: {}", output_path.display());
    println!("Total render time: {:.3}s", total_time.as_secs_f64());
    println!("Average frame time: {:.3}s", frame_time.as_secs_f64());
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
    let (name, mut config) = if let Some(path) = scene_path {
        if camera_distance.is_some() {
            bail!(
                "--camera-distance is available only with the built-in quad-float Mandelbox preset"
            );
        }
        let scene = load_scene(path)?;
        (scene.name, scene.config)
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
                ("mandelbox-quad".to_owned(), config)
            }
            (FractalName::Mandelbulb, PrecisionName::QuadFloat) => {
                bail!("quad-float precision is currently supported only for Mandelbox scenes");
            }
            (_, PrecisionName::F32) => {
                if camera_distance.is_some() {
                    bail!("--camera-distance requires --precision quad-float");
                }
                (fractal.label().to_owned(), fractal.built_in_config(seed))
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

    let default_output = if has_scene_file {
        PathBuf::from("output")
            .join(&name)
            .join(format!("{name}.png"))
    } else {
        PathBuf::from("output/phase1").join(format!("{name}.png"))
    };
    Ok(PreparedScene {
        name,
        config,
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
    use super::*;

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
}
