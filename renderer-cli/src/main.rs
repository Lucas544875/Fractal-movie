mod output;

use std::{path::Path, path::PathBuf, process::ExitCode, time::Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use fractal_renderer_core::{RenderConfig, Renderer, RendererOptions, adapter_is_software};

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
    /// Render the built-in Phase 1 Mandelbulb scene to one PNG file.
    Render {
        /// Output PNG path.
        #[arg(long, default_value = "output/phase1/mandelbulb.png")]
        output: PathBuf,

        /// Image width in pixels.
        #[arg(long, default_value_t = 640)]
        width: u32,

        /// Image height in pixels.
        #[arg(long, default_value_t = 360)]
        height: u32,

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
            output,
            width,
            height,
            allow_software,
            adapter,
        } => render(output, width, height, allow_software, adapter),
        Command::GpuInfo => gpu_info(),
    }
}

fn render(
    output_path: PathBuf,
    width: u32,
    height: u32,
    allow_software: bool,
    adapter_name: Option<String>,
) -> Result<()> {
    let mut config = RenderConfig::default();
    config.render.width = width;
    config.render.height = height;

    let renderer = pollster::block_on(Renderer::new_with_options(
        config,
        RendererOptions {
            allow_software_adapter: allow_software,
            adapter_name,
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
    println!("Resolution: {width}x{height}");
    println!("Frames: 1");
    println!("Rendering frame 1/1");

    let total_start = Instant::now();
    let frame_start = Instant::now();
    let image = renderer
        .render_frame(0, 0.0)
        .context("failed to render Mandelbulb frame 0")?;
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
