use std::{
    any::Any,
    collections::HashMap,
    fs,
    io::ErrorKind,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use image::{Rgba, RgbaImage, imageops};
use serde::{Deserialize, Serialize};

use crate::{
    Artifact, EncodeRequest, PreviewRequest, PreviewResult, ProjectStore, RenderRequest,
    RenderResult,
    artifact::{cleanup_abandoned_temporary_files, save_png_atomic},
    encode::encode_sequence,
    preview::render_preview_with_progress,
    project::{unix_time_ms, write_json_atomic},
    render::{read_sequence_manifest, render_sequence_with_progress},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobKind {
    Preview,
    Compare,
    Render,
    Encode,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobStatus {
    Queued,
    Running,
    PauseRequested,
    Paused,
    CancelRequested,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl JobStatus {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct JobProgress {
    pub completed: u32,
    pub total: u32,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub causes: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ResourceBudget {
    pub maximum_concurrent_jobs: usize,
    pub maximum_preview_frames: u32,
    pub maximum_render_frames: u32,
    pub maximum_width: u32,
    pub maximum_height: u32,
    pub maximum_gpu_duty_cycle: f64,
    pub maximum_estimated_output_bytes: u64,
    pub maximum_wall_time_seconds: u64,
}

impl Default for ResourceBudget {
    fn default() -> Self {
        Self {
            maximum_concurrent_jobs: 2,
            maximum_preview_frames: 25,
            maximum_render_frames: 1_000_000,
            maximum_width: 8_192,
            maximum_height: 8_192,
            maximum_gpu_duty_cycle: 100.0,
            maximum_estimated_output_bytes: 1_000_000_000_000,
            maximum_wall_time_seconds: 7 * 24 * 60 * 60,
        }
    }
}

impl ResourceBudget {
    pub fn validate(&self) -> Result<()> {
        if self.maximum_concurrent_jobs == 0 || self.maximum_concurrent_jobs > 64 {
            bail!("maximum_concurrent_jobs must be in 1..=64");
        }
        if self.maximum_preview_frames == 0
            || self.maximum_render_frames == 0
            || self.maximum_width == 0
            || self.maximum_height == 0
            || self.maximum_estimated_output_bytes == 0
            || self.maximum_wall_time_seconds == 0
        {
            bail!("resource budget dimensions, counts, bytes, and wall time must be positive");
        }
        if !self.maximum_gpu_duty_cycle.is_finite()
            || !(1.0..=100.0).contains(&self.maximum_gpu_duty_cycle)
        {
            bail!("maximum_gpu_duty_cycle must be in 1.0..=100.0 percent");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JobManifest {
    pub version: u32,
    #[serde(default = "workflow_version")]
    pub harness_version: String,
    pub git_commit: Option<String>,
    pub id: String,
    pub project_id: String,
    pub kind: JobKind,
    pub status: JobStatus,
    pub revision_id: Option<String>,
    pub source_job_id: Option<String>,
    pub created_unix_ms: u64,
    pub updated_unix_ms: u64,
    pub progress: JobProgress,
    pub request: serde_json::Value,
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub error: Option<ToolError>,
    pub resource_budget: ResourceBudget,
}

fn workflow_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

#[derive(Clone)]
pub struct Harness {
    store: ProjectStore,
    budget: ResourceBudget,
    jobs: Arc<Mutex<HashMap<String, Arc<JobControl>>>>,
    next_job: Arc<AtomicU64>,
}

struct JobControl {
    kind: JobKind,
    manifest: Mutex<JobManifest>,
    manifest_path: PathBuf,
    cancel: AtomicBool,
    pause: AtomicBool,
}

struct ExecutionContext {
    control: Arc<JobControl>,
    started: Instant,
}

impl Harness {
    pub fn new(store: ProjectStore) -> Result<Self> {
        Self::try_with_budget(store, ResourceBudget::default())
    }

    pub fn try_with_budget(store: ProjectStore, budget: ResourceBudget) -> Result<Self> {
        budget.validate()?;
        cleanup_abandoned_temporary_files(store.root())?;
        recover_interrupted_jobs(&store)?;
        Ok(Self {
            store,
            budget,
            jobs: Arc::new(Mutex::new(HashMap::new())),
            next_job: Arc::new(AtomicU64::new(1)),
        })
    }

    #[must_use]
    pub fn store(&self) -> &ProjectStore {
        &self.store
    }

    #[must_use]
    pub const fn budget(&self) -> &ResourceBudget {
        &self.budget
    }

    pub fn start_preview(&self, mut request: PreviewRequest) -> Result<JobManifest> {
        self.ensure_capacity()?;
        let (revision, spec) = self
            .store
            .scene(&request.project_id, request.revision_id.as_deref())?;
        request.revision_id = Some(revision.id.clone());
        let loaded = spec.resolve()?;
        let frame_count = loaded
            .animation
            .as_ref()
            .map_or(1, |value| value.frame_count);
        let requested_count = if request.frames.is_empty() {
            if frame_count == 1 { 1 } else { 5 }
        } else {
            request.frames.len() as u32
        };
        if requested_count > self.budget.maximum_preview_frames {
            bail!(
                "preview requests {requested_count} frames, budget permits {}",
                self.budget.maximum_preview_frames
            );
        }
        self.validate_gpu_budget(request.gpu_duty_cycle)?;
        let request_json = serde_json::to_value(&request)?;
        let (control, manifest) = self.create_job(
            &request.project_id,
            JobKind::Preview,
            Some(revision.id),
            None,
            request_json,
        )?;
        let store = self.store.clone();
        let output = self
            .store
            .run_directory(&request.project_id, &manifest.id)?
            .join("preview")
            .join(request.profile.label());
        self.spawn(control, move |context| {
            let mut progress = |completed, total| {
                context.checkpoint()?;
                context.progress(completed, total, "rendering preview")
            };
            let result = render_preview_with_progress(&store, &request, &output, &mut progress)?;
            let mut artifacts = result
                .frames
                .iter()
                .map(|frame| frame.artifact.clone())
                .collect::<Vec<_>>();
            artifacts.push(result.contact_sheet.clone());
            artifacts.push(result.metrics_manifest.clone());
            Ok((serde_json::to_value(result)?, artifacts))
        });
        Ok(manifest)
    }

    pub fn start_compare(
        &self,
        project_id: &str,
        preview_job_ids: Vec<String>,
    ) -> Result<JobManifest> {
        self.ensure_capacity()?;
        if preview_job_ids.is_empty() {
            bail!("preview comparison requires at least one job id");
        }
        let mut previews = Vec::with_capacity(preview_job_ids.len());
        let mut revision = None;
        let mut revisions_match = true;
        for job_id in &preview_job_ids {
            let manifest = self.job_status(project_id, job_id)?;
            if manifest.kind != JobKind::Preview || manifest.status != JobStatus::Completed {
                bail!("comparison source {job_id} is not a completed preview job");
            }
            let result: PreviewResult = serde_json::from_value(
                manifest
                    .result
                    .context("completed preview has no structured result")?,
            )?;
            if let Some(existing) = &revision {
                revisions_match &= existing == &result.revision_id;
            } else {
                revision = Some(result.revision_id.clone());
            }
            previews.push((job_id.clone(), result));
        }
        let request = serde_json::json!({"preview_job_ids": preview_job_ids});
        let (control, manifest) = self.create_job(
            project_id,
            JobKind::Compare,
            revisions_match.then_some(revision).flatten(),
            None,
            request,
        )?;
        let output = self
            .store
            .run_directory(project_id, &manifest.id)?
            .join("comparison");
        self.spawn(control, move |context| {
            context.checkpoint()?;
            fs::create_dir_all(&output)?;
            let path = output.join("contact-sheet.png");
            combine_contact_sheets(&previews, &path)?;
            let artifact = Artifact::from_file("comparison-contact-sheet", "image/png", path)?;
            let summaries = previews
                .iter()
                .map(|(job_id, preview)| {
                    serde_json::json!({
                        "job_id": job_id,
                        "revision_id": preview.revision_id,
                        "profile": preview.profile,
                        "frames": preview.frames.iter().map(|frame| {
                            serde_json::json!({
                                "frame_index": frame.frame_index,
                                "metrics": frame.metrics,
                            })
                        }).collect::<Vec<_>>()
                    })
                })
                .collect::<Vec<_>>();
            context.progress(1, 1, "comparison complete")?;
            Ok((
                serde_json::json!({
                    "sources": summaries,
                    "contact_sheet": artifact,
                }),
                vec![artifact],
            ))
        });
        Ok(manifest)
    }

    pub fn start_render(
        &self,
        mut request: RenderRequest,
        resume_source_job_id: Option<String>,
    ) -> Result<JobManifest> {
        self.ensure_capacity()?;
        let (revision, spec) = self
            .store
            .scene(&request.project_id, request.revision_id.as_deref())?;
        request.revision_id = Some(revision.id.clone());
        self.validate_gpu_budget(request.gpu_duty_cycle)?;
        let scene = spec.resolve()?;
        self.validate_render_budget(&request, &scene)?;

        let output = if let Some(source_job_id) = &resume_source_job_id {
            let source = self.job_status(&request.project_id, source_job_id)?;
            if source.kind != JobKind::Render {
                bail!("resume source {source_job_id} is not a render job");
            }
            if source.revision_id.as_deref() != Some(&revision.id) {
                bail!("resume source revision does not match requested revision");
            }
            request.resume = true;
            request.overwrite = false;
            self.store
                .run_directory(&request.project_id, source_job_id)?
                .join("render")
                .join("frames")
        } else {
            PathBuf::new()
        };
        let request_json = serde_json::to_value(&request)?;
        let (control, manifest) = self.create_job(
            &request.project_id,
            JobKind::Render,
            Some(revision.id),
            resume_source_job_id,
            request_json,
        )?;
        let output = if output.as_os_str().is_empty() {
            self.store
                .run_directory(&request.project_id, &manifest.id)?
                .join("render")
                .join("frames")
        } else {
            output
        };
        let store = self.store.clone();
        self.spawn(control, move |context| {
            let mut progress = |completed, total| {
                context.checkpoint()?;
                context.progress(completed, total, "rendering final frames")
            };
            let result = render_sequence_with_progress(&store, &request, &output, &mut progress)?;
            let artifacts = vec![result.sequence_artifact.clone()];
            Ok((serde_json::to_value(result)?, artifacts))
        });
        Ok(manifest)
    }

    pub fn start_encode(&self, request: EncodeRequest) -> Result<JobManifest> {
        self.ensure_capacity()?;
        let source = self.job_status(&request.project_id, &request.source_job_id)?;
        if source.kind != JobKind::Render {
            bail!(
                "encode source {} is not a render job",
                request.source_job_id
            );
        }
        let revision_id = source
            .revision_id
            .clone()
            .context("render source is not bound to a revision")?;
        let frames_directory = if let Some(result) = &source.result {
            serde_json::from_value::<RenderResult>(result.clone())?.frames_directory
        } else if let Some(resume_source) = &source.source_job_id {
            self.store
                .run_directory(&request.project_id, resume_source)?
                .join("render")
                .join("frames")
        } else {
            self.store
                .run_directory(&request.project_id, &request.source_job_id)?
                .join("render")
                .join("frames")
        };
        let sequence = read_sequence_manifest(&frames_directory)?;
        if sequence.revision_id != revision_id {
            bail!("render source manifest revision does not match its job");
        }
        let request_json = serde_json::to_value(&request)?;
        let (control, manifest) = self.create_job(
            &request.project_id,
            JobKind::Encode,
            Some(revision_id),
            Some(request.source_job_id.clone()),
            request_json,
        )?;
        let output = self
            .store
            .run_directory(&request.project_id, &manifest.id)?
            .join("encode");
        let store = self.store.clone();
        self.spawn(control, move |context| {
            context.checkpoint()?;
            context.progress(0, 1, "encoding video")?;
            let result = encode_sequence(&store, &request, &frames_directory, &output, &|| {
                context.control.cancel.load(Ordering::Acquire)
            });
            let result = match result {
                Ok(result) => result,
                Err(error) => return Err(error),
            };
            context.progress(1, 1, "encode complete")?;
            let artifacts = vec![result.output.clone()];
            Ok((serde_json::to_value(result)?, artifacts))
        });
        Ok(manifest)
    }

    pub fn job_status(&self, project_id: &str, job_id: &str) -> Result<JobManifest> {
        if let Some(control) = self.jobs.lock().expect("job registry poisoned").get(job_id) {
            let manifest = control
                .manifest
                .lock()
                .expect("job manifest poisoned")
                .clone();
            if manifest.project_id != project_id {
                bail!("job {job_id} does not belong to project {project_id}");
            }
            return Ok(manifest);
        }
        let path = self.manifest_path(project_id, job_id)?;
        let bytes = fs::read(&path)
            .with_context(|| format!("could not read job manifest {}", path.display()))?;
        let mut manifest: JobManifest = serde_json::from_slice(&bytes)?;
        if !manifest.status.is_terminal() {
            mark_interrupted(&mut manifest);
            write_json_atomic(&path, &manifest)?;
        }
        Ok(manifest)
    }

    pub fn list_jobs(&self, project_id: &str) -> Result<Vec<JobManifest>> {
        let runs = self
            .store
            .root()
            .join("projects")
            .join(project_id)
            .join("runs");
        let mut jobs = Vec::new();
        if !runs.exists() {
            return Ok(jobs);
        }
        for entry in fs::read_dir(runs)? {
            let entry = entry?;
            let path = entry.path().join("manifest.json");
            if !path.is_file() {
                continue;
            }
            let job_id = entry.file_name().to_string_lossy().into_owned();
            jobs.push(self.job_status(project_id, &job_id)?);
        }
        jobs.sort_by_key(|job| job.created_unix_ms);
        Ok(jobs)
    }

    pub fn pause_job(&self, project_id: &str, job_id: &str) -> Result<JobManifest> {
        let control = self.active_control(project_id, job_id)?;
        if control.kind == JobKind::Encode {
            bail!("encode jobs cannot be paused; cancel and start them again instead");
        }
        control.pause.store(true, Ordering::Release);
        control.update(|manifest| {
            if !manifest.status.is_terminal() {
                manifest.status = JobStatus::PauseRequested;
                manifest.progress.message = "pause requested".to_owned();
            }
        })?;
        self.job_status(project_id, job_id)
    }

    pub fn resume_job(&self, project_id: &str, job_id: &str) -> Result<JobManifest> {
        let control = self.active_control(project_id, job_id)?;
        control.pause.store(false, Ordering::Release);
        control.update(|manifest| {
            if matches!(
                manifest.status,
                JobStatus::Paused | JobStatus::PauseRequested
            ) {
                manifest.status = JobStatus::Running;
                manifest.progress.message = "resumed".to_owned();
            }
        })?;
        self.job_status(project_id, job_id)
    }

    pub fn cancel_job(&self, project_id: &str, job_id: &str) -> Result<JobManifest> {
        let control = self.active_control(project_id, job_id)?;
        control.cancel.store(true, Ordering::Release);
        control.pause.store(false, Ordering::Release);
        control.update(|manifest| {
            if !manifest.status.is_terminal() {
                manifest.status = JobStatus::CancelRequested;
                manifest.progress.message = "cancellation requested".to_owned();
            }
        })?;
        self.job_status(project_id, job_id)
    }

    pub fn wait_for_job(
        &self,
        project_id: &str,
        job_id: &str,
        timeout: Duration,
    ) -> Result<JobManifest> {
        let started = Instant::now();
        loop {
            let manifest = self.job_status(project_id, job_id)?;
            if manifest.status.is_terminal() {
                return Ok(manifest);
            }
            if started.elapsed() >= timeout {
                bail!("timed out waiting for job {job_id}");
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn create_job(
        &self,
        project_id: &str,
        kind: JobKind,
        revision_id: Option<String>,
        source_job_id: Option<String>,
        request: serde_json::Value,
    ) -> Result<(Arc<JobControl>, JobManifest)> {
        self.store.project(project_id)?;
        let runs_directory = self
            .store
            .root()
            .join("projects")
            .join(project_id)
            .join("runs");
        fs::create_dir_all(&runs_directory)?;
        let (job_id, run_directory) = loop {
            let sequence = self.next_job.fetch_add(1, Ordering::Relaxed);
            let candidate = format!("job-{}-{sequence}", unix_time_ms());
            let directory = self.store.run_directory(project_id, &candidate)?;
            match fs::create_dir(&directory) {
                Ok(()) => break (candidate, directory),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("could not reserve job directory {}", directory.display())
                    });
                }
            }
        };
        let manifest_path = run_directory.join("manifest.json");
        let now = unix_time_ms();
        let manifest = JobManifest {
            version: 1,
            harness_version: env!("CARGO_PKG_VERSION").to_owned(),
            git_commit: option_env!("FRACTAL_RENDERER_GIT_COMMIT").map(str::to_owned),
            id: job_id.clone(),
            project_id: project_id.to_owned(),
            kind,
            status: JobStatus::Queued,
            revision_id,
            source_job_id,
            created_unix_ms: now,
            updated_unix_ms: now,
            progress: JobProgress::default(),
            request,
            result: None,
            artifacts: Vec::new(),
            warnings: Vec::new(),
            error: None,
            resource_budget: self.budget.clone(),
        };
        if let Err(error) = write_json_atomic(&manifest_path, &manifest) {
            let _ = fs::remove_dir_all(&run_directory);
            return Err(error).context("could not publish initial job manifest");
        }
        let control = Arc::new(JobControl {
            kind,
            manifest: Mutex::new(manifest.clone()),
            manifest_path,
            cancel: AtomicBool::new(false),
            pause: AtomicBool::new(false),
        });
        self.jobs
            .lock()
            .expect("job registry poisoned")
            .insert(job_id, Arc::clone(&control));
        Ok((control, manifest))
    }

    fn spawn<F>(&self, control: Arc<JobControl>, operation: F)
    where
        F: FnOnce(&ExecutionContext) -> Result<(serde_json::Value, Vec<Artifact>)> + Send + 'static,
    {
        thread::spawn(move || {
            let context = ExecutionContext {
                control: Arc::clone(&control),
                started: Instant::now(),
            };
            let started = control.update(|manifest| {
                manifest.status = JobStatus::Running;
                manifest.progress.message = "started".to_owned();
            });
            let outcome = match started {
                Ok(()) => match catch_unwind(AssertUnwindSafe(|| operation(&context))) {
                    Ok(outcome) => outcome,
                    Err(payload) => Err(anyhow!(
                        "job operation panicked: {}",
                        panic_payload_message(&payload)
                    )),
                },
                Err(error) => Err(error.context("could not persist running job state")),
            };
            let cancelled = control.cancel.load(Ordering::Acquire);
            let _ = control.update(|manifest| match outcome {
                Ok((result, artifacts)) => {
                    manifest.status = JobStatus::Completed;
                    manifest.result = Some(result);
                    manifest.artifacts = artifacts;
                    manifest.error = None;
                    manifest.progress.completed = manifest.progress.total;
                    manifest.progress.message = "completed".to_owned();
                }
                Err(error) => {
                    manifest.status = if cancelled {
                        JobStatus::Cancelled
                    } else {
                        JobStatus::Failed
                    };
                    manifest.error = Some(tool_error(&error, cancelled));
                    manifest.progress.message = if cancelled {
                        "cancelled".to_owned()
                    } else {
                        "failed".to_owned()
                    };
                }
            });
        });
    }

    fn active_control(&self, project_id: &str, job_id: &str) -> Result<Arc<JobControl>> {
        let control = self
            .jobs
            .lock()
            .expect("job registry poisoned")
            .get(job_id)
            .cloned()
            .ok_or_else(|| anyhow!("job {job_id} is not active in this harness process"))?;
        let manifest = control.manifest.lock().expect("job manifest poisoned");
        if manifest.project_id != project_id {
            bail!("job {job_id} does not belong to project {project_id}");
        }
        if manifest.status.is_terminal() {
            bail!("job {job_id} is already in terminal state");
        }
        drop(manifest);
        Ok(control)
    }

    fn ensure_capacity(&self) -> Result<()> {
        let jobs = self.jobs.lock().expect("job registry poisoned");
        let active = jobs
            .values()
            .filter(|control| {
                !control
                    .manifest
                    .lock()
                    .expect("job manifest poisoned")
                    .status
                    .is_terminal()
            })
            .count();
        if active >= self.budget.maximum_concurrent_jobs {
            bail!(
                "active job count {active} reached resource budget {}",
                self.budget.maximum_concurrent_jobs
            );
        }
        Ok(())
    }

    fn validate_gpu_budget(&self, duty_cycle: Option<f64>) -> Result<()> {
        let duty_cycle = duty_cycle.unwrap_or(100.0);
        if !duty_cycle.is_finite()
            || duty_cycle < 1.0
            || duty_cycle > self.budget.maximum_gpu_duty_cycle
        {
            bail!(
                "GPU duty cycle {duty_cycle} exceeds resource budget 1.0..={} percent",
                self.budget.maximum_gpu_duty_cycle
            );
        }
        Ok(())
    }

    fn validate_render_budget(
        &self,
        request: &RenderRequest,
        scene: &fractal_renderer_core::LoadedScene,
    ) -> Result<()> {
        let frame_count = scene
            .animation
            .as_ref()
            .map_or(1, |value| value.frame_count);
        let start = request.start_frame.unwrap_or(0);
        let end = request.end_frame.unwrap_or(frame_count.saturating_sub(1));
        let requested = end.saturating_sub(start).saturating_add(1);
        if requested > self.budget.maximum_render_frames {
            bail!(
                "render requests {requested} frames, budget permits {}",
                self.budget.maximum_render_frames
            );
        }
        let width = scene.config.render.width;
        let height = scene.config.render.height;
        if width > self.budget.maximum_width || height > self.budget.maximum_height {
            bail!(
                "render resolution {width}x{height} exceeds resource budget {}x{}",
                self.budget.maximum_width,
                self.budget.maximum_height
            );
        }
        let estimate = u64::from(width)
            .saturating_mul(u64::from(height))
            .saturating_mul(4)
            .saturating_mul(u64::from(requested));
        if estimate > self.budget.maximum_estimated_output_bytes {
            bail!(
                "render estimated output {estimate} bytes exceeds resource budget {}",
                self.budget.maximum_estimated_output_bytes
            );
        }
        Ok(())
    }

    fn manifest_path(&self, project_id: &str, job_id: &str) -> Result<PathBuf> {
        Ok(self
            .store
            .run_directory(project_id, job_id)?
            .join("manifest.json"))
    }
}

impl JobControl {
    fn update(&self, update: impl FnOnce(&mut JobManifest)) -> Result<()> {
        // Keep mutation and publication under the same lock. Control requests
        // and worker progress can arrive on different threads; publishing after
        // releasing the lock would allow an older snapshot to overwrite a
        // newer terminal state.
        let mut manifest = self.manifest.lock().expect("job manifest poisoned");
        update(&mut manifest);
        manifest.updated_unix_ms = unix_time_ms();
        let mut last_error = None;
        for attempt in 0..3 {
            match write_json_atomic(&self.manifest_path, &*manifest) {
                Ok(()) => return Ok(()),
                Err(error) => last_error = Some(error),
            }
            if attempt < 2 {
                thread::sleep(Duration::from_millis(10));
            }
        }
        Err(last_error.expect("manifest persistence attempted at least once"))
    }
}

impl ExecutionContext {
    fn checkpoint(&self) -> Result<()> {
        if self.control.cancel.load(Ordering::Acquire) {
            bail!("job was cancelled");
        }
        let maximum_seconds = self
            .control
            .manifest
            .lock()
            .expect("job manifest poisoned")
            .resource_budget
            .maximum_wall_time_seconds;
        if self.started.elapsed().as_secs() > maximum_seconds {
            bail!("job exceeded its wall-time resource budget");
        }
        let mut announced_pause = false;
        while self.control.pause.load(Ordering::Acquire) {
            if !announced_pause {
                self.control.update(|manifest| {
                    manifest.status = JobStatus::Paused;
                    manifest.progress.message = "paused".to_owned();
                })?;
                announced_pause = true;
            }
            if self.control.cancel.load(Ordering::Acquire) {
                bail!("job was cancelled while paused");
            }
            thread::sleep(Duration::from_millis(100));
        }
        if announced_pause {
            self.control.update(|manifest| {
                manifest.status = JobStatus::Running;
                manifest.progress.message = "resumed".to_owned();
            })?;
        }
        Ok(())
    }

    fn progress(&self, completed: u32, total: u32, message: &str) -> Result<()> {
        self.control.update(|manifest| {
            manifest.progress = JobProgress {
                completed,
                total,
                message: message.to_owned(),
            };
        })
    }
}

fn tool_error(error: &anyhow::Error, cancelled: bool) -> ToolError {
    let message = error.to_string();
    let diagnostic = format!("{error:#}");
    let code = if cancelled {
        "job_cancelled"
    } else if diagnostic.contains("job operation panicked") {
        "job_panicked"
    } else if diagnostic.contains("GPU acceleration is unavailable") {
        "gpu_unavailable"
    } else if diagnostic.contains("could not execute ffmpeg") || diagnostic.contains("FFmpeg") {
        "ffmpeg_error"
    } else if message.contains("revision conflict") {
        "revision_conflict"
    } else if message.contains("budget") {
        "resource_budget_exceeded"
    } else if message.contains("invalid")
        || message.contains("outside")
        || message.contains("does not match")
    {
        "validation_error"
    } else {
        "workflow_error"
    };
    ToolError {
        code: code.to_owned(),
        message,
        causes: error.chain().skip(1).map(ToString::to_string).collect(),
    }
}

fn panic_payload_message(payload: &Box<dyn Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}

fn recover_interrupted_jobs(store: &ProjectStore) -> Result<usize> {
    let projects = store.root().join("projects");
    if !projects.exists() {
        return Ok(0);
    }
    let mut recovered = 0;
    for project in fs::read_dir(&projects)? {
        let project = project?;
        if !project.file_type()?.is_dir() {
            continue;
        }
        let runs = project.path().join("runs");
        if !runs.exists() {
            continue;
        }
        for run in fs::read_dir(runs)? {
            let run = run?;
            if !run.file_type()?.is_dir() {
                continue;
            }
            let path = run.path().join("manifest.json");
            if !path.is_file() {
                continue;
            }
            let bytes = fs::read(&path)
                .with_context(|| format!("could not read job manifest {}", path.display()))?;
            let mut manifest: JobManifest = serde_json::from_slice(&bytes)
                .with_context(|| format!("job manifest {} is invalid", path.display()))?;
            if manifest.status.is_terminal() {
                continue;
            }
            mark_interrupted(&mut manifest);
            write_json_atomic(&path, &manifest)?;
            recovered += 1;
        }
    }
    Ok(recovered)
}

fn mark_interrupted(manifest: &mut JobManifest) {
    manifest.status = JobStatus::Interrupted;
    manifest.updated_unix_ms = unix_time_ms();
    manifest.progress.message = "interrupted".to_owned();
    manifest.error = Some(ToolError {
        code: "job_interrupted".to_owned(),
        message: "the harness process ended before this job reached a terminal state".to_owned(),
        causes: Vec::new(),
    });
}

fn combine_contact_sheets(previews: &[(String, PreviewResult)], output: &Path) -> Result<()> {
    let mut images = Vec::with_capacity(previews.len());
    for (_, preview) in previews {
        images.push(image::open(&preview.contact_sheet.path)?.to_rgba8());
    }
    let cell_width = images.iter().map(RgbaImage::width).max().unwrap_or(1);
    let cell_height = images.iter().map(RgbaImage::height).max().unwrap_or(1);
    let columns = (images.len() as f64).sqrt().ceil() as u32;
    let rows = (images.len() as u32).div_ceil(columns);
    let mut sheet = RgbaImage::from_pixel(
        cell_width * columns,
        cell_height * rows,
        Rgba([12, 12, 16, 255]),
    );
    for (index, image) in images.iter().enumerate() {
        let x = index as u32 % columns * cell_width;
        let y = index as u32 / columns * cell_height;
        imageops::overlay(&mut sheet, image, i64::from(x), i64::from(y));
    }
    save_png_atomic(output, &sheet)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(1);

    fn test_store(label: &str) -> (ProjectStore, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "fractal-jobs-{label}-{}-{}",
            std::process::id(),
            NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        let store = ProjectStore::new(&root);
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../scenes/examples/alchemy-pseudo-kleinian-target-orbit.yaml");
        store.create("alchemy", source).unwrap();
        (store, root)
    }

    #[test]
    fn terminal_statuses_are_explicit() {
        assert!(JobStatus::Completed.is_terminal());
        assert!(JobStatus::Failed.is_terminal());
        assert!(JobStatus::Cancelled.is_terminal());
        assert!(JobStatus::Interrupted.is_terminal());
        assert!(!JobStatus::Running.is_terminal());
    }

    #[test]
    fn resource_budget_rejects_unbounded_operator_settings() {
        let invalid = ResourceBudget {
            maximum_gpu_duty_cycle: 101.0,
            ..ResourceBudget::default()
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn operation_panic_is_persisted_as_a_failed_terminal_job() {
        let (store, root) = test_store("panic");
        let harness = Harness::new(store.clone()).unwrap();
        let (control, created) = harness
            .create_job(
                "alchemy",
                JobKind::Compare,
                None,
                None,
                serde_json::json!({"test": "panic"}),
            )
            .unwrap();
        harness.spawn(control, |_| -> Result<(serde_json::Value, Vec<Artifact>)> {
            panic!("intentional worker failure");
        });
        let terminal = harness
            .wait_for_job("alchemy", &created.id, Duration::from_secs(5))
            .unwrap();
        assert_eq!(terminal.status, JobStatus::Failed);
        assert_eq!(terminal.error.as_ref().unwrap().code, "job_panicked");
        assert!(
            terminal
                .error
                .as_ref()
                .unwrap()
                .message
                .contains("intentional worker failure")
        );
        drop(harness);

        let restarted = Harness::new(store).unwrap();
        let persisted = restarted.job_status("alchemy", &created.id).unwrap();
        assert_eq!(persisted.status, JobStatus::Failed);
        assert_eq!(persisted.error.unwrap().code, "job_panicked");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn asynchronous_job_can_be_inspected_paused_resumed_and_cancelled() {
        let (store, root) = test_store("control");
        let harness = Harness::new(store).unwrap();
        let (control, created) = harness
            .create_job(
                "alchemy",
                JobKind::Compare,
                None,
                None,
                serde_json::json!({"test": "control lifecycle"}),
            )
            .unwrap();
        harness.spawn(control, |context| {
            for step in 0..100 {
                context.checkpoint()?;
                context.progress(step, 100, "working")?;
                thread::sleep(Duration::from_millis(10));
            }
            Ok((serde_json::json!({"done": true}), Vec::new()))
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = harness.job_status("alchemy", &created.id).unwrap();
            if status.status == JobStatus::Running && status.progress.completed > 0 {
                break;
            }
            assert!(Instant::now() < deadline, "job did not begin working");
            thread::sleep(Duration::from_millis(5));
        }

        harness.pause_job("alchemy", &created.id).unwrap();
        let paused_progress = loop {
            let status = harness.job_status("alchemy", &created.id).unwrap();
            if status.status == JobStatus::Paused {
                break status.progress.completed;
            }
            assert!(Instant::now() < deadline, "job did not pause");
            thread::sleep(Duration::from_millis(5));
        };
        thread::sleep(Duration::from_millis(40));
        let still_paused = harness.job_status("alchemy", &created.id).unwrap();
        assert_eq!(still_paused.status, JobStatus::Paused);
        assert_eq!(still_paused.progress.completed, paused_progress);

        harness.resume_job("alchemy", &created.id).unwrap();
        loop {
            let status = harness.job_status("alchemy", &created.id).unwrap();
            if status.progress.completed > paused_progress {
                break;
            }
            assert!(Instant::now() < deadline, "job did not resume");
            thread::sleep(Duration::from_millis(5));
        }

        harness.cancel_job("alchemy", &created.id).unwrap();
        let terminal = harness
            .wait_for_job("alchemy", &created.id, Duration::from_secs(5))
            .unwrap();
        assert_eq!(terminal.status, JobStatus::Cancelled);
        assert_eq!(terminal.error.as_ref().unwrap().code, "job_cancelled");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_recovers_all_interrupted_job_kinds_and_abandoned_temporaries() {
        let (store, root) = test_store("recovery");
        let harness = Harness::new(store.clone()).unwrap();
        let mut jobs = Vec::new();
        for kind in [JobKind::Preview, JobKind::Render, JobKind::Encode] {
            let (control, manifest) = harness
                .create_job(
                    "alchemy",
                    kind,
                    None,
                    None,
                    serde_json::json!({"test": format!("{kind:?}")}),
                )
                .unwrap();
            control
                .update(|manifest| {
                    manifest.status = JobStatus::Running;
                    manifest.progress.message = "publishing artifact".to_owned();
                })
                .unwrap();
            jobs.push(manifest.id);
        }
        let abandoned = crate::artifact::temporary_file_path(
            &root.join("projects/alchemy/runs/abandoned.mp4"),
            true,
        )
        .unwrap();
        fs::write(&abandoned, b"partial video").unwrap();
        drop(harness);

        let restarted = Harness::new(store).unwrap();
        assert!(!abandoned.exists());
        for job_id in jobs {
            let manifest = restarted.job_status("alchemy", &job_id).unwrap();
            assert_eq!(manifest.status, JobStatus::Interrupted);
            assert_eq!(manifest.error.as_ref().unwrap().code, "job_interrupted");
            let persisted: JobManifest = serde_json::from_slice(
                &fs::read(
                    root.join("projects/alchemy/runs")
                        .join(&job_id)
                        .join("manifest.json"),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(persisted.status, JobStatus::Interrupted);
        }
        fs::remove_dir_all(root).unwrap();
    }
}
