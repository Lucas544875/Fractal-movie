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

#[test]
fn brief_check_is_refused_before_locations_are_selected() {
    let sandbox = Sandbox::new("brief");
    sandbox.run(&["init", "film"]);
    let (ok, value) = sandbox.run(&["brief", "check", "film"]);
    assert!(!ok);
    assert!(value["error"].as_str().unwrap().contains("stage scout"));
}

#[test]
fn ledger_list_reports_films_and_their_axes() {
    let sandbox = Sandbox::new("ledger");
    let (ok, empty) = sandbox.run(&["ledger", "list"]);
    assert!(ok);
    assert_eq!(empty["films"], serde_json::json!([]));

    write_ledger(&sandbox.ledger());
    let (_, value) = sandbox.run(&["ledger", "list"]);
    assert_eq!(value["films"].as_array().unwrap().len(), 2);
    assert_eq!(value["films"][0]["id"], "ivory-vault");
    assert_eq!(value["films"][0]["axes"]["formula_family"], "amazing-surf");
    assert_eq!(value["films"][0]["gpu_minutes"], 99.0);

    // A film without an animation cannot be imported, and nothing is written.
    let before = fs::read_to_string(sandbox.ledger()).unwrap();
    let scene = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../scenes/examples/mandelbulb.yaml"
    );
    let (ok, value) = sandbox.run(&[
        "ledger",
        "import",
        scene,
        "--id",
        "still",
        "--title",
        "Still",
        "--date",
        "2026-11-15",
    ]);
    assert!(!ok);
    assert!(value["error"].as_str().unwrap().contains("no animation"));
    assert_eq!(fs::read_to_string(sandbox.ledger()).unwrap(), before);
}

#[test]
fn later_stage_commands_are_refused_while_scouting() {
    let sandbox = Sandbox::new("stages");
    sandbox.run(&[
        "init",
        "film",
        "--film-seconds",
        "10",
        "30",
        "--film-cuts",
        "2",
        "4",
    ]);

    for arguments in [
        vec!["shotlist", "check", "film"],
        vec!["animatic", "film"],
        vec!["look", "preview", "film"],
        vec!["proof", "film"],
        vec!["produce", "film"],
        vec!["encode", "film"],
        vec!["reopen", "film", "--stage", "shotlist", "--reason", "recut"],
    ] {
        let (ok, value) = sandbox.run(&arguments);
        assert!(!ok, "{arguments:?} was accepted");
        let error = value["error"].as_str().unwrap();
        assert!(error.contains("scout"), "{arguments:?}: {error}");
    }

    // Nothing was recorded and the custom film limits were stored.
    assert_eq!(
        fs::read_dir(sandbox.root().join("film/reviews"))
            .unwrap()
            .count(),
        0
    );
    let budget: Value =
        serde_json::from_str(&fs::read_to_string(sandbox.root().join("film/budget.json")).unwrap())
            .unwrap();
    assert_eq!(budget["film"]["max_duration_seconds"], 30.0);
    assert_eq!(budget["film"]["min_cuts"], 2);

    let (ok, value) = sandbox.run(&["init", "bad", "--film-seconds", "30", "10"]);
    assert!(!ok);
    assert!(value["error"].as_str().unwrap().contains("duration limits"));
}

