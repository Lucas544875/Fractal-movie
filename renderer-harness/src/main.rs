use std::{
    path::PathBuf,
    process::ExitCode,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use fractal_renderer_core::load_scene;
use fractal_renderer_harness::{
    animatic,
    axes::FormulaFamily,
    brief,
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
    sample, scout, shotlist,
    stage::{InfeasibleOutcome, ReviewOutcome, Stage, Verdict},
    storyboard,
    tuning::{DEFAULT_TUNING_FILE, Tuning},
    vocabulary,
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

    /// Measure the frames of a reference film the way the harness measures
    /// its own, and report the spread of every legibility measure. For
    /// setting thresholds, not for productions.
    Calibrate {
        /// Directory of consecutive frames as PNG files, in name order.
        frames: PathBuf,

        /// Measure legibility on every n-th frame; flicker uses them all.
        #[arg(long, default_value_t = 15)]
        stride: usize,

        /// Frames to leave out at each end, such as fades.
        #[arg(long, default_value_t = 0)]
        skip: usize,
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

    /// Draw the first and last frame of every beat of the checked brief.
    Storyboard {
        id: String,

        #[command(flatten)]
        gpu: GpuArgs,
    },

    /// Record vocabulary a beat needed and the harness lacks. The
    /// production carries on with what exists.
    Request {
        id: String,

        #[arg(long, value_enum)]
        kind: RequestKind,

        /// The moment of the film that could not be expressed.
        #[arg(long)]
        beat: String,

        /// What is missing.
        #[arg(long)]
        need: String,
    },

    /// Work through vocabulary requests: list them, record decisions, and
    /// sample what a formula family yields. For vocabulary development, not
    /// for productions.
    Vocabulary {
        #[command(subcommand)]
        command: VocabularyCommand,
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

    /// Print an annotated template of brief.yaml, shotlist.yaml, or
    /// look.yaml with this production's limits.
    Schema {
        /// brief, shotlist, or look.
        file: String,
        id: String,
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

#[derive(Debug, Subcommand)]
enum VocabularyCommand {
    /// List requests with their standing.
    List {
        /// Only requests nobody has decided about yet.
        #[arg(long)]
        open: bool,
    },

    /// Record the decision about an open request.
    Resolve {
        /// Number of the request, as `vocabulary list` shows it.
        request: u32,

        #[arg(long, value_enum)]
        status: ResolutionStatus,

        /// What was added and how to use it, or why the request is declined.
        #[arg(long)]
        note: String,
    },

    /// Generate formulas of one family, render the views scouting finds,
    /// and report how many are usable and what they cost.
    Sample {
        /// Formula family, as `describe` lists it.
        #[arg(long)]
        family: String,

        /// Formulas to generate.
        #[arg(long, default_value_t = 30)]
        formulas: usize,

        #[arg(long, default_value_t = 1)]
        seed: u64,

        /// Directory the contact sheets are written to.
        #[arg(long)]
        out: PathBuf,

        #[command(flatten)]
        gpu: GpuArgs,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ResolutionStatus {
    Implemented,
    Declined,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RequestKind {
    Motion,
    Shape,
    Material,
    Light,
    Other,
}

impl From<RequestKind> for vocabulary::Kind {
    fn from(kind: RequestKind) -> Self {
        match kind {
            RequestKind::Motion => Self::Motion,
            RequestKind::Shape => Self::Shape,
            RequestKind::Material => Self::Material,
            RequestKind::Light => Self::Light,
            RequestKind::Other => Self::Other,
        }
    }
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
        Command::Schema { file, id } => {
            let production = Production::open(&cli.root, &id)?;
            let template = fractal_renderer_harness::schema::template(
                &file,
                &production.load_tuning()?,
                &production.load_budget()?,
            )?;
            Ok(json!({ "ok": true, "file": format!("{file}.yaml"), "template": template }))
        }
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
        Command::Calibrate {
            frames,
            stride,
            skip,
        } => Ok(serde_json::to_value(
            fractal_renderer_harness::calibrate::calibrate(&frames, stride, skip)?,
        )?),
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
            // One short line per candidate, grouped by the sheet it is on;
            // measurements stay in scout/index.json.
            let mut by_sheet: std::collections::BTreeMap<&str, Vec<String>> =
                std::collections::BTreeMap::new();
            for candidate in &report.candidates {
                by_sheet.entry(&candidate.sheet).or_default().push(format!(
                    "{} {} {} {} {}",
                    candidate.label,
                    candidate.location,
                    candidate.viewpoint,
                    candidate.family.as_str(),
                    candidate.scale_regime.as_str()
                ));
            }
            let candidates =
                serde_json::to_value(&by_sheet).context("could not serialize report")?;
            let mut value = serde_json::to_value(&report).context("could not serialize report")?;
            value["candidates"] = candidates;
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
            with_report(&production, "selected", &selected, &[])
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
            if let Some(infeasible) = &report.infeasible
                && infeasible.outcome == InfeasibleOutcome::Killed
            {
                production.archive_concept(&format!(
                    "mandate cannot be realised: {}",
                    infeasible.reason
                ))?;
            }
            with_report(&production, "brief", &report, &[])
        }
        Command::Storyboard { id, gpu } => {
            let production = Production::open(&cli.root, &id)?;
            let mut state = production.load_state()?;
            let mut budget = production.load_budget()?;
            let report = storyboard::run(&production, &mut state, &mut budget, &gpu.options())?;
            production.save_state(&state)?;
            with_report(&production, "storyboard", &report, &[])
        }
        Command::Request {
            id,
            kind,
            beat,
            need,
        } => {
            let production = Production::open(&cli.root, &id)?;
            let number = vocabulary::record(
                &cli.ledger,
                &vocabulary::Request {
                    id: 0,
                    production: id,
                    stage: production.load_state()?.stage,
                    kind: kind.into(),
                    beat,
                    need,
                },
            )?;
            Ok(json!({
                "ok": true,
                "request": number,
                "note": "the request is kept for later; continue with the vocabulary that exists",
            }))
        }
        Command::Vocabulary {
            command: VocabularyCommand::List { open },
        } => {
            let entries: Vec<_> = vocabulary::list(&cli.ledger)?
                .into_iter()
                .filter(|entry| !open || entry.status == vocabulary::Status::Open)
                .collect();
            Ok(json!({ "ok": true, "requests": entries }))
        }
        Command::Vocabulary {
            command:
                VocabularyCommand::Resolve {
                    request,
                    status,
                    note,
                },
        } => {
            vocabulary::resolve(
                &cli.ledger,
                &vocabulary::Resolution {
                    request,
                    status: match status {
                        ResolutionStatus::Implemented => vocabulary::Status::Implemented,
                        ResolutionStatus::Declined => vocabulary::Status::Declined,
                    },
                    note,
                },
            )?;
            Ok(json!({ "ok": true, "request": request }))
        }
        Command::Vocabulary {
            command:
                VocabularyCommand::Sample {
                    family,
                    formulas,
                    seed,
                    out,
                    gpu,
                },
        } => {
            let Some(family) = FormulaFamily::parse(&family) else {
                anyhow::bail!("unknown formula family {family:?}; `describe` lists the families");
            };
            let tuning = general_tuning(tuning_file.as_ref())?;
            let mut options = gpu.options();
            // The same ceiling productions scout under.
            options.duty_cycle_percent = options
                .duty_cycle_percent
                .or(Some(Budget::default().max_gpu_duty_cycle));
            let report = sample::family(family, formulas, seed, &out, &options, &tuning)?;
            let mut value = serde_json::to_value(report).context("could not serialize report")?;
            value["ok"] = json!(true);
            Ok(value)
        }
        Command::Shotlist {
            command: ShotlistCommand::Check { id },
        } => {
            let production = Production::open(&cli.root, &id)?;
            let mut state = production.load_state()?;
            let mut budget = production.load_budget()?;
            let report = shotlist::check(&production, &mut state, &mut budget)?;
            production.save_state(&state)?;
            with_report(&production, "shotlist", &report, &[])
        }
        Command::Animatic { id, gpu } => {
            let production = Production::open(&cli.root, &id)?;
            let mut state = production.load_state()?;
            let mut budget = production.load_budget()?;
            let report = animatic::run(&production, &mut state, &mut budget, &gpu.options())?;
            production.save_state(&state)?;
            with_report(&production, "animatic", &report, &[])
        }
        Command::Look {
            command: LookCommand::Preview { id, gpu },
        } => {
            let production = Production::open(&cli.root, &id)?;
            let mut state = production.load_state()?;
            let mut budget = production.load_budget()?;
            let report = look::preview(&production, &mut state, &mut budget, &gpu.options())?;
            production.save_state(&state)?;
            with_report(&production, "look", &report, &["frames"])
        }
        Command::Proof { id, gpu } => {
            let production = Production::open(&cli.root, &id)?;
            let mut state = production.load_state()?;
            let mut budget = production.load_budget()?;
            let report = proof::run(&production, &mut state, &mut budget, &gpu.options())?;
            production.save_state(&state)?;
            let mut value = with_report(&production, "proof", &report, &["decisions", "cuts"])?;
            // Per cut, only what a director can act on.
            value["proof"]["cuts"] = report
                .cuts
                .iter()
                .map(|cut| {
                    json!({
                        "id": cut.id,
                        "estimated_gpu_seconds": cut.estimated_gpu_seconds.round(),
                        "flicker": cut.flicker,
                        "delivery_ssim": cut.delivery_ssim,
                    })
                })
                .collect();
            value["proof"]["quality"] = report
                .decisions
                .iter()
                .map(|(location, decision)| {
                    (
                        location.clone(),
                        json!({
                            "settings": decision.settings,
                            "fits_budget": decision.fits_budget,
                            "degradations": decision.degradations,
                        }),
                    )
                })
                .collect::<serde_json::Map<_, _>>()
                .into();
            Ok(value)
        }
        Command::Produce {
            id,
            max_frames,
            gpu,
        } => {
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
            with_report(&production, "produce", &report, &[])
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
            with_report(&production, "encode", &report, &[])
        }
        Command::Reopen { id, stage, reason } => {
            let production = Production::open(&cli.root, &id)?;
            let mut state = production.load_state()?;
            let target = match stage {
                ReopenStage::Shotlist => Stage::Shotlist,
                ReopenStage::Look => Stage::Look,
            };
            let remaining = state.reopen(target, reason)?;
            production.save_state(&state)?;
            let mut value = step_report(&production)?;
            value["revisions_remaining"] = json!(remaining);
            Ok(value)
        }
        Command::Review {
            command: ReviewCommand::Submit { id, verdict, notes },
        } => {
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
                production.archive_concept(&format!("critic: {notes}"))?;
            }
            let advanced_to = state.advance_if_ready()?;

            let record = json!({
                "stage": stage,
                "verdict": verdict,
                "notes": notes,
                "outcome": outcome,
            });
            let directory = production.path("reviews");
            // Verdicts are numbered after the highest number present, so a
            // record is never overwritten whatever else the directory holds.
            let number = std::fs::read_dir(&directory)
                .with_context(|| format!("could not read {}", directory.display()))?
                .filter_map(|entry| entry.ok())
                .filter_map(|entry| {
                    let name = entry.file_name();
                    let name = name.to_str()?;
                    name.ends_with(".json")
                        .then(|| name.split('-').next()?.parse::<u32>().ok())
                        .flatten()
                })
                .max()
                .unwrap_or(0)
                + 1;
            std::fs::write(
                directory.join(format!("{number:03}-{}.json", stage.as_str())),
                format!("{record:#}\n"),
            )
            .context("could not record the review")?;
            production.save_state(&state)?;

            let mut value = step_report(&production)?;
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
            state.kill(reason.clone(), abandoned)?;
            production.save_state(&state)?;
            let kept = production.archive_concept(&reason)?;
            let mut value = next_report(&production, &ledger)?;
            value["archived"] = json!(kept);
            Ok(value)
        }
    }
}

/// The short report every step command returns: where the production now
/// stands and which gates still stand in the way. The full picture, with
/// diversity constraints and budget, is `next`.
fn step_report(production: &Production) -> Result<Value> {
    let state = production.load_state()?;
    let gates: Vec<Value> = state
        .stage
        .gates()
        .iter()
        .map(|gate| match state.gates.get(*gate) {
            Some(record) => json!({
                "gate": gate,
                "status": if record.passed { "passed" } else { "failed" },
                "detail": record.detail,
            }),
            None => json!({ "gate": gate, "status": "pending" }),
        })
        .collect();
    Ok(json!({
        "ok": true,
        "stage": state.stage,
        "complete": state.stage == Stage::Done,
        "gates": gates,
        "limits": next::limits(&state),
        "compromised": state.compromised,
    }))
}

/// Merges `report` into a step report under `key`, dropping fields that are
/// written to the stage's report file and would only crowd the output.
fn with_report<T: serde::Serialize>(
    production: &Production,
    key: &str,
    report: &T,
    drop: &[&str],
) -> Result<Value> {
    let mut report = serde_json::to_value(report).context("could not serialize report")?;
    if let Some(fields) = report.as_object_mut() {
        for field in drop {
            fields.remove(*field);
        }
    }
    let mut value = step_report(production)?;
    value[key] = report;
    Ok(value)
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
