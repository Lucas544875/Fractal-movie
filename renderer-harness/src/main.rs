use std::{
    path::PathBuf,
    process::ExitCode,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use fractal_renderer_core::load_scene;
use fractal_renderer_harness::{
    budget::Budget,
    describe::describe,
    gpu::{GpuOptions, SceneProbe, measure_legibility},
    ledger::Ledger,
    next,
    production::{DEFAULT_LEDGER, DEFAULT_ROOT, Production},
    quality::{FrameLimits, decide},
    scout,
};
use serde_json::{Value, json};

#[derive(Debug, Parser)]
#[command(
    name = "fractal-harness",
    version,
    about = "Production harness for agent-directed fractal films"
)]
struct Cli {
    /// Directory that holds one subdirectory per production.
    #[arg(long, global = true, default_value = DEFAULT_ROOT)]
    root: PathBuf,

    /// Ledger of finished films used by the diversity rules.
    #[arg(long, global = true, default_value = DEFAULT_LEDGER)]
    ledger: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the design vocabulary: axes, stages, gates, and limits.
    Describe,

    /// Create a production and draw its diversity constraints and budget.
    Init {
        /// Production id: a-z, 0-9, and inner hyphens.
        id: String,

        /// Seed for the constraint draw. Defaults to the current time.
        #[arg(long)]
        seed: Option<u64>,

        /// Film length assumed until the shot list fixes it, in seconds.
        #[arg(long, value_name = "SECONDS")]
        duration: Option<f64>,

        /// Production GPU minutes allowed per second of finished film.
        #[arg(long, value_name = "MINUTES")]
        gpu_minutes_per_second: Option<f64>,

        /// Upper bound for every GPU command's duty cycle, in percent.
        #[arg(long, value_name = "PERCENT")]
        max_gpu_duty_cycle: Option<f64>,
    },

    /// Report the current stage, its gates, and the commands that apply.
    Next { id: String },

    /// Measure an existing scene file: legibility, the quality the harness
    /// would choose for it, and the render time against a budget.
    ///
    /// This is a calibration tool for people. Productions never use scene
    /// files; their cuts are measured by `proof`.
    Measure {
        /// Versioned YAML scene file.
        scene: PathBuf,

        /// Production GPU minutes allowed per second of finished film.
        #[arg(long, value_name = "MINUTES")]
        gpu_minutes_per_second: Option<f64>,

        /// Limit the average GPU duty cycle, as a percentage.
        #[arg(long, value_name = "PERCENT")]
        gpu_duty_cycle: Option<f64>,

        /// Permit CPU/software rendering when no hardware GPU is available.
        #[arg(long)]
        allow_software: bool,

        /// Select an adapter by a case-insensitive name substring.
        #[arg(long, value_name = "NAME")]
        adapter: Option<String>,
    },

    /// Scout locations: sample shapes and viewpoints, then choose among them.
    Scout {
        #[command(subcommand)]
        command: ScoutCommand,
    },

    /// Abandon the current concept and return to scouting.
    Kill {
        id: String,

        /// Why the concept is being abandoned.
        #[arg(long)]
        reason: String,
    },
}

#[derive(Debug, Subcommand)]
enum ScoutCommand {
    /// Sample shapes and viewpoints and compose contact sheets of the ones
    /// that pass screening.
    Run {
        id: String,

        /// Number of shapes to render in this round.
        #[arg(long, default_value_t = scout::DEFAULT_SHAPES)]
        shapes: usize,

        /// Sample variations of an already scouted location, e.g. loc-0012.
        #[arg(long, value_name = "LOCATION")]
        around: Option<String>,

        /// Limit the average GPU duty cycle, as a percentage. Defaults to
        /// the production's ceiling.
        #[arg(long, value_name = "PERCENT")]
        gpu_duty_cycle: Option<f64>,

        /// Permit CPU/software rendering when no hardware GPU is available.
        #[arg(long)]
        allow_software: bool,

        /// Select an adapter by a case-insensitive name substring.
        #[arg(long, value_name = "NAME")]
        adapter: Option<String>,
    },

    /// Choose the locations the film will use.
    Select {
        id: String,

        /// Location ids such as loc-0012.
        #[arg(required = true)]
        locations: Vec<String>,
    },
}

