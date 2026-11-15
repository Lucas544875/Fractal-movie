//! On-disk layout of one production. Every command is a short-lived process,
//! so all state lives in this directory.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Serialize, de::DeserializeOwned};

use crate::{budget::Budget, ledger::Ledger, mandate::Mandate, stage::State};

pub const DEFAULT_ROOT: &str = "output/productions";
pub const DEFAULT_LEDGER: &str = "portfolio/ledger.jsonl";

const MAX_ID_LENGTH: usize = 64;

/// Files the director authors. Everything else is written by the harness.
pub const AUTHORED_FILES: [&str; 3] = ["brief.yaml", "shotlist.yaml", "look.yaml"];

const SUBDIRECTORIES: [&str; 8] = [
    "scout", "compiled", "animatic", "proof", "reviews", "frames", "jobs", "final",
];

#[derive(Clone, Debug)]
pub struct Production {
    id: String,
    directory: PathBuf,
}

impl Production {
    /// Creates the directory of a new production and draws its constraints.
    pub fn create(
        root: &Path,
        id: &str,
        ledger: &Ledger,
        seed: u64,
        budget: Budget,
    ) -> Result<Self> {
        validate_id(id)?;
        budget.validate()?;
        if ledger.entries().iter().any(|entry| entry.id == id) {
            bail!("production id {id:?} is already used by a finished film in the ledger");
        }

        fs::create_dir_all(root).with_context(|| format!("could not create {}", root.display()))?;
        let directory = root.join(id);
        // `create_dir` fails on an existing directory, so two concurrent
        // `init` calls cannot both claim the id.
        fs::create_dir(&directory).with_context(|| {
            format!(
                "could not create production directory {}",
                directory.display()
            )
        })?;
        for name in SUBDIRECTORIES {
            fs::create_dir(directory.join(name))
                .with_context(|| format!("could not create {name} directory"))?;
        }

        let production = Self {
            id: id.to_owned(),
            directory,
        };
        production.save_mandate(&Mandate::draw(ledger, seed))?;
        production.save_budget(&budget)?;
        production.save_state(&State::new(id))?;
        Ok(production)
    }

    /// Opens an existing production.
    pub fn open(root: &Path, id: &str) -> Result<Self> {
        validate_id(id)?;
        let directory = root.join(id);
        if !directory.join("state.json").is_file() {
            bail!(
                "no production {id:?} under {}; create it with `init`",
                root.display()
            );
        }
        Ok(Self {
            id: id.to_owned(),
            directory,
        })
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    #[must_use]
    pub fn path(&self, relative: &str) -> PathBuf {
        self.directory.join(relative)
    }

    pub fn load_state(&self) -> Result<State> {
        let state: State = read_json(&self.path("state.json"))?;
        state.validate()?;
        if state.id != self.id {
            bail!(
                "state.json belongs to production {:?}, not {:?}",
                state.id,
                self.id
            );
        }
        Ok(state)
    }

    pub fn save_state(&self, state: &State) -> Result<()> {
        state.validate()?;
        write_json(&self.path("state.json"), state)
    }

    pub fn load_mandate(&self) -> Result<Mandate> {
        read_json(&self.path("mandate.json"))
    }

    fn save_mandate(&self, mandate: &Mandate) -> Result<()> {
        write_json(&self.path("mandate.json"), mandate)
    }

    pub fn load_budget(&self) -> Result<Budget> {
        let budget: Budget = read_json(&self.path("budget.json"))?;
        budget.validate()?;
        Ok(budget)
    }

    pub fn save_budget(&self, budget: &Budget) -> Result<()> {
        budget.validate()?;
        write_json(&self.path("budget.json"), budget)
    }
}

/// Ids become directory names and ledger keys, so they are restricted to a
/// portable subset that cannot escape the production root.
pub fn validate_id(id: &str) -> Result<()> {
    let well_formed = !id.is_empty()
        && id.len() <= MAX_ID_LENGTH
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !id.starts_with('-')
        && !id.ends_with('-');
    if !well_formed {
        bail!(
            "production id {id:?} must be 1..={MAX_ID_LENGTH} characters of a-z, 0-9, and inner hyphens"
        );
    }
    Ok(())
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let text =
        fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("invalid {}", path.display()))
}