#[test]
fn a_production_keeps_the_tuning_it_was_created_with() {
    let sandbox = Sandbox::new("tuning");
    let file = sandbox.directory.join("tuning.yaml");
    let file_argument = file.to_str().unwrap();

    // Exporting writes the defaults and never overwrites.
    let (ok, value) = sandbox.run(&["tuning", "write", file_argument]);
    assert!(ok, "{value}");
    let exported = fs::read_to_string(&file).unwrap();
    assert!(
        exported.contains("overview_distance_radii: 2.6"),
        "{exported}"
    );
    let (ok, value) = sandbox.run(&["tuning", "write", file_argument]);
    assert!(!ok);
    assert!(value["error"].as_str().unwrap().contains("already exists"));

    fs::write(
        &file,
        "scouting:\n  min_selected: 3\n  max_selected: 4\nlimits:\n  kills: 1\n",
    )
    .unwrap();
    let (ok, shown) = sandbox.run(&["--tuning", file_argument, "tuning", "show"]);
    assert!(ok);
    assert_eq!(shown["tuning"]["scouting"]["min_selected"], 3);
    assert_eq!(shown["tuning"]["scouting"]["default_shapes"], 60);

    let (ok, value) = sandbox.run(&["--tuning", file_argument, "init", "film"]);
    assert!(ok, "{value}");
    assert_eq!(value["limits"]["kills_allowed"], 1);

    // Later commands use the snapshot, whatever the file says by then.
    fs::write(&file, "scouting:\n  min_selected: 2\n").unwrap();
    let (ok, value) = sandbox.run(&[
        "--tuning",
        file_argument,
        "scout",
        "select",
        "film",
        "loc-0001",
        "loc-0002",
    ]);
    assert!(!ok);
    assert!(
        value["error"]
            .as_str()
            .unwrap()
            .contains("select 3 to 4 locations"),
        "{value}"
    );
    let (_, next) = sandbox.run(&["next", "film"]);
    assert_eq!(next["limits"]["kills_allowed"], 1);

    // Misspelt or unusable tuning stops `init` before anything is created.
    fs::write(&file, "scouting:\n  min_select: 3\n").unwrap();
    let (ok, value) = sandbox.run(&["--tuning", file_argument, "init", "typo"]);
    assert!(!ok);
    assert!(
        value["error"].as_str().unwrap().contains("tuning"),
        "{value}"
    );
    assert!(!sandbox.root().join("typo").exists());
    fs::write(&file, "quality:\n  spp_ladder: [8, 4]\n").unwrap();
    let (ok, _) = sandbox.run(&["--tuning", file_argument, "init", "unusable"]);
    assert!(!ok);
    assert!(!sandbox.root().join("unusable").exists());
}

#[test]
fn only_the_critic_role_may_submit_a_verdict() {
    let sandbox = Sandbox::new("critic");
    sandbox.run(&["init", "film"]);
    let arguments = [
        "review",
        "submit",
        "film",
        "--verdict",
        "pass",
        "--notes",
        "fine",
    ];

    let (ok, value) = sandbox.run(&arguments);
    assert!(!ok);
    assert!(
        value["error"].as_str().unwrap().contains("film-critic.sh"),
        "{value}"
    );

    // With the role, the request reaches the stage machine, which refuses
    // it because scouting has no review.
    let output = Command::new(env!("CARGO_BIN_EXE_fractal-harness"))
        .env("FRACTAL_HARNESS_ROLE", "critic")
        .arg("--root")
        .arg(sandbox.root())
        .arg("--ledger")
        .arg(sandbox.ledger())
        .args(arguments)
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        value["error"]
            .as_str()
            .unwrap()
            .contains("no critic review"),
        "{value}"
    );
}

#[test]
fn vocabulary_requests_are_recorded_listed_and_resolved() {
    let sandbox = Sandbox::new("vocabulary");
    sandbox.run(&["init", "film"]);

    let (ok, value) = sandbox.run(&[
        "request",
        "film",
        "--kind",
        "motion",
        "--beat",
        "rise above the towers",
        "--need",
        "a vertical crane move",
    ]);
    assert!(ok, "{value}");
    assert_eq!(value["request"], 1);
    sandbox.run(&[
        "request",
        "film",
        "--kind",
        "material",
        "--beat",
        "the walls glow",
        "--need",
        "emissive surfaces",
    ]);

    let (_, listed) = sandbox.run(&["vocabulary", "list"]);
    let requests = listed["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["status"], "open");
    assert_eq!(requests[0]["production"], "film");
    assert_eq!(requests[1]["kind"], "material");

    let (ok, _) = sandbox.run(&[
        "vocabulary",
        "resolve",
        "1",
        "--status",
        "implemented",
        "--note",
        "motion crane: { kind: crane, rise: 0.5 }",
    ]);
    assert!(ok);
    // A decision is recorded once.
    let (ok, value) = sandbox.run(&[
        "vocabulary",
        "resolve",
        "1",
        "--status",
        "declined",
        "--note",
        "changed my mind",
    ]);
    assert!(!ok);
    assert!(
        value["error"]
            .as_str()
            .unwrap()
            .contains("already resolved")
    );

    let (_, open) = sandbox.run(&["vocabulary", "list", "--open"]);
    let open = open["requests"].as_array().unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0]["id"], 2);
    let (_, all) = sandbox.run(&["vocabulary", "list"]);
    assert_eq!(all["requests"][0]["status"], "implemented");
    assert!(
        all["requests"][0]["note"]
            .as_str()
            .unwrap()
            .contains("crane")
    );

    let (ok, value) = sandbox.run(&[
        "vocabulary",
        "sample",
        "--family",
        "no-such-family",
        "--out",
        "/nonexistent",
    ]);
    assert!(!ok);
    assert!(
        value["error"]
            .as_str()
            .unwrap()
            .contains("unknown formula family")
    );
}
