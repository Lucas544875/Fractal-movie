use std::{
    path::PathBuf,
    process::ExitCode,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use fractal_renderer_core::load_scene;
use fractal_renderer_harness::{
    animatic, brief,
    budget::Budget,
    describe::describe,
    encode,
    gpu::{GpuOptions, SceneProbe, measure_legibility},
    import::{self, ImportOptions},
    ledger::Ledger,
    look, next,
    produce::{self, ProduceOptions},
    production::{DEFAULT_LEDGER, DEFAULT_ROOT, Production},
    proof,
    quality::{FrameLimits, decide},
    scout, shotlist,
    stage::{ReviewOutcome, Stage, Verdict},
    tuning::{DEFAULT_TUNING_FILE, Tuning},
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

    /// Tuning file with thresholds and limits. Defaults to
    /// harness-tuning.yaml in the working directory when it exists. A
    /// production keeps the tuning it was created with.
    #[arg(long, global = true, value_name = "FILE")]
    tuning: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

/// Environment variable that marks a process as the critic.
const CRITIC_ROLE_VARIABLE: &str = "FRACTAL_HARNESS_ROLE";

/// Tuning for commands that are not bound to a production.
fn general_tuning(explicit: Option<&PathBuf>) -> Result<Tuning> {
    let default = std::path::Path::new(DEFAULT_TUNING_FILE);
    match explicit {
        Some(path) => Tuning::load(path),
        None if default.is_file() => Tuning::load(default),
        None => Ok(Tuning::default()),
    }
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

        /// Shortest and longest permitted film, in seconds.
        #[arg(long, value_names = ["MIN", "MAX"], num_args = 2)]
        film_seconds: Option<Vec<f64>>,

        /// Fewest and most permitted cuts.
        #[arg(long, value_names = ["MIN", "MAX"], num_args = 2)]
        film_cuts: Option<Vec<usize>>,
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

    /// Check brief.yaml against the schema, the mandate, and recent films.
    Brief {
        #[command(subcommand)]
        command: BriefCommand,
    },

    /// Check shotlist.yaml: compile every cut, walk its camera route, and
    /// verify the film against its brief.
    Shotlist {
        #[command(subcommand)]
        command: ShotlistCommand,
    },

    /// Render the whole film at thumbnail size, cut together with its
    /// transitions, and judge its legibility.
    Animatic {
        id: String,

        #[command(flatten)]
        gpu: GpuArgs,
    },

    /// Apply look.yaml to every cut, render preview frames, and judge the
    /// look against the brief.
    Look {
        #[command(subcommand)]
        command: LookCommand,
    },

    /// Decide the final quality, estimate the render time against the
    /// budget, and render a proof clip judged after delivery compression.
    Proof {
        id: String,

        #[command(flatten)]
        gpu: GpuArgs,
    },

    /// Go back to the shot list or the look to change it. Costs one
    /// revision of the stage being left.
    Reopen {
        id: String,

        #[arg(long, value_enum)]
        stage: ReopenStage,

        /// Why the earlier stage has to change.
        #[arg(long)]
        reason: String,
    },

    /// Render the final frames. Safe to interrupt; run it again to
    /// continue.
    Produce {
        id: String,

        /// Stop after this many frames in this run.
        #[arg(long)]
        max_frames: Option<u32>,

        #[command(flatten)]
        gpu: GpuArgs,
    },

    /// Report the progress of production rendering.
    Status { id: String },

    /// Cut the rendered frames into the final file, check it, and enter
    /// the film into the ledger.
    Encode { id: String },

    /// Record the critic's verdict on the current stage.
    Review {
        #[command(subcommand)]
        command: ReviewCommand,
    },

    /// Inspect or extend the ledger of finished films.
    Ledger {
        #[command(subcommand)]
        command: LedgerCommand,
    },

    /// Show or export the tuning: every threshold, limit, and window.
    Tuning {
        #[command(subcommand)]
        command: TuningCommand,
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

        /// Number of shapes to render in this round. Defaults to the
        /// production's tuning.
        #[arg(long)]
        shapes: Option<usize>,

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

/// GPU selection and pacing shared by rendering commands.
#[derive(Debug, Args)]
struct GpuArgs {
    /// Limit the average GPU duty cycle, as a percentage. Defaults to the
    /// production's ceiling.
    #[arg(long, value_name = "PERCENT")]
    gpu_duty_cycle: Option<f64>,

    /// Permit CPU/software rendering when no hardware GPU is available.
    #[arg(long)]
    allow_software: bool,

    /// Select an adapter by a case-insensitive name substring.
    #[arg(long, value_name = "NAME")]
    adapter: Option<String>,
}

impl GpuArgs {
    fn options(self) -> GpuOptions {
        GpuOptions {
            duty_cycle_percent: self.gpu_duty_cycle,
            allow_software: self.allow_software,
            adapter: self.adapter,
        }
    }
}

#[derive(Debug, Subcommand)]
enum TuningCommand {
    /// Print the tuning new productions would be created with.
    Show,

    /// Write that tuning as a YAML file to edit. Existing files are kept.
    Write { path: PathBuf },
}

#[derive(Debug, Subcommand)]
enum ShotlistCommand {
    /// Judge the schema, route-safety, and variety gates from shotlist.yaml.
    Check { id: String },
}

#[derive(Debug, Subcommand)]
enum LookCommand {
    /// Judge the schema, hue-count, and clipping gates from look.yaml.
    Preview {
        id: String,

        #[command(flatten)]
        gpu: GpuArgs,
    },
}

#[derive(Debug, Subcommand)]
enum ReviewCommand {
    /// Apply a verdict. Every measured gate of the stage must have passed.
    Submit {
        id: String,

        #[arg(long, value_enum)]
        verdict: VerdictArg,

        /// The critic's findings, kept with the production.
        #[arg(long)]
        notes: String,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ReopenStage {
    Shotlist,
    Look,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum VerdictArg {
    Pass,
    Revise,
    Kill,
}

impl From<VerdictArg> for Verdict {
    fn from(verdict: VerdictArg) -> Self {
        match verdict {
            VerdictArg::Pass => Self::Pass,
            VerdictArg::Revise => Self::Revise,
            VerdictArg::Kill => Self::Kill,
        }
    }
}

#[derive(Debug, Subcommand)]
enum BriefCommand {
    /// Judge every gate of the brief stage from brief.yaml.
    Check { id: String },
}

#[derive(Debug, Subcommand)]
enum LedgerCommand {
    /// List finished films with their axes.
    List,

    /// Compose the keyframes of the most recent films into one image for
    /// review.
    Sheet {
        /// PNG file to write.
        path: PathBuf,
    },

    /// Register a film made outside the harness from its scene file. Its
    /// axes are classified and its keyframes rendered for the novelty gate.
    Import {
        /// Versioned YAML scene file of the finished film.
        scene: PathBuf,

        /// Film id: a-z, 0-9, and inner hyphens.
        #[arg(long)]
        id: String,

        #[arg(long)]
        title: String,

        /// Completion date as YYYY-MM-DD.
        #[arg(long)]
        date: String,

        /// GPU minutes the film took, when known.
        #[arg(long)]
        gpu_minutes: Option<f64>,

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
    let tuning_file = cli.tuning.clone();
    match cli.command {
        Command::Describe => Ok(
            json!({ "ok": true, "vocabulary": describe(&general_tuning(tuning_file.as_ref())?) }),
        ),
        Command::Tuning {
            command: TuningCommand::Show,
        } => Ok(json!({ "ok": true, "tuning": general_tuning(tuning_file.as_ref())? })),
        Command::Tuning {
            command: TuningCommand::Write { path },
        } => {
            let tuning = general_tuning(tuning_file.as_ref())?;
            if path.exists() {
                anyhow::bail!("{} already exists; it was not overwritten", path.display());
            }
            let yaml = serde_yaml_ng::to_string(&tuning).context("could not serialize tuning")?;
            std::fs::write(&path, yaml)
                .with_context(|| format!("could not write {}", path.display()))?;
            Ok(json!({ "ok": true, "written": path }))
        }
        Command::Init {
            id,
            seed,
            duration,
            gpu_minutes_per_second,
            max_gpu_duty_cycle,
            film_seconds,
            film_cuts,
        } => {
            let ledger = Ledger::load(&cli.ledger)?;
            let mut budget = Budget::default();
            if let Some(seconds) = film_seconds {
                budget.film.min_duration_seconds = seconds[0];
                budget.film.max_duration_seconds = seconds[1];
            }
            if let Some(cuts) = film_cuts {
                budget.film.min_cuts = cuts[0];
                budget.film.max_cuts = cuts[1];
            }
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
            let production = Production::create_with_tuning(
                &cli.root,
                &id,
                &ledger,
                seed,
                budget,
                &general_tuning(tuning_file.as_ref())?,
            )?;
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
                &general_tuning(tuning_file.as_ref())?,
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
            let ledger = Ledger::load(&cli.ledger)?;
            let mut state = production.load_state()?;
            let mut mandate = production.load_mandate()?;
            let report = scout::run(
                &production,
                &mut state,
                &mut mandate,
                &ledger,
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
            // An unrealisable family assignment changes the stage record.
            production.save_state(&state)?;
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
                &ledger,
                &locations,
            )?;
            production.save_state(&state)?;
            let mut value = next_report(&production, &ledger)?;
            value["selected"] =
                serde_json::to_value(selected).context("could not serialize selection")?;
            Ok(value)
        }
        Command::Brief {
            command: BriefCommand::Check { id },
        } => {
            let ledger = Ledger::load(&cli.ledger)?;
            let production = Production::open(&cli.root, &id)?;
            let mut state = production.load_state()?;
            let mut mandate = production.load_mandate()?;
            let report = brief::check(&production, &mut state, &mut mandate, &ledger)?;
            production.save_state(&state)?;
            let mut value = next_report(&production, &ledger)?;
            value["brief"] = serde_json::to_value(report).context("could not serialize report")?;
            Ok(value)
        }
        Command::Shotlist {
            command: ShotlistCommand::Check { id },
        } => {
            let ledger = Ledger::load(&cli.ledger)?;
            let production = Production::open(&cli.root, &id)?;
            let mut state = production.load_state()?;
            let mut budget = production.load_budget()?;
            let report = shotlist::check(&production, &mut state, &mut budget)?;
            production.save_state(&state)?;
            let mut value = next_report(&production, &ledger)?;
            value["shotlist"] =
                serde_json::to_value(report).context("could not serialize report")?;
            Ok(value)
        }
        Command::Animatic { id, gpu } => {
            let ledger = Ledger::load(&cli.ledger)?;
            let production = Production::open(&cli.root, &id)?;
            let mut state = production.load_state()?;
            let mut budget = production.load_budget()?;
            let report = animatic::run(&production, &mut state, &mut budget, &gpu.options())?;
            production.save_state(&state)?;
            let mut value = next_report(&production, &ledger)?;
            value["animatic"] =
                serde_json::to_value(report).context("could not serialize report")?;
            Ok(value)
        }
        Command::Look {
            command: LookCommand::Preview { id, gpu },
        } => {
            let ledger = Ledger::load(&cli.ledger)?;
            let production = Production::open(&cli.root, &id)?;
            let mut state = production.load_state()?;
            let mut budget = production.load_budget()?;
            let report = look::preview(&production, &mut state, &mut budget, &gpu.options())?;
            production.save_state(&state)?;
            let mut value = next_report(&production, &ledger)?;
            value["look"] = serde_json::to_value(report).context("could not serialize report")?;
            Ok(value)
        }
        Command::Proof { id, gpu } => {
            let ledger = Ledger::load(&cli.ledger)?;
            let production = Production::open(&cli.root, &id)?;
            let mut state = production.load_state()?;
            let mut budget = production.load_budget()?;
            let report = proof::run(&production, &mut state, &mut budget, &gpu.options())?;
            production.save_state(&state)?;
            let mut value = next_report(&production, &ledger)?;
            value["proof"] = serde_json::to_value(report).context("could not serialize report")?;
            Ok(value)
        }
        Command::Produce {
            id,
            max_frames,
            gpu,
        } => {
            let ledger = Ledger::load(&cli.ledger)?;
            let production = Production::open(&cli.root, &id)?;
            let mut state = production.load_state()?;
            let mut budget = production.load_budget()?;
            let report = produce::run(
                &production,
                &mut state,
                &mut budget,
                &ProduceOptions {
                    max_frames,
                    gpu: gpu.options(),
                },
            )?;
            production.save_state(&state)?;
            let mut value = next_report(&production, &ledger)?;
            value["produce"] =
                serde_json::to_value(report).context("could not serialize report")?;
            Ok(value)
        }
        Command::Status { id } => {
            let production = Production::open(&cli.root, &id)?;
            let status = produce::status(&production)?;
            Ok(json!({ "ok": true, "stage": production.load_state()?.stage, "status": status }))
        }
        Command::Encode { id } => {
            let mut ledger = Ledger::load(&cli.ledger)?;
            let production = Production::open(&cli.root, &id)?;
            let mut state = production.load_state()?;
            let report = encode::run(
                &production,
                &mut state,
                &production.load_budget()?,
                &mut ledger,
                &cli.ledger,
            )?;
            production.save_state(&state)?;
            let mut value = next_report(&production, &ledger)?;
            value["encode"] = serde_json::to_value(report).context("could not serialize report")?;
            Ok(value)
        }
        Command::Reopen { id, stage, reason } => {
            let ledger = Ledger::load(&cli.ledger)?;
            let production = Production::open(&cli.root, &id)?;
            let mut state = production.load_state()?;
            let target = match stage {
                ReopenStage::Shotlist => Stage::Shotlist,
                ReopenStage::Look => Stage::Look,
            };
            let remaining = state.reopen(target, reason)?;
            production.save_state(&state)?;
            let mut value = next_report(&production, &ledger)?;
            value["revisions_remaining"] = json!(remaining);
            Ok(value)
        }
        Command::Review {
            command: ReviewCommand::Submit { id, verdict, notes },
        } => {
            let ledger = Ledger::load(&cli.ledger)?;
            let production = Production::open(&cli.root, &id)?;
            let mut state = production.load_state()?;
            // The verdict must come from the critic's own process. A director
            // is only permitted to run this executable without an environment
            // prefix, so it cannot grade its own work.
            if std::env::var(CRITIC_ROLE_VARIABLE).as_deref() != Ok("critic") {
                anyhow::bail!(
                    "verdicts are submitted by the critic; run tools/film-critic.sh {id} instead"
                );
            }
            let stage = state.stage;
            let verdict = Verdict::from(verdict);
            let outcome = state.review(verdict, notes.clone())?;
            if matches!(outcome, ReviewOutcome::Killed { .. }) {
                // The dropped concept's locations may not be chosen again.
                state.ban_locations(scout::Index::load(&production)?.selected);
            }
            let advanced_to = state.advance_if_ready()?;

            let record = json!({
                "stage": stage,
                "verdict": verdict,
                "notes": notes,
                "outcome": outcome,
            });
            let directory = production.path("reviews");
            let number = std::fs::read_dir(&directory)
                .with_context(|| format!("could not read {}", directory.display()))?
                .count()
                + 1;
            std::fs::write(
                directory.join(format!("{number:03}-{}.json", stage.as_str())),
                format!("{record:#}\n"),
            )
            .context("could not record the review")?;
            production.save_state(&state)?;

            let mut value = next_report(&production, &ledger)?;
            value["review"] = record;
            value["advanced_to"] = json!(advanced_to);
            Ok(value)
        }
        Command::Ledger {
            command: LedgerCommand::List,
        } => {
            let ledger = Ledger::load(&cli.ledger)?;
            let films: Vec<Value> = ledger
                .entries()
                .iter()
                .map(|entry| {
                    json!({
                        "id": entry.id,
                        "date": entry.date,
                        "title": entry.title,
                        "axes": entry.axes,
                        "duration_seconds": entry.duration_seconds,
                        "gpu_minutes": entry.gpu_minutes,
                        "keyframes": entry.keyframes.len(),
                        "compromised": entry.compromised,
                    })
                })
                .collect();
            Ok(json!({ "ok": true, "films": films }))
        }
        Command::Ledger {
            command: LedgerCommand::Sheet { path },
        } => {
            let ledger = Ledger::load(&cli.ledger)?;
            let window = general_tuning(tuning_file.as_ref())?
                .diversity
                .axis_diff_window;
            let films = ledger.write_sheet(&path, window)?;
            Ok(json!({ "ok": true, "sheet": films.as_ref().map(|_| &path), "films": films }))
        }
        Command::Ledger {
            command:
                LedgerCommand::Import {
                    scene,
                    id,
                    title,
                    date,
                    gpu_minutes,
                    gpu_duty_cycle,
                    allow_software,
                    adapter,
                },
        } => {
            let mut ledger = Ledger::load(&cli.ledger)?;
            let entry = import::import(
                &scene,
                &cli.ledger,
                &mut ledger,
                &ImportOptions {
                    id,
                    title,
                    date,
                    gpu_minutes,
                    gpu: GpuOptions {
                        duty_cycle_percent: gpu_duty_cycle,
                        allow_software,
                        adapter,
                    },
                },
                &general_tuning(tuning_file.as_ref())?.classification,
            )?;
            Ok(json!({
                "ok": true,
                "id": entry.id,
                "axes": entry.axes,
                "keyframes": entry.keyframes,
                "duration_seconds": entry.duration_seconds,
            }))
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
        &production.load_tuning()?,
    );
    let mut value = serde_json::to_value(report).context("could not serialize report")?;
    value["ok"] = json!(true);
    value["directory"] = json!(production.directory());
    Ok(value)
}

/// Frame rate assumed for a scene without an animation.
const STATIC_SCENE_FPS: u32 = 30;

fn measure(
    scene_path: &std::path::Path,
    budget: &Budget,
    options: &GpuOptions,
    tuning: &Tuning,
) -> Result<Value> {
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

    let mut probe = SceneProbe::new(&scene, options, tuning.proof.region_fraction)?;
    let frames = probe.probe_frames();
    let requested = probe.requested();
    eprintln!(
        "Measuring {} on {} with probe frames {frames:?}",
        scene.name,
        probe.adapter_name()
    );
    let (legibility, thumbnail_gpu_seconds) = measure_legibility(&scene, &frames, options)?;
    let decision = decide(&mut probe, &frames, &requested, &limits, &tuning.quality)?;

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