fn main() -> ExitCode {
    // Help and usage errors keep clap's own formatting and exit codes.
    let cli = Cli::parse();
    // stdout carries exactly one JSON document per invocation so an agent can
    // parse every result, including failures, the same way.
    match run(cli) {
        Ok(value) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("{}", json!({ "ok": false, "error": format!("{error:#}") }));
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<Value> {
    match cli.command {
        Command::Describe => Ok(json!({ "ok": true, "vocabulary": describe() })),
        Command::Init {
            id,
            seed,
            duration,
            gpu_minutes_per_second,
            max_gpu_duty_cycle,
        } => {
            let ledger = Ledger::load(&cli.ledger)?;
            let mut budget = Budget::default();
            if let Some(duration) = duration {
                budget.planned_duration_seconds = duration;
            }
            if let Some(minutes) = gpu_minutes_per_second {
                budget.gpu_minutes_per_output_second = minutes;
            }
            if let Some(duty_cycle) = max_gpu_duty_cycle {
                budget.max_gpu_duty_cycle = duty_cycle;
            }
            let seed = seed.map_or_else(time_seed, Ok)?;
            let production = Production::create(&cli.root, &id, &ledger, seed, budget)?;
            next_report(&production, &ledger)
        }
        Command::Next { id } => {
            let ledger = Ledger::load(&cli.ledger)?;
            next_report(&Production::open(&cli.root, &id)?, &ledger)
        }
        Command::Measure {
            scene,
            gpu_minutes_per_second,
            gpu_duty_cycle,
            allow_software,
            adapter,
        } => {
            let mut budget = Budget::default();
            if let Some(minutes) = gpu_minutes_per_second {
                budget.gpu_minutes_per_output_second = minutes;
            }
            budget.validate()?;
            measure(
                &scene,
                &budget,
                &GpuOptions {
                    duty_cycle_percent: gpu_duty_cycle,
                    allow_software,
                    adapter,
                },
            )
        }
        Command::Scout {
            command:
                ScoutCommand::Run {
                    id,
                    shapes,
                    around,
                    gpu_duty_cycle,
                    allow_software,
                    adapter,
                },
        } => {
            let production = Production::open(&cli.root, &id)?;
            let mut budget = production.load_budget()?;
            let report = scout::run(
                &production,
                &production.load_state()?,
                &production.load_mandate()?,
                &mut budget,
                &scout::RunOptions {
                    shapes,
                    around,
                    gpu: GpuOptions {
                        duty_cycle_percent: gpu_duty_cycle,
                        allow_software,
                        adapter,
                    },
                },
            )?;
            let mut value = serde_json::to_value(report).context("could not serialize report")?;
            value["ok"] = json!(true);
            value["directory"] = json!(production.directory());
            Ok(value)
        }
        Command::Scout {
            command: ScoutCommand::Select { id, locations },
        } => {
            let ledger = Ledger::load(&cli.ledger)?;
            let production = Production::open(&cli.root, &id)?;
            let mut state = production.load_state()?;
            let selected = scout::select(
                &production,
                &mut state,
                &production.load_budget()?,
                &locations,
            )?;
            production.save_state(&state)?;
            let mut value = next_report(&production, &ledger)?;
            value["selected"] =
                serde_json::to_value(selected).context("could not serialize selection")?;
            Ok(value)
        }
        Command::Kill { id, reason } => {
            let ledger = Ledger::load(&cli.ledger)?;
            let production = Production::open(&cli.root, &id)?;
            let mut state = production.load_state()?;
            // The abandoned concept's locations may not be chosen again.
            let abandoned = scout::Index::load(&production)?.selected;
            state.kill(reason, abandoned)?;
            production.save_state(&state)?;
            next_report(&production, &ledger)
        }
    }
}

fn next_report(production: &Production, ledger: &Ledger) -> Result<Value> {
    let report = next::report(
        &production.load_state()?,
        &production.load_mandate()?,
        &production.load_budget()?,
        ledger,
    );
    let mut value = serde_json::to_value(report).context("could not serialize report")?;
    value["ok"] = json!(true);
    value["directory"] = json!(production.directory());
    Ok(value)
}

/// Frame rate assumed for a scene without an animation.
const STATIC_SCENE_FPS: u32 = 30;

fn measure(scene_path: &std::path::Path, budget: &Budget, options: &GpuOptions) -> Result<Value> {
    let scene = load_scene(scene_path)?;
    let (fps, frame_count) = scene
        .animation
        .as_ref()
        .map_or((STATIC_SCENE_FPS, 1), |animation| {
            (animation.fps, animation.frame_count)
        });
    let duration_seconds = f64::from(frame_count) / f64::from(fps);
    let limits = FrameLimits {
        mean_frame_seconds: budget.mean_frame_seconds(f64::from(fps)),
        max_frame_seconds: budget.max_frame_seconds,
    };

    let mut probe = SceneProbe::new(&scene, options)?;
    let frames = probe.probe_frames();
    let requested = probe.requested();
    eprintln!(
        "Measuring {} on {} with probe frames {frames:?}",
        scene.name,
        probe.adapter_name()
    );
    let (legibility, thumbnail_gpu_seconds) = measure_legibility(&scene, &frames, options)?;
    let decision = decide(&mut probe, &frames, &requested, &limits)?;

    let estimated = decision.estimate.mean_frame_seconds * f64::from(frame_count);
    let allowed = budget.gpu_minutes_per_output_second * 60.0 * duration_seconds;
    Ok(json!({
        "ok": true,
        "scene": scene.name,
        "adapter": probe.adapter_name(),
        "resolution": [scene.config.render.width, scene.config.render.height],
        "fps": fps,
        "frame_count": frame_count,
        "duration_seconds": duration_seconds,
        "requested": requested,
        "legibility": legibility,
        "decision": decision,
        "budget": {
            "gpu_minutes_per_output_second": budget.gpu_minutes_per_output_second,
            "allowed_production_gpu_seconds": allowed,
            "estimated_production_gpu_seconds": estimated,
            "estimated_gpu_minutes_per_output_second": estimated / 60.0 / duration_seconds,
        },
        "measurement_gpu_seconds": decision.gpu_seconds_spent + thumbnail_gpu_seconds,
    }))
}

fn time_seed() -> Result<u64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?;
    // Nanosecond resolution separates productions created in the same second.
    Ok(elapsed.as_nanos() as u64)
}