/// Writes through a sibling temporary file so an interrupted command never
/// leaves a truncated state file behind.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut text = serde_json::to_string_pretty(value)
        .with_context(|| format!("could not serialize {}", path.display()))?;
    text.push('\n');
    let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
    fs::write(&temporary, text)
        .with_context(|| format!("could not write {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("could not replace {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        axes::{FormulaFamily, Motion},
        ledger::tests::entry,
        stage::Stage,
    };

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "fractal-harness-production-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn ids_are_restricted_to_a_safe_subset() {
        for valid in ["a", "film-01", "0-9"] {
            validate_id(valid).unwrap();
        }
        let too_long = "a".repeat(MAX_ID_LENGTH + 1);
        for invalid in [
            "", "Film", "a_b", "../x", "a/b", "-a", "a-", "a b", ".", &too_long,
        ] {
            assert!(validate_id(invalid).is_err(), "{invalid:?} was accepted");
        }
    }

    #[test]
    fn create_writes_layout_and_initial_state() {
        let root = temp_root("create");
        let ledger = Ledger::from_entries(vec![entry(
            "earlier",
            FormulaFamily::Mandelbox,
            Motion::Dive,
        )])
        .unwrap();
        let production =
            Production::create(&root, "film-01", &ledger, 5, Budget::default()).unwrap();

        for name in SUBDIRECTORIES {
            assert!(production.path(name).is_dir(), "{name} is missing");
        }
        let state = production.load_state().unwrap();
        assert_eq!(state.stage, Stage::Scout);
        assert_eq!(production.load_budget().unwrap(), Budget::default());
        let mandate = production.load_mandate().unwrap();
        assert_eq!(mandate, Mandate::draw(&ledger, 5));
        assert_eq!(mandate.banned_families, vec![FormulaFamily::Mandelbox]);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn create_refuses_existing_and_finished_ids() {
        let root = temp_root("duplicate");
        let ledger = Ledger::from_entries(vec![entry(
            "finished",
            FormulaFamily::Mandelbox,
            Motion::Dive,
        )])
        .unwrap();
        Production::create(&root, "film", &ledger, 1, Budget::default()).unwrap();
        assert!(Production::create(&root, "film", &ledger, 1, Budget::default()).is_err());
        assert!(Production::create(&root, "finished", &ledger, 1, Budget::default()).is_err());
        assert!(!root.join("finished").exists());

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn open_requires_an_initialised_production() {
        let root = temp_root("open");
        assert!(Production::open(&root, "missing").is_err());
        Production::create(&root, "film", &Ledger::default(), 1, Budget::default()).unwrap();
        let production = Production::open(&root, "film").unwrap();
        assert_eq!(production.id(), "film");

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn state_changes_persist_without_leaving_temporaries() {
        let root = temp_root("persist");
        let production =
            Production::create(&root, "film", &Ledger::default(), 1, Budget::default()).unwrap();
        let mut state = production.load_state().unwrap();
        state.record_gate("novelty", true, "ok").unwrap();
        production.save_state(&state).unwrap();
        assert_eq!(production.load_state().unwrap(), state);

        let leftovers: Vec<_> = fs::read_dir(production.directory())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn state_of_another_production_is_rejected() {
        let root = temp_root("mismatch");
        let a = Production::create(&root, "a", &Ledger::default(), 1, Budget::default()).unwrap();
        let b = Production::create(&root, "b", &Ledger::default(), 1, Budget::default()).unwrap();
        fs::copy(a.path("state.json"), b.path("state.json")).unwrap();
        assert!(b.load_state().is_err());

        fs::remove_dir_all(&root).unwrap();
    }
}
