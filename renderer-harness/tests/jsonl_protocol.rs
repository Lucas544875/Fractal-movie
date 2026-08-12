use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::Duration,
};

use serde_json::{Value, json};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct HarnessProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    stderr: Option<ChildStderr>,
}

impl HarnessProcess {
    fn start(root: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_fractal-harness"))
            .current_dir(workspace_root())
            .args([
                "--root",
                root.to_str().unwrap(),
                "--max-concurrent-jobs",
                "2",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("harness subprocess must start");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let stderr = child.stderr.take().unwrap();
        Self {
            child,
            stdin: Some(stdin),
            stdout,
            stderr: Some(stderr),
        }
    }

    fn request(&mut self, request: Value) -> Value {
        self.raw_request(&serde_json::to_string(&request).unwrap())
    }

    fn raw_request(&mut self, request: &str) -> Value {
        let stdin = self.stdin.as_mut().expect("harness stdin is open");
        writeln!(stdin, "{request}").unwrap();
        stdin.flush().unwrap();
        let mut response = String::new();
        self.stdout.read_line(&mut response).unwrap();
        assert!(!response.is_empty(), "harness ended without a response");
        serde_json::from_str(response.trim_end())
            .unwrap_or_else(|error| panic!("stdout was not one JSON response: {error}: {response}"))
    }

    fn finish(mut self) -> String {
        drop(self.stdin.take());
        let status = self.child.wait().unwrap();
        assert!(status.success(), "harness exited with {status}");
        let mut remaining_stdout = String::new();
        self.stdout.read_to_string(&mut remaining_stdout).unwrap();
        assert!(
            remaining_stdout.trim().is_empty(),
            "harness emitted an unsolicited stdout line: {remaining_stdout}"
        );
        let mut stderr = String::new();
        self.stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        stderr
    }
}

impl Drop for HarnessProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap()
}

fn temporary_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "fractal-jsonl-{label}-{}-{}",
        std::process::id(),
        NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn assert_ok(response: &Value) -> &Value {
    assert_eq!(response["ok"], true, "request failed: {response}");
    &response["result"]
}

fn is_transient_gpu_unavailable(job: &Value) -> bool {
    assert_ok(job)["error"]["code"] == "gpu_unavailable"
}

#[test]
fn jsonl_subprocess_emits_exactly_one_response_and_recovers_idempotency() {
    let root = temporary_root("contract");
    let create_arguments = json!({
        "project_id": "alchemy",
        "scene_path": "scenes/examples/alchemy-pseudo-kleinian-target-orbit.yaml"
    });
    let mut first = HarnessProcess::start(&root);
    let capabilities = first.request(json!({
        "id": "capabilities",
        "tool": "capabilities.describe",
        "arguments": {}
    }));
    assert_eq!(capabilities["id"], "capabilities");
    assert_eq!(
        assert_ok(&capabilities)["protocol"],
        "fractal-harness-jsonl"
    );

    let malformed = first.raw_request("{not-json");
    assert_eq!(malformed["id"], Value::Null);
    assert_eq!(malformed["ok"], false);
    assert_eq!(malformed["error"]["code"], "invalid_request");

    let created = first.request(json!({
        "id": 1,
        "idempotency_key": "subprocess-create-alchemy",
        "tool": "project.create",
        "arguments": create_arguments
    }));
    let initial_revision = assert_ok(&created)["current_revision"].clone();
    let repeated = first.request(json!({
        "id": 2,
        "idempotency_key": "subprocess-create-alchemy",
        "tool": "project.create",
        "arguments": create_arguments
    }));
    assert_eq!(assert_ok(&repeated), assert_ok(&created));
    let _stderr = first.finish();

    let mut restarted = HarnessProcess::start(&root);
    let recovered = restarted.request(json!({
        "id": 3,
        "idempotency_key": "subprocess-create-alchemy",
        "tool": "project.create",
        "arguments": create_arguments
    }));
    assert_eq!(assert_ok(&recovered)["current_revision"], initial_revision);
    let status = restarted.request(json!({
        "id": 4,
        "tool": "project.status",
        "arguments": {"project_id": "alchemy"}
    }));
    assert_eq!(
        assert_ok(&status)["project"]["current_revision"],
        initial_revision
    );
    let _stderr = restarted.finish();
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires a wgpu adapter and FFmpeg"]
fn alchemy_autonomous_design_render_encode_over_jsonl() {
    let root = temporary_root("alchemy-e2e");
    let mut harness = HarnessProcess::start(&root);
    let created = harness.request(json!({
        "id": 1,
        "idempotency_key": "e2e-create",
        "tool": "project.create",
        "arguments": {
            "project_id": "alchemy",
            "scene_path": "scenes/examples/alchemy-pseudo-kleinian-target-orbit.yaml"
        }
    }));
    let base = assert_ok(&created)["current_revision"].as_str().unwrap();
    let patched = harness.request(json!({
        "id": 2,
        "idempotency_key": "e2e-candidate",
        "tool": "scene.patch",
        "arguments": {
            "project_id": "alchemy",
            "base_revision": base,
            "operations": [
                {"op":"set", "path":"name", "value":"alchemy-autonomous-e2e"},
                {"op":"set", "path":"render.width", "value":64},
                {"op":"set", "path":"render.height", "value":44},
                {"op":"set", "path":"quality.samples_per_pixel", "value":1},
                {"op":"set", "path":"camera.aperture_radius", "value":0.0},
                {"op":"set", "path":"quality.ambient_occlusion.max_steps", "value":0},
                {"op":"set", "path":"quality.ambient_occlusion.strength", "value":0.0},
                {"op":"set", "path":"quality.soft_shadow.max_steps", "value":0},
                {"op":"set", "path":"animation.fps", "value":2},
                {"op":"set", "path":"animation.frame_count", "value":5},
                {"op":"set", "path":"animation.path.parameters.duration", "value":2.0},
                {"op":"set", "path":"video.preset", "value":"ultrafast"}
            ]
        }
    }));
    let candidate = assert_ok(&patched)["id"].as_str().unwrap();

    let route = harness.request(json!({
        "id": 3,
        "tool": "route.inspect",
        "arguments": {
            "project_id":"alchemy", "revision_id":candidate,
            "validation_samples":21, "minimum_clearance_ratio":0.000001
        }
    }));
    assert_eq!(assert_ok(&route)["valid"], true);

    // Some Linux driver stacks briefly expose only llvmpipe immediately after
    // another wgpu process exits. Keep hardware-only policy, but exercise the
    // agent's structured-error retry path before declaring the host unusable.
    let mut preview_attempt = 0;
    let (preview_job, preview_done) = loop {
        preview_attempt += 1;
        let attempt = preview_attempt;
        let preview = harness.request(json!({
            "id": format!("preview-{attempt}"),
            "idempotency_key": format!("e2e-preview-{attempt}"),
            "tool": "preview.start",
            "arguments": {
                "project_id":"alchemy", "revision_id":candidate,
                "profile":"composition", "width":64, "allow_software":false
            }
        }));
        let preview_job = assert_ok(&preview)["id"].as_str().unwrap().to_owned();
        let preview_done = harness.request(json!({
            "id": format!("wait-preview-{attempt}"), "tool":"job.wait",
            "arguments":{"project_id":"alchemy", "job_id":preview_job, "timeout_ms":60000}
        }));
        if assert_ok(&preview_done)["status"] == "completed" {
            break (preview_job, preview_done);
        }
        assert!(
            is_transient_gpu_unavailable(&preview_done) && attempt < 3,
            "preview failed without a retryable GPU error: {preview_done}"
        );
        thread::sleep(Duration::from_millis(250));
    };
    assert_eq!(assert_ok(&preview_done)["status"], "completed");
    assert_eq!(
        assert_ok(&preview_done)["result"]["frames"]
            .as_array()
            .unwrap()
            .len(),
        5
    );

    let compared = harness.request(json!({
        "id": 6, "tool":"preview.compare",
        "arguments":{"project_id":"alchemy", "preview_job_ids":[preview_job]}
    }));
    let compare_job = assert_ok(&compared)["id"].as_str().unwrap();
    let compare_done = harness.request(json!({
        "id": 7, "tool":"job.wait",
        "arguments":{"project_id":"alchemy", "job_id":compare_job, "timeout_ms":60000}
    }));
    assert_eq!(assert_ok(&compare_done)["status"], "completed");

    let promoted = harness.request(json!({
        "id": 8,
        "idempotency_key": "e2e-promote",
        "tool":"scene.promote",
        "arguments": {
            "project_id":"alchemy", "revision_id":candidate,
            "expected_current_revision":base
        }
    }));
    assert_eq!(assert_ok(&promoted)["current_revision"], candidate);

    let mut render_attempt = 0;
    let (render_job, render_done) = loop {
        render_attempt += 1;
        let attempt = render_attempt;
        let render = harness.request(json!({
            "id": format!("render-{attempt}"),
            "idempotency_key": format!("e2e-render-{attempt}"),
            "tool":"render.start",
            "arguments": {
                "project_id":"alchemy", "revision_id":candidate,
                "gpu_duty_cycle":80, "allow_software":false
            }
        }));
        let render_job = assert_ok(&render)["id"].as_str().unwrap().to_owned();
        let render_done = harness.request(json!({
            "id": format!("wait-render-{attempt}"), "tool":"job.wait",
            "arguments":{"project_id":"alchemy", "job_id":render_job, "timeout_ms":60000}
        }));
        if assert_ok(&render_done)["status"] == "completed" {
            break (render_job, render_done);
        }
        assert!(
            is_transient_gpu_unavailable(&render_done) && attempt < 3,
            "render failed without a retryable GPU error: {render_done}"
        );
        thread::sleep(Duration::from_millis(250));
    };
    assert_eq!(assert_ok(&render_done)["status"], "completed");
    assert_eq!(
        assert_ok(&render_done)["result"]["rendered_frames"]
            .as_array()
            .unwrap()
            .len(),
        5
    );

    let encode = harness.request(json!({
        "id": 11,
        "idempotency_key": "e2e-encode",
        "tool":"encode.start",
        "arguments": {
            "project_id":"alchemy", "source_job_id":render_job,
            "selection":{"kind":"complete"},
            "output_name":"alchemy-autonomous-e2e.mp4"
        }
    }));
    let encode_job = assert_ok(&encode)["id"].as_str().unwrap();
    let encode_done = harness.request(json!({
        "id": 12, "tool":"job.wait",
        "arguments":{"project_id":"alchemy", "job_id":encode_job, "timeout_ms":60000}
    }));
    assert_eq!(assert_ok(&encode_done)["status"], "completed");
    let artifacts = harness.request(json!({
        "id":13, "tool":"artifact.list",
        "arguments":{"project_id":"alchemy", "job_id":encode_job}
    }));
    let artifacts = assert_ok(&artifacts).as_array().unwrap();
    assert_eq!(artifacts.len(), 1);
    assert!(Path::new(artifacts[0]["path"].as_str().unwrap()).is_file());
    let _stderr = harness.finish();
    fs::remove_dir_all(root).unwrap();
}
