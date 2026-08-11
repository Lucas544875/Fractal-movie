mod output;

use std::{path::PathBuf, process::ExitCode, time::Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use fractal_renderer_core::{RenderConfig, Renderer};

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
    },
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
        } => render(output, width, height),
    }
}

fn render(output_path: PathBuf, width: u32, height: u32) -> Result<()> {
    let mut config = RenderConfig::default();
    config.render.width = width;
    config.render.height = height;

    let renderer = pollster::block_on(Renderer::new(config))
        .context("could not initialize the offscreen renderer")?;
    let adapter = renderer.adapter_info();
    println!(
        "GPU: {} ({:?}, {:?})",
        adapter.name, adapter.backend, adapter.device_type
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
