use std::{
    env, fs,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use fractal_renderer_workflow::{
    EncodeRequest, Harness, PatchOperation, PreviewRequest, ProjectStore, RenderRequest,
    ResourceBudget, RouteInspectRequest, TargetSearchRequest, inspect_route, search_targets,
};
use serde::Deserialize;
use serde_json::{Value, json};

const PROTOCOL_VERSION: u32 = 1;
const TEMPORARY_FILE_MARKER: &str = ".fractal-tmp-";
static NEXT_IDEMPOTENCY_TEMPORARY: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Deserialize)]
struct ToolRequest {
    #[serde(default)]
    id: Value,
    tool: String,
    #[serde(default)]
    idempotency_key: Option<String>,
    #[serde(default)]
    arguments: Value,
}

#[derive(Debug, Deserialize)]
struct ProjectCreateArguments {
    project_id: String,
    scene_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct ProjectArguments {
    project_id: String,
}

#[derive(Debug, Deserialize)]
struct SceneArguments {
    project_id: String,
    #[serde(default)]
    revision_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SceneValidateArguments {
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    revision_id: Option<String>,
    #[serde(default)]
    yaml: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ScenePatchArguments {
    project_id: String,
    base_revision: String,
    operations: Vec<PatchOperation>,
    #[serde(default)]
    promote: bool,
}

#[derive(Debug, Deserialize)]
struct ScenePromoteArguments {
    project_id: String,
    revision_id: String,
    #[serde(default)]
    expected_current_revision: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SceneUndoArguments {
    project_id: String,
    #[serde(default)]
    expected_current_revision: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PreviewCompareArguments {
    project_id: String,
    preview_job_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RenderStartArguments {
    #[serde(flatten)]
    request: RenderRequest,
    #[serde(default)]
    resume_source_job_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JobArguments {
    project_id: String,
    job_id: String,
}

#[derive(Debug, Deserialize)]
struct JobWaitArguments {
    project_id: String,
    job_id: String,
    #[serde(default = "default_wait_ms")]
    timeout_ms: u64,
}

struct Server {
    harness: Harness,
    workspace_root: PathBuf,
}

struct ServerOptions {
    root: PathBuf,
    budget: ResourceBudget,
}

#[derive(Debug, Deserialize, serde::Serialize)]
struct IdempotencyRecord {
    tool: String,
    arguments: Value,
    response: Value,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("fractal-harness fatal error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let options = parse_arguments()?;
    let workspace_root = env::current_dir().context("could not resolve workspace directory")?;
    let server = Server {
        harness: Harness::try_with_budget(ProjectStore::new(options.root), options.budget)?,
        workspace_root,
    };
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.context("could not read harness request")?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<ToolRequest>(&line) {
            Ok(request) => server.handle(request),
            Err(error) => json!({
                "id": Value::Null,
                "ok": false,
                "error": {
                    "code": "invalid_request",
                    "message": error.to_string(),
                    "causes": []
                }
            }),
        };
        serde_json::to_writer(&mut stdout, &response)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    Ok(())
}

impl Server {
    fn handle(&self, request: ToolRequest) -> Value {
        let id = request.id.clone();
        if let Some(key) = request.idempotency_key.as_deref() {
            match self.cached_response(key, &request.tool, &request.arguments) {
                Ok(Some(response)) => return with_response_id(id, response),
                Ok(None) => {}
                Err(error) => {
                    return with_response_id(id, error_response(&error));
                }
            }
        }
        let response = match self.dispatch(&request.tool, request.arguments.clone()) {
            Ok(result) => json!({"ok": true, "result": result}),
            Err(error) => error_response(&error),
        };
        if let Some(key) = request.idempotency_key.as_deref()
            && let Err(error) =
                self.store_cached_response(key, &request.tool, &request.arguments, &response)
        {
            return with_response_id(id, error_response(&error));
        }
        with_response_id(id, response)
    }

    fn dispatch(&self, tool: &str, arguments: Value) -> Result<Value> {
        match tool {
            "capabilities.describe" => Ok(capabilities(Some(self.harness.budget()))),
            "project.create" => {
                let arguments: ProjectCreateArguments = decode(arguments)?;
                let source = self.confined_source_path(&arguments.scene_path)?;
                Ok(serde_json::to_value(
                    self.harness.store().create(&arguments.project_id, source)?,
                )?)
            }
            "project.status" => {
                let arguments: ProjectArguments = decode(arguments)?;
                let project = self.harness.store().project(&arguments.project_id)?;
                let jobs = self.harness.list_jobs(&arguments.project_id)?;
                Ok(json!({"project": project, "jobs": jobs}))
            }
            "scene.get" => {
                let arguments: SceneArguments = decode(arguments)?;
                let (revision, scene) = self
                    .harness
                    .store()
                    .scene(&arguments.project_id, arguments.revision_id.as_deref())?;
                Ok(json!({"revision": revision, "scene": scene}))
            }
            "scene.validate" => {
                let arguments: SceneValidateArguments = decode(arguments)?;
                match (arguments.yaml, arguments.project_id) {
                    (Some(yaml), None) => {
                        let scene = fractal_renderer_core::parse_scene_spec(&yaml)?;
                        Ok(json!({"valid": true, "scene": scene}))
                    }
                    (None, Some(project_id)) => {
                        let (revision, scene) = self
                            .harness
                            .store()
                            .scene(&project_id, arguments.revision_id.as_deref())?;
                        Ok(json!({"valid": true, "revision": revision, "scene": scene}))
                    }
                    _ => bail!("scene.validate requires exactly one of yaml or project_id"),
                }
            }
            "scene.patch" => {
                let arguments: ScenePatchArguments = decode(arguments)?;
                Ok(serde_json::to_value(self.harness.store().patch(
                    &arguments.project_id,
                    &arguments.base_revision,
                    &arguments.operations,
                    arguments.promote,
                )?)?)
            }
            "scene.promote" => {
                let arguments: ScenePromoteArguments = decode(arguments)?;
                Ok(serde_json::to_value(self.harness.store().promote(
                    &arguments.project_id,
                    &arguments.revision_id,
                    arguments.expected_current_revision.as_deref(),
                )?)?)
            }
            "scene.describe_parameters" => {
                let _: Value = arguments;
                Ok(serde_json::to_value(ProjectStore::parameters())?)
            }
            "route.inspect" => {
                let request: RouteInspectRequest = decode(arguments)?;
                Ok(serde_json::to_value(inspect_route(
                    self.harness.store(),
                    &request,
                )?)?)
            }
            "target.search" => {
                let request: TargetSearchRequest = decode(arguments)?;
                Ok(serde_json::to_value(search_targets(
                    self.harness.store(),
                    &request,
                )?)?)
            }
            "preview.start" => {
                let request: PreviewRequest = decode(arguments)?;
                Ok(serde_json::to_value(self.harness.start_preview(request)?)?)
            }
            "preview.compare" => {
                let arguments: PreviewCompareArguments = decode(arguments)?;
                Ok(serde_json::to_value(self.harness.start_compare(
                    &arguments.project_id,
                    arguments.preview_job_ids,
                )?)?)
            }
            "render.start" => {
                let arguments: RenderStartArguments = decode(arguments)?;
                Ok(serde_json::to_value(self.harness.start_render(
                    arguments.request,
                    arguments.resume_source_job_id,
                )?)?)
            }
            "encode.start" => {
                let request: EncodeRequest = decode(arguments)?;
                if request.ffmpeg.is_some() {
                    bail!(
                        "the harness does not accept a per-request FFmpeg executable; configure PATH for the server process"
                    );
                }
                Ok(serde_json::to_value(self.harness.start_encode(request)?)?)
            }
            "job.status" => {
                let arguments: JobArguments = decode(arguments)?;
                Ok(serde_json::to_value(
                    self.harness
                        .job_status(&arguments.project_id, &arguments.job_id)?,
                )?)
            }
            "scene.undo" => {
                let arguments: SceneUndoArguments = decode(arguments)?;
                let project = self.harness.store().project(&arguments.project_id)?;
                if let Some(expected) = arguments.expected_current_revision.as_deref()
                    && project.current_revision != expected
                {
                    bail!(
                        "revision conflict: project current revision is {}, not {expected}",
                        project.current_revision
                    );
                }
                let revision = self
                    .harness
                    .store()
                    .revision(&arguments.project_id, &project.current_revision)?;
                let parent = revision
                    .parent_revision
                    .context("current revision has no parent to restore")?;
                Ok(serde_json::to_value(self.harness.store().promote(
                    &arguments.project_id,
                    &parent,
                    Some(&project.current_revision),
                )?)?)
            }
            "job.list" => {
                let arguments: ProjectArguments = decode(arguments)?;
                Ok(serde_json::to_value(
                    self.harness.list_jobs(&arguments.project_id)?,
                )?)
            }
            "job.pause" => {
                let arguments: JobArguments = decode(arguments)?;
                Ok(serde_json::to_value(
                    self.harness
                        .pause_job(&arguments.project_id, &arguments.job_id)?,
                )?)
            }
            "job.resume" => {
                let arguments: JobArguments = decode(arguments)?;
                Ok(serde_json::to_value(
                    self.harness
                        .resume_job(&arguments.project_id, &arguments.job_id)?,
                )?)
            }
            "job.cancel" => {
                let arguments: JobArguments = decode(arguments)?;
                Ok(serde_json::to_value(
                    self.harness
                        .cancel_job(&arguments.project_id, &arguments.job_id)?,
                )?)
            }
            "job.wait" => {
                let arguments: JobWaitArguments = decode(arguments)?;
                let timeout = arguments.timeout_ms.clamp(1, 60_000);
                Ok(serde_json::to_value(self.harness.wait_for_job(
                    &arguments.project_id,
                    &arguments.job_id,
                    Duration::from_millis(timeout),
                )?)?)
            }
            "artifact.list" => {
                let arguments: JobArguments = decode(arguments)?;
                let manifest = self
                    .harness
                    .job_status(&arguments.project_id, &arguments.job_id)?;
                Ok(serde_json::to_value(manifest.artifacts)?)
            }
            _ => bail!("unknown harness tool {tool}"),
        }
    }

    fn confined_source_path(&self, path: &Path) -> Result<PathBuf> {
        let candidate = if path.is_absolute() {
            path.to_owned()
        } else {
            self.workspace_root.join(path)
        };
        let canonical = candidate
            .canonicalize()
            .with_context(|| format!("could not resolve source scene {}", candidate.display()))?;
        if !canonical.starts_with(&self.workspace_root) {
            bail!("source scene must be contained in the harness workspace");
        }
        Ok(canonical)
    }

    fn cached_response(&self, key: &str, tool: &str, arguments: &Value) -> Result<Option<Value>> {
        validate_idempotency_key(key)?;
        let path = self.idempotency_path(key);
        if !path.exists() {
            return Ok(None);
        }
        let record: IdempotencyRecord =
            serde_json::from_slice(&fs::read(&path).with_context(|| {
                format!("could not read idempotency record {}", path.display())
            })?)
            .with_context(|| format!("idempotency record {} is invalid", path.display()))?;
        if record.tool != tool || record.arguments != *arguments {
            bail!("idempotency key {key} was already used for a different request");
        }
        Ok(Some(record.response))
    }

    fn store_cached_response(
        &self,
        key: &str,
        tool: &str,
        arguments: &Value,
        response: &Value,
    ) -> Result<()> {
        validate_idempotency_key(key)?;
        let path = self.idempotency_path(key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let record = IdempotencyRecord {
            tool: tool.to_owned(),
            arguments: arguments.clone(),
            response: response.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&record)?;
        let file_name = path
            .file_name()
            .context("idempotency path must end in a file name")?
            .to_string_lossy();
        let token = NEXT_IDEMPOTENCY_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let temporary = path.with_file_name(format!(
            ".{file_name}{TEMPORARY_FILE_MARKER}{}-{token}",
            std::process::id()
        ));
        if let Err(error) = fs::write(&temporary, bytes) {
            let _ = fs::remove_file(&temporary);
            return Err(error).with_context(|| {
                format!(
                    "could not write temporary idempotency record {}",
                    temporary.display()
                )
            });
        }
        if path.exists() {
            fs::remove_file(&temporary)?;
            let cached = self
                .cached_response(key, tool, arguments)?
                .context("idempotency record disappeared while writing")?;
            if cached != *response {
                bail!("idempotency key {key} completed with a different response");
            }
            return Ok(());
        }
        if let Err(error) = fs::rename(&temporary, &path) {
            let _ = fs::remove_file(&temporary);
            return Err(error).with_context(|| {
                format!("could not publish idempotency record {}", path.display())
            });
        }
        Ok(())
    }

    fn idempotency_path(&self, key: &str) -> PathBuf {
        self.harness
            .store()
            .root()
            .join("idempotency")
            .join(format!("{key}.json"))
    }
}

fn with_response_id(id: Value, response: Value) -> Value {
    let mut object = response.as_object().cloned().unwrap_or_default();
    object.insert("id".to_owned(), id);
    Value::Object(object)
}

fn error_response(error: &anyhow::Error) -> Value {
    let message = error.to_string();
    let code = classify_error(&message);
    let causes = error
        .chain()
        .skip(1)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    json!({
        "ok": false,
        "error": {"code": code, "message": message, "causes": causes}
    })
}

fn validate_idempotency_key(key: &str) -> Result<()> {
    if key.is_empty()
        || key.len() > 96
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("idempotency_key must contain only ASCII letters, digits, '-' or '_'");
    }
    Ok(())
}

fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).context("tool arguments do not match the schema")
}

fn classify_error(message: &str) -> &'static str {
    if message.contains("revision conflict") {
        "revision_conflict"
    } else if message.contains("already exists") {
        "already_exists"
    } else if message.contains("does not exist") || message.contains("could not read") {
        "not_found"
    } else if message.contains("budget") {
        "resource_budget_exceeded"
    } else if message.contains("invalid")
        || message.contains("outside")
        || message.contains("requires")
        || message.contains("must")
    {
        "validation_error"
    } else {
        "workflow_error"
    }
}

fn default_wait_ms() -> u64 {
    10_000
}

fn parse_arguments() -> Result<ServerOptions> {
    let mut arguments = env::args_os().skip(1);
    let mut root = PathBuf::from(".fractal-harness");
    let mut budget = ResourceBudget::default();
    while let Some(argument) = arguments.next() {
        if argument == "--root" {
            root = arguments
                .next()
                .context("--root requires a directory")?
                .into();
        } else if argument == "--max-concurrent-jobs" {
            budget.maximum_concurrent_jobs =
                parse_argument("--max-concurrent-jobs", arguments.next())?;
        } else if argument == "--max-gpu-duty-cycle" {
            budget.maximum_gpu_duty_cycle =
                parse_argument("--max-gpu-duty-cycle", arguments.next())?;
        } else if argument == "--max-preview-frames" {
            budget.maximum_preview_frames =
                parse_argument("--max-preview-frames", arguments.next())?;
        } else if argument == "--max-render-frames" {
            budget.maximum_render_frames = parse_argument("--max-render-frames", arguments.next())?;
        } else if argument == "--max-output-bytes" {
            budget.maximum_estimated_output_bytes =
                parse_argument("--max-output-bytes", arguments.next())?;
        } else if argument == "--max-wall-time-seconds" {
            budget.maximum_wall_time_seconds =
                parse_argument("--max-wall-time-seconds", arguments.next())?;
        } else if argument == "--help" || argument == "-h" {
            eprintln!(
                "Usage: fractal-harness [--root DIRECTORY] [RESOURCE LIMITS]\n\
                 \nResource limits:\n\
                 \x20 --max-concurrent-jobs N\n\
                 \x20 --max-gpu-duty-cycle PERCENT\n\
                 \x20 --max-preview-frames N\n\
                 \x20 --max-render-frames N\n\
                 \x20 --max-output-bytes N\n\
                 \x20 --max-wall-time-seconds N\n\
                 \nReads one JSON tool request per stdin line."
            );
            std::process::exit(0);
        } else {
            bail!("unknown argument {}", PathBuf::from(argument).display());
        }
    }
    budget
        .validate()
        .context("invalid harness resource limits")?;
    let root = if root.is_absolute() {
        root
    } else {
        env::current_dir()
            .context("could not resolve current directory")?
            .join(root)
    };
    Ok(ServerOptions { root, budget })
}

fn parse_argument<T>(name: &str, value: Option<std::ffi::OsString>) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .with_context(|| format!("{name} requires a value"))?
        .to_string_lossy()
        .parse()
        .map_err(|error| anyhow::anyhow!("{name} has an invalid value: {error}"))
}

fn capabilities(budget: Option<&ResourceBudget>) -> Value {
    let tools = [
        tool(
            "capabilities.describe",
            "List protocol and tool capabilities",
        ),
        tool(
            "project.create",
            "Import a source scene as an immutable project revision",
        ),
        tool("project.status", "Get current revision and project jobs"),
        tool("scene.get", "Read a structured SceneSpec revision"),
        tool(
            "scene.validate",
            "Validate project scene or YAML without changing state",
        ),
        tool(
            "scene.patch",
            "Apply transactional typed operations to a base revision",
        ),
        tool("scene.promote", "Make a validated revision current"),
        tool("scene.undo", "Promote the current revision's parent"),
        tool(
            "scene.describe_parameters",
            "List stable parameter paths, ranges, and change costs",
        ),
        tool(
            "route.inspect",
            "Sample camera motion and validate DE camera clearance",
        ),
        tool(
            "target.search",
            "Return deterministic DE surface target candidates",
        ),
        tool(
            "preview.start",
            "Start representative-frame preview and metric generation",
        ),
        tool(
            "preview.compare",
            "Compare completed preview jobs in a contact sheet",
        ),
        tool(
            "render.start",
            "Start or restart a revision-bound final frame sequence",
        ),
        tool(
            "encode.start",
            "Encode complete, ranged, or currently available frames",
        ),
        tool(
            "job.status",
            "Read persisted job progress and terminal result",
        ),
        tool("job.list", "List persisted project jobs"),
        tool(
            "job.pause",
            "Pause preview or render work at the next frame boundary",
        ),
        tool("job.resume", "Resume a paused job"),
        tool("job.cancel", "Cancel work at the next safe boundary"),
        tool("job.wait", "Wait up to 60 seconds for a terminal job state"),
        tool("artifact.list", "List artifacts produced by a job"),
    ];
    let mut result = json!({
        "protocol": "fractal-harness-jsonl",
        "protocol_version": PROTOCOL_VERSION,
        "scene_version": fractal_renderer_core::CURRENT_SCENE_VERSION,
        "transport": {
            "requests": "one JSON object per stdin line",
            "responses": "one JSON object per stdout line",
            "logs": "stderr only"
        },
        "tools": tools,
        "render_passes": ["beauty"],
        "job_statuses": [
            "queued", "running", "pause-requested", "paused", "cancel-requested",
            "completed", "failed", "cancelled", "interrupted"
        ]
    });
    if let Some(budget) = budget {
        result["resource_budget"] = serde_json::to_value(budget).expect("budget is serializable");
    }
    result
}

fn tool(name: &str, description: &str) -> Value {
    json!({
        "name": name,
        "description": description,
        "input_schema": input_schema(name),
    })
}

fn input_schema(name: &str) -> Value {
    let string = || json!({"type": "string"});
    let optional_revision = || json!({"type": "string"});
    match name {
        "capabilities.describe" | "scene.describe_parameters" => {
            json!({"type": "object", "additionalProperties": false})
        }
        "project.create" => json!({
            "type": "object",
            "required": ["project_id", "scene_path"],
            "properties": {"project_id": string(), "scene_path": string()},
            "additionalProperties": false
        }),
        "project.status" | "job.list" => json!({
            "type": "object", "required": ["project_id"],
            "properties": {"project_id": string()}, "additionalProperties": false
        }),
        "scene.get" => json!({
            "type": "object", "required": ["project_id"],
            "properties": {"project_id": string(), "revision_id": optional_revision()},
            "additionalProperties": false
        }),
        "scene.validate" => json!({
            "type": "object",
            "properties": {
                "project_id": string(), "revision_id": optional_revision(), "yaml": string()
            },
            "description": "Provide exactly one of project_id or yaml"
        }),
        "scene.patch" => json!({
            "type": "object", "required": ["project_id", "base_revision", "operations"],
            "properties": {
                "project_id": string(), "base_revision": string(),
                "operations": {
                    "type": "array", "minItems": 1,
                    "items": {
                        "type": "object", "required": ["op", "path"],
                        "properties": {
                            "op": {"enum": ["set", "remove", "insert"]},
                            "path": string(), "value": {},
                            "index": {"type": "integer", "minimum": 0}
                        }
                    }
                },
                "promote": {"type": "boolean"}
            }
        }),
        "scene.promote" => json!({
            "type": "object", "required": ["project_id", "revision_id"],
            "properties": {
                "project_id": string(), "revision_id": string(),
                "expected_current_revision": optional_revision()
            },
            "additionalProperties": false
        }),
        "scene.undo" => json!({
            "type": "object", "required": ["project_id"],
            "properties": {
                "project_id": string(), "expected_current_revision": optional_revision()
            }, "additionalProperties": false
        }),
        "route.inspect" => json!({
            "type": "object", "required": ["project_id"],
            "properties": {
                "project_id": string(), "revision_id": optional_revision(),
                "frames": {"type": "array", "items": {"type": "integer", "minimum": 0}},
                "validation_samples": {"type": "integer", "minimum": 2, "maximum": 10000},
                "minimum_clearance_ratio": {"type": "number", "minimum": 0}
            }, "additionalProperties": false
        }),
        "target.search" => json!({
            "type": "object", "required": ["project_id"],
            "properties": {
                "project_id": string(), "revision_id": optional_revision(),
                "candidate_count": {"type": "integer", "minimum": 1, "maximum": 64},
                "seed": {"type": "integer", "minimum": 0},
                "mode": {"enum": ["best", "random", "origin-gap"]},
                "approach_direction": {"type": "array", "minItems": 3, "maxItems": 3, "items": {"type": "number"}},
                "bound_radius": {"type": "number", "exclusiveMinimum": 0},
                "hit_epsilon": {"type": "number", "exclusiveMinimum": 0},
                "max_steps": {"type": "integer", "minimum": 1, "maximum": 10000},
                "attempts": {"type": "integer", "minimum": 1, "maximum": 4096},
                "aim_jitter": {"type": "number", "minimum": 0, "maximum": 1.5}
            }, "additionalProperties": false
        }),
        "preview.start" => json!({
            "type": "object", "required": ["project_id"],
            "properties": {
                "project_id": string(), "revision_id": optional_revision(),
                "profile": {"enum": ["composition", "lookdev", "proof", "final"]},
                "frames": {"type": "array", "items": {"type": "integer", "minimum": 0}},
                "width": {"type": "integer", "minimum": 1},
                "height": {"type": "integer", "minimum": 1},
                "region": {"type": "array", "minItems": 4, "maxItems": 4, "items": {"type": "number"}},
                "render_passes": {"type": "array", "items": {"enum": ["beauty"]}},
                "gpu_duty_cycle": {"type": "number", "minimum": 1, "maximum": 100},
                "allow_software": {"type": "boolean"}, "adapter": string()
            }, "additionalProperties": false
        }),
        "preview.compare" => json!({
            "type": "object", "required": ["project_id", "preview_job_ids"],
            "properties": {
                "project_id": string(),
                "preview_job_ids": {"type": "array", "minItems": 1, "items": string()}
            }, "additionalProperties": false
        }),
        "render.start" => json!({
            "type": "object", "required": ["project_id"],
            "properties": {
                "project_id": string(), "revision_id": optional_revision(),
                "start_frame": {"type": "integer", "minimum": 0},
                "end_frame": {"type": "integer", "minimum": 0},
                "resume": {"type": "boolean"}, "overwrite": {"type": "boolean"},
                "resume_source_job_id": string(),
                "gpu_duty_cycle": {"type": "number", "minimum": 1, "maximum": 100},
                "allow_software": {"type": "boolean"}, "adapter": string()
            }, "additionalProperties": false
        }),
        "encode.start" => json!({
            "type": "object", "required": ["project_id", "source_job_id"],
            "properties": {
                "project_id": string(), "source_job_id": string(),
                "selection": {
                    "type": "object", "required": ["kind"],
                    "properties": {
                        "kind": {"enum": ["complete", "range", "available"]},
                        "start_frame": {"type": "integer", "minimum": 0},
                        "end_frame": {"type": "integer", "minimum": 0}
                    }
                },
                "output_name": string(), "video": {"type": "object"}
            }, "additionalProperties": false
        }),
        "job.status" | "job.pause" | "job.resume" | "job.cancel" | "artifact.list" => json!({
            "type": "object", "required": ["project_id", "job_id"],
            "properties": {"project_id": string(), "job_id": string()},
            "additionalProperties": false
        }),
        "job.wait" => json!({
            "type": "object", "required": ["project_id", "job_id"],
            "properties": {
                "project_id": string(), "job_id": string(),
                "timeout_ms": {"type": "integer", "minimum": 1, "maximum": 60000}
            }, "additionalProperties": false
        }),
        _ => json!({"type": "object"}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    fn server() -> (Server, PathBuf) {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap();
        let root = env::temp_dir().join(format!(
            "fractal-harness-e2e-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        let server = Server {
            harness: Harness::new(ProjectStore::new(&root)).unwrap(),
            workspace_root,
        };
        (server, root)
    }

    #[test]
    fn alchemy_revision_and_route_workflow_is_gpu_free_and_idempotent() {
        let (server, root) = server();
        let create = ToolRequest {
            id: json!(1),
            tool: "project.create".to_owned(),
            idempotency_key: Some("create-alchemy".to_owned()),
            arguments: json!({
                "project_id": "alchemy",
                "scene_path": "scenes/examples/alchemy-pseudo-kleinian-target-orbit.yaml"
            }),
        };
        let first = server.handle(create);
        assert_eq!(first["ok"], true);
        let revision = first["result"]["current_revision"]
            .as_str()
            .unwrap()
            .to_owned();
        let repeated = server.handle(ToolRequest {
            id: json!(2),
            tool: "project.create".to_owned(),
            idempotency_key: Some("create-alchemy".to_owned()),
            arguments: json!({
                "project_id": "alchemy",
                "scene_path": "scenes/examples/alchemy-pseudo-kleinian-target-orbit.yaml"
            }),
        });
        assert_eq!(repeated["ok"], true);
        assert_eq!(repeated["result"], first["result"]);

        let patched = server.handle(ToolRequest {
            id: json!(3),
            tool: "scene.patch".to_owned(),
            idempotency_key: Some("alchemy-orbit-quarter".to_owned()),
            arguments: json!({
                "project_id": "alchemy",
                "base_revision": revision,
                "operations": [{
                    "op": "set",
                    "path": "animation.path.parameters.revolutions",
                    "value": 0.125
                }]
            }),
        });
        assert_eq!(patched["ok"], true);
        let candidate_revision = patched["result"]["id"].as_str().unwrap().to_owned();

        let inspection = server.handle(ToolRequest {
            id: json!(4),
            tool: "route.inspect".to_owned(),
            idempotency_key: None,
            arguments: json!({
                "project_id": "alchemy",
                "revision_id": candidate_revision,
                "validation_samples": 21,
                "minimum_clearance_ratio": 0.000001
            }),
        });
        assert_eq!(inspection["ok"], true);
        assert_eq!(inspection["result"]["valid"], true);
        assert_eq!(
            inspection["result"]["representative_samples"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        let promoted = server.handle(ToolRequest {
            id: json!(5),
            tool: "scene.promote".to_owned(),
            idempotency_key: Some("promote-alchemy-candidate".to_owned()),
            arguments: json!({
                "project_id": "alchemy",
                "revision_id": candidate_revision,
                "expected_current_revision": revision
            }),
        });
        assert_eq!(promoted["ok"], true);
        assert_eq!(
            promoted["result"]["current_revision"],
            patched["result"]["id"]
        );
        let undone = server.handle(ToolRequest {
            id: json!(6),
            tool: "scene.undo".to_owned(),
            idempotency_key: Some("undo-alchemy-candidate".to_owned()),
            arguments: json!({
                "project_id": "alchemy",
                "expected_current_revision": candidate_revision
            }),
        });
        assert_eq!(undone["ok"], true);
        assert_eq!(undone["result"]["current_revision"], revision);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn capabilities_expose_input_schemas() {
        let capabilities = capabilities(None);
        for tool in capabilities["tools"].as_array().unwrap() {
            assert_eq!(tool["input_schema"]["type"], "object");
        }
    }

    #[test]
    #[ignore = "requires a wgpu adapter and FFmpeg"]
    fn alchemy_preview_render_and_partial_encode_integration() {
        let (server, root) = server();
        let source = server
            .workspace_root
            .join("scenes/examples/alchemy-pseudo-kleinian-target-orbit.yaml");
        let project = server.harness.store().create("alchemy", source).unwrap();
        let candidate = server
            .harness
            .store()
            .patch(
                "alchemy",
                &project.current_revision,
                &[
                    PatchOperation::Set {
                        path: "name".to_owned(),
                        value: json!("alchemy-harness-integration"),
                    },
                    PatchOperation::Set {
                        path: "render.width".to_owned(),
                        value: json!(64),
                    },
                    PatchOperation::Set {
                        path: "render.height".to_owned(),
                        value: json!(44),
                    },
                    PatchOperation::Set {
                        path: "quality.samples_per_pixel".to_owned(),
                        value: json!(1),
                    },
                    PatchOperation::Set {
                        path: "camera.aperture_radius".to_owned(),
                        value: json!(0.0),
                    },
                    PatchOperation::Set {
                        path: "quality.ambient_occlusion.max_steps".to_owned(),
                        value: json!(0),
                    },
                    PatchOperation::Set {
                        path: "quality.ambient_occlusion.strength".to_owned(),
                        value: json!(0.0),
                    },
                    PatchOperation::Set {
                        path: "quality.soft_shadow.max_steps".to_owned(),
                        value: json!(0),
                    },
                    PatchOperation::Set {
                        path: "quality.reflection.max_steps".to_owned(),
                        value: json!(0),
                    },
                    PatchOperation::Set {
                        path: "quality.reflection.strength".to_owned(),
                        value: json!(0.0),
                    },
                ],
                false,
            )
            .unwrap();

        let preview = server
            .harness
            .start_preview(PreviewRequest {
                project_id: "alchemy".to_owned(),
                revision_id: Some(candidate.id.clone()),
                profile: fractal_renderer_workflow::PreviewProfile::Composition,
                frames: vec![0],
                width: Some(64),
                height: None,
                region: None,
                render_passes: Vec::new(),
                gpu_duty_cycle: Some(100.0),
                allow_software: true,
                adapter: None,
            })
            .unwrap();
        let preview = server
            .harness
            .wait_for_job("alchemy", &preview.id, Duration::from_secs(60))
            .unwrap();
        assert_eq!(
            preview.status,
            fractal_renderer_workflow::JobStatus::Completed
        );

        let render = server
            .harness
            .start_render(
                RenderRequest {
                    project_id: "alchemy".to_owned(),
                    revision_id: Some(candidate.id),
                    start_frame: Some(0),
                    end_frame: Some(0),
                    resume: false,
                    overwrite: false,
                    gpu_duty_cycle: Some(100.0),
                    allow_software: true,
                    adapter: None,
                },
                None,
            )
            .unwrap();
        let render = server
            .harness
            .wait_for_job("alchemy", &render.id, Duration::from_secs(60))
            .unwrap();
        assert_eq!(
            render.status,
            fractal_renderer_workflow::JobStatus::Completed
        );

        let encode = server
            .harness
            .start_encode(EncodeRequest {
                project_id: "alchemy".to_owned(),
                source_job_id: render.id,
                selection: fractal_renderer_workflow::SequenceSelection::Available {
                    start_frame: 0,
                },
                output_name: Some("alchemy-integration.mp4".to_owned()),
                video: fractal_renderer_workflow::VideoOverrides::default(),
                ffmpeg: None,
            })
            .unwrap();
        let encode = server
            .harness
            .wait_for_job("alchemy", &encode.id, Duration::from_secs(60))
            .unwrap();
        assert_eq!(
            encode.status,
            fractal_renderer_workflow::JobStatus::Completed
        );
        assert_eq!(encode.artifacts.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }
}
