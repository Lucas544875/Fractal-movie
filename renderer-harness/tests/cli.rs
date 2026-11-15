//! Drives the real executable, because the contract agents rely on is the
//! process boundary: one JSON document on stdout and a meaningful exit code.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

struct Sandbox {
    directory: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let directory =
            std::env::temp_dir().join(format!("fractal-harness-cli-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        Self { directory }
    }

    fn root(&self) -> PathBuf {
        self.directory.join("productions")
    }

    fn ledger(&self) -> PathBuf {
        self.directory.join("ledger.jsonl")
    }

    /// Runs the harness and returns its exit status and parsed stdout.
    fn run(&self, arguments: &[&str]) -> (bool, Value) {
        let output = Command::new(env!("CARGO_BIN_EXE_fractal-harness"))
            .arg("--root")
            .arg(self.root())
            .arg("--ledger")
            .arg(self.ledger())
            .args(arguments)
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            stdout.lines().count(),
            1,
            "stdout must be exactly one JSON line, got: {stdout:?}"
        );
        (
            output.status.success(),
            serde_json::from_str(&stdout).unwrap(),
        )
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn write_ledger(path: &Path) {
    let film = |id: &str, family: &str, motion: &str| {
        format!(
            r#"{{"id":"{id}","date":"2026-09-20","title":"{id}","axes":{{"formula_family":"{family}","scale_regime":"mid","motion":"{motion}","palette":{{"primary_hue":"teal","key":"low"}},"light":"raking","pacing":"slow"}},"duration_seconds":10.0,"gpu_minutes":99.0}}"#
        )
    };
    fs::write(
        path,
        format!(
            "{}\n{}\n",
            film("ivory-vault", "amazing-surf", "orbit"),
            film("azure-cloister", "amazing-surf", "orbit")
        ),
    )
    .unwrap();
}

#[test]
fn describe_reports_the_vocabulary() {
    let sandbox = Sandbox::new("describe");
    let (ok, value) = sandbox.run(&["describe"]);
    assert!(ok);
    assert_eq!(value["ok"], true);
    assert_eq!(value["vocabulary"]["axes"].as_array().unwrap().len(), 6);
    assert_eq!(
        value["vocabulary"]["authored_files"],
        serde_json::json!(["brief.yaml", "shotlist.yaml", "look.yaml"])
    );
}

#[test]
fn init_creates_a_production_constrained_by_the_ledger() {
    let sandbox = Sandbox::new("init");
    write_ledger(&sandbox.ledger());

    let (ok, value) = sandbox.run(&["init", "film-01", "--seed", "7", "--duration", "45"]);
    assert!(ok, "{value}");
    assert_eq!(value["stage"], "scout");
    assert_eq!(value["stage_code"], "S1");
    assert_eq!(
        value["diversity"]["mandate"]["banned_families"],
        serde_json::json!(["amazing-surf"])
    );
    assert_eq!(
        value["diversity"]["mandate"]["assigned"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        value["diversity"]["recent_films"][0]["id"],
        "azure-cloister"
    );
    // 45 s of film at 2 GPU minutes per second.
    assert_eq!(value["budget"]["production_limit_gpu_seconds"], 5400.0);
    assert!(sandbox.root().join("film-01/state.json").is_file());

    // The same seed must reproduce the same constraints.
    let (_, again) = sandbox.run(&["init", "film-02", "--seed", "7"]);
    assert_eq!(again["diversity"]["mandate"], value["diversity"]["mandate"]);

    // `next` is a read-only view of the same state.
    let (ok, next) = sandbox.run(&["next", "film-01"]);
    assert!(ok);
    assert_eq!(next["gates"], value["gates"]);
    assert_eq!(next["gates"][0]["status"], "pending");
}

#[test]
fn failures_are_reported_as_json_with_a_failing_status() {
    let sandbox = Sandbox::new("failures");

    let (ok, value) = sandbox.run(&["next", "missing"]);
    assert!(!ok);
    assert_eq!(value["ok"], false);
    assert!(value["error"].as_str().unwrap().contains("missing"));

    let (ok, value) = sandbox.run(&["init", "../escape"]);
    assert!(!ok);
    assert!(value["error"].as_str().unwrap().contains("production id"));
    assert!(!sandbox.directory.join("escape").exists());

    let (ok, _) = sandbox.run(&["init", "film"]);
    assert!(ok);
    let (ok, value) = sandbox.run(&["init", "film"]);
    assert!(!ok, "{value}");

    let (ok, value) = sandbox.run(&["init", "bad-budget", "--max-gpu-duty-cycle", "150"]);
    assert!(!ok);
    assert!(
        value["error"]
            .as_str()
            .unwrap()
            .contains("max_gpu_duty_cycle")
    );
    assert!(!sandbox.root().join("bad-budget").exists());
}

#[test]
fn kill_is_refused_before_a_concept_exists() {
    let sandbox = Sandbox::new("kill");
    sandbox.run(&["init", "film"]);
    let (ok, value) = sandbox.run(&["kill", "film", "--reason", "too early"]);
    assert!(!ok);
    assert!(value["error"].as_str().unwrap().contains("scout"));

    let (_, next) = sandbox.run(&["next", "film"]);
    assert_eq!(next["stage"], "scout");
    assert_eq!(next["limits"]["kills_used"], 0);
}

#[test]
fn corrupt_state_is_rejected_instead_of_guessed() {
    let sandbox = Sandbox::new("corrupt");
    sandbox.run(&["init", "film"]);
    fs::write(sandbox.root().join("film/state.json"), "{ not json").unwrap();
    let (ok, value) = sandbox.run(&["next", "film"]);
    assert!(!ok);
    assert!(value["error"].as_str().unwrap().contains("state.json"));
}

#[test]
fn scouting_rejects_bad_requests_before_touching_the_gpu() {
    let sandbox = Sandbox::new("scout");
    sandbox.run(&["init", "film"]);

    let (ok, value) = sandbox.run(&["scout", "run", "film", "--shapes", "0"]);
    assert!(!ok);
    assert!(
        value["error"]
            .as_str()
            .unwrap()
            .contains("at least one shape")
    );

    let (ok, value) = sandbox.run(&["scout", "run", "film", "--gpu-duty-cycle", "95"]);
    assert!(!ok);
    assert!(value["error"].as_str().unwrap().contains("duty cycle"));

    let (ok, value) = sandbox.run(&["scout", "run", "film", "--around", "loc-0042"]);
    assert!(!ok);
    assert!(
        value["error"]
            .as_str()
            .unwrap()
            .contains("unknown location")
    );

    let (ok, value) = sandbox.run(&["scout", "select", "film", "loc-0001", "loc-0002"]);
    assert!(!ok);
    assert!(
        value["error"]
            .as_str()
            .unwrap()
            .contains("unknown location")
    );

    // Nothing above may have advanced or altered the production.
    let (_, next) = sandbox.run(&["next", "film"]);
    assert_eq!(next["stage"], "scout");
    assert!(
        next["gates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|gate| gate["status"] == "pending")
    );
    assert_eq!(next["budget"]["design_remaining_gpu_seconds"], 1800.0);
}
