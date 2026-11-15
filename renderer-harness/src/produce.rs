//! Production rendering. By this stage every decision has been made: the
//! command renders the final scenes frame by frame, can be interrupted at
//! any point, and continues where it stopped when it is run again.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};

use anyhow::{Context, Result, bail};
use fractal_renderer_core::{Renderer, load_scene};
use serde::{Deserialize, Serialize};

use crate::{
    budget::Budget,
    gpu::{GpuOptions, frame_config},
    production::Production,
    proof::{FinalCut, FinalManifest},
    sheet,
    shotlist::{Manifest, content_hash},
    stage::{Stage, State},
};

const LOCK_FILE: &str = "jobs/render.lock";
/// Frames between saves of a job's progress.
const CHECKPOINT_FRAMES: u32 = 10;
/// Actual render time above this multiple of the estimate is reported.
const SLOW_FACTOR: f64 = 1.5;

/// Progress of one cut, kept across interruptions.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: u32,
    /// Hash of the scene the existing frames were rendered from.
    pub scene_hash: String,
    pub frame_count: u32,
    /// Frames `0..completed` exist on disk.
    pub completed: u32,
    pub gpu_seconds: f64,
    pub estimated_gpu_seconds: f64,
    pub adapter: String,
}

impl Job {
    #[must_use]
    pub const fn done(&self) -> bool {
        self.completed >= self.frame_count
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CutStatus {
    pub id: u32,
    pub completed: u32,
    pub frame_count: u32,
    pub gpu_seconds: f64,
    pub estimated_gpu_seconds: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    /// True while a render process holds the lock.
    pub running: bool,
    pub frames_completed: u32,
    pub frames_total: u32,
    pub gpu_seconds: f64,
    /// GPU seconds still needed, from the pace so far or the proof's
    /// estimate.
    pub remaining_gpu_seconds: f64,
    pub cuts: Vec<CutStatus>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ProduceOptions {
    /// Stop after rendering this many frames in this run.
    pub max_frames: Option<u32>,
    pub gpu: GpuOptions,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProduceReport {
    pub rendered_frames: u32,
    pub complete: bool,
    pub status: Status,
}

pub fn frame_path(production: &Production, cut: u32, frame: u32) -> PathBuf {
    production.path(&format!("frames/cut-{cut:02}/frame_{frame:06}.png"))
}

fn job_path(production: &Production, cut: u32) -> PathBuf {
    production.path(&format!("jobs/cut-{cut:02}.json"))
}

fn load_job(production: &Production, cut: &FinalCut, scene_hash: &str) -> Result<Job> {
    let path = job_path(production, cut.id);
    let job = match fs::read_to_string(&path) {
        Ok(text) => {
            let job: Job = serde_json::from_str(&text)
                .with_context(|| format!("invalid {}", path.display()))?;
            if job.scene_hash != scene_hash || job.frame_count != cut.frame_count {
                bail!(
                    "frames of cut {} were rendered from a different scene; they cannot be continued",
                    cut.id
                );
            }
            job
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Job {
            id: cut.id,
            scene_hash: scene_hash.to_owned(),
            frame_count: cut.frame_count,
            estimated_gpu_seconds: cut.estimated_gpu_seconds,
            ..Job::default()
        },
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
    };
    Ok(job)
}

fn save_job(production: &Production, job: &Job) -> Result<()> {
    let path = job_path(production, job.id);
    let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let mut text = serde_json::to_string_pretty(job).context("could not serialize job")?;
    text.push('\n');
    fs::write(&temporary, text)
        .with_context(|| format!("could not write {}", temporary.display()))?;
    fs::rename(&temporary, &path).with_context(|| format!("could not replace {}", path.display()))
}

/// Holds the render lock of a production for as long as it lives, so two
/// renders never fight over the GPU or write the same frames.
struct Lock {
    path: PathBuf,
}

impl Lock {
    fn acquire(production: &Production) -> Result<Self> {
        let path = production.path(LOCK_FILE);
        if let Some(pid) = lock_holder(production) {
            bail!("production rendering is already running as process {pid}");
        }
        // A lock without a live holder is left over from a crash.
        let _ = fs::remove_file(&path);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("could not take the render lock {}", path.display()))?;
        write!(file, "{}", std::process::id()).context("could not write the render lock")?;
        Ok(Self { path })
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Process id of a live render holding the lock, if any.
fn lock_holder(production: &Production) -> Option<u32> {
    let pid: u32 = fs::read_to_string(production.path(LOCK_FILE))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    std::path::Path::new(&format!("/proc/{pid}"))
        .exists()
        .then_some(pid)
}

/// Renders the frames that are still missing.
pub fn run(
    production: &Production,
    state: &mut State,
    budget: &mut Budget,
    options: &ProduceOptions,
) -> Result<ProduceReport> {
    if state.stage != Stage::Production {
        bail!(
            "production rendering happens in stage production; the production is in stage {}",
            state.stage.as_str()
        );
    }
    let manifest = FinalManifest::load_current(production)?;
    let duty_cycle = options
        .gpu
        .duty_cycle_percent
        .unwrap_or(budget.max_gpu_duty_cycle);
    budget.check_duty_cycle(duty_cycle)?;
    let gpu = GpuOptions {
        duty_cycle_percent: Some(duty_cycle),
        ..options.gpu.clone()
    };
    let _lock = Lock::acquire(production)?;

    let mut renderer: Option<Renderer> = None;
    let mut rendered = 0u32;
    let mut budget_left = options.max_frames;
    'cuts: for cut in &manifest.cuts {
        let text = fs::read_to_string(production.path(&cut.scene))
            .with_context(|| format!("could not read {}", cut.scene))?;
        let mut job = load_job(production, cut, &content_hash(&text))?;
        if job.done() {
            continue;
        }
        let scene = load_scene(production.path(&cut.scene))?;
        fs::create_dir_all(production.path(&format!("frames/cut-{:02}", cut.id)))
            .context("could not create the frame directory")?;
        match &mut renderer {
            Some(renderer) => pollster::block_on(renderer.replace_fractal(scene.config.clone()))
                .with_context(|| format!("could not compile cut {}", cut.id))?,
            None => renderer = Some(gpu.renderer(scene.config.clone())?),
        }
        let renderer = renderer.as_ref().expect("renderer was just initialised");
        job.adapter.clone_from(&renderer.adapter_info().name);

        while !job.done() {
            if budget_left == Some(0) {
                save_job(production, &job)?;
                break 'cuts;
            }
            let index = job.completed;
            let (config, time_seconds) =
                frame_config(&scene.config, scene.animation.as_ref(), index)?;
            let image = renderer
                .render_frame_with_config(&config, index, time_seconds)
                .with_context(|| format!("could not render cut {} frame {index}", cut.id))?;
            // The frame is written before it is counted, so an interruption
            // can only lose work, never claim a frame that is not there.
            sheet::save_png(
                &frame_path(production, cut.id, index),
                image.width(),
                image.height(),
                image.pixels(),
            )?;
            let seconds = image.gpu_time().as_secs_f64();
            job.completed += 1;
            job.gpu_seconds += seconds;
            budget.spent.production_gpu_seconds += seconds;
            rendered += 1;
            if let Some(left) = &mut budget_left {
                *left -= 1;
            }
            if job.completed % CHECKPOINT_FRAMES == 0 || job.done() {
                save_job(production, &job)?;
                production.save_budget(budget)?;
            }
        }
    }
    production.save_budget(budget)?;

    let status = status(production)?;
    let complete = status.frames_completed == status.frames_total;
    if complete {
        let missing = verify_frames(production, &manifest)?;
        state.record_gate(
            "frames-complete",
            missing.is_empty(),
            if missing.is_empty() {
                format!("all {} frames are rendered and decode", status.frames_total)
            } else {
                missing.join("; ")
            },
        )?;
        state.advance_if_ready()?;
    }
    Ok(ProduceReport {
        rendered_frames: rendered,
        complete,
        status,
    })
}

/// Reports how far production rendering has come.
pub fn status(production: &Production) -> Result<Status> {
    let manifest = FinalManifest::load_current(production)?;
    let mut cuts = Vec::with_capacity(manifest.cuts.len());
    let mut warnings = Vec::new();
    let mut remaining = 0.0;
    for cut in &manifest.cuts {
        let path = job_path(production, cut.id);
        let job: Job = match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("invalid {}", path.display()))?,
            Err(_) => Job {
                id: cut.id,
                frame_count: cut.frame_count,
                estimated_gpu_seconds: cut.estimated_gpu_seconds,
                ..Job::default()
            },
        };
        let left = f64::from(job.frame_count - job.completed.min(job.frame_count));
        let estimated_frame = cut.estimated_gpu_seconds / f64::from(cut.frame_count.max(1));
        // The pace of the cut itself is the better predictor once it exists.
        let frame_seconds = if job.completed > 0 {
            job.gpu_seconds / f64::from(job.completed)
        } else {
            estimated_frame
        };
        remaining += left * frame_seconds;
        if job.completed > 0 && frame_seconds > estimated_frame * SLOW_FACTOR {
            warnings.push(format!(
                "cut {} renders at {frame_seconds:.2} s per frame, above {SLOW_FACTOR} times the estimated {estimated_frame:.2} s",
                cut.id
            ));
        }
        cuts.push(CutStatus {
            id: cut.id,
            completed: job.completed,
            frame_count: job.frame_count,
            gpu_seconds: job.gpu_seconds,
            estimated_gpu_seconds: cut.estimated_gpu_seconds,
        });
    }
    Ok(Status {
        running: lock_holder(production).is_some(),
        frames_completed: cuts.iter().map(|cut| cut.completed).sum(),
        frames_total: cuts.iter().map(|cut| cut.frame_count).sum(),
        gpu_seconds: cuts.iter().map(|cut| cut.gpu_seconds).sum(),
        remaining_gpu_seconds: remaining,
        cuts,
        warnings,
    })
}

/// Decodes every frame and reports those that are missing, damaged, or of
/// the wrong size.
fn verify_frames(production: &Production, manifest: &FinalManifest) -> Result<Vec<String>> {
    let format = Manifest::load_current(production)?.format;
    let mut problems = Vec::new();
    for cut in &manifest.cuts {
        for frame in 0..cut.frame_count {
            let path = frame_path(production, cut.id, frame);
            match image::open(&path) {
                Ok(image) if (image.width(), image.height()) == (format.width, format.height) => {}
                Ok(image) => problems.push(format!(
                    "cut {} frame {frame} is {}x{}, not {}x{}",
                    cut.id,
                    image.width(),
                    image.height(),
                    format.width,
                    format.height
                )),
                Err(error) => {
                    problems.push(format!(
                        "cut {} frame {frame} does not decode: {error}",
                        cut.id
                    ));
                }
            }
        }
    }
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Ledger;

    fn production(name: &str) -> (Production, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "fractal-harness-produce-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let production =
            Production::create(&root, "film", &Ledger::default(), 1, Budget::default()).unwrap();
        (production, root)
    }

    fn final_cut() -> FinalCut {
        FinalCut {
            id: 3,
            scene: "compiled/final/cut-03.yaml".to_owned(),
            frame_count: 40,
            estimated_gpu_seconds: 80.0,
        }
    }

    #[test]
    fn a_new_job_starts_empty_and_survives_a_save() {
        let (production, root) = production("job");
        let cut = final_cut();
        let mut job = load_job(&production, &cut, "abc").unwrap();
        assert_eq!((job.completed, job.frame_count), (0, 40));
        assert!(!job.done());

        job.completed = 12;
        job.gpu_seconds = 30.0;
        save_job(&production, &job).unwrap();
        assert_eq!(load_job(&production, &cut, "abc").unwrap(), job);

        job.completed = 40;
        assert!(job.done());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn frames_of_another_scene_are_never_continued() {
        let (production, root) = production("hash");
        let cut = final_cut();
        let mut job = load_job(&production, &cut, "abc").unwrap();
        job.completed = 5;
        save_job(&production, &job).unwrap();

        let error = load_job(&production, &cut, "changed")
            .unwrap_err()
            .to_string();
        assert!(error.contains("different scene"), "{error}");
        let longer = FinalCut {
            frame_count: 41,
            ..cut
        };
        assert!(load_job(&production, &longer, "abc").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_lock_excludes_a_second_render_and_clears_itself() {
        let (production, root) = production("lock");
        assert_eq!(lock_holder(&production), None);
        {
            let _lock = Lock::acquire(&production).unwrap();
            assert_eq!(lock_holder(&production), Some(std::process::id()));
            let error = Lock::acquire(&production).err().unwrap().to_string();
            assert!(error.contains("already running"), "{error}");
        }
        assert_eq!(lock_holder(&production), None);
        assert!(!production.path(LOCK_FILE).exists());

        // A lock left by a process that no longer exists is taken over.
        fs::write(production.path(LOCK_FILE), "4294967294").unwrap();
        assert_eq!(lock_holder(&production), None);
        let _lock = Lock::acquire(&production).unwrap();
        assert_eq!(lock_holder(&production), Some(std::process::id()));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rendering_is_refused_outside_its_stage() {
        let (production, root) = production("stage");
        let mut state = production.load_state().unwrap();
        let mut budget = production.load_budget().unwrap();
        let error = run(
            &production,
            &mut state,
            &mut budget,
            &ProduceOptions::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("stage scout"), "{error}");
        fs::remove_dir_all(root).unwrap();
    }
}
