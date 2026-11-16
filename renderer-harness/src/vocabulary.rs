//! Requests for vocabulary the harness does not have. A director that
//! cannot film a beat records what was missing and carries on with what
//! exists; the requests are implemented later, outside any production, so
//! that the vocabulary grows from what films actually needed.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::stage::Stage;

const REQUESTS_FILE: &str = "vocabulary-requests.jsonl";
const MAX_TEXT_CHARS: usize = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// A camera motion.
    Motion,
    /// A distance estimator or orbit transform.
    Shape,
    /// A surface or colouring feature.
    Material,
    /// A lighting feature.
    Light,
    Other,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub production: String,
    pub stage: Stage,
    pub kind: Kind,
    /// The moment of the film that could not be expressed.
    pub beat: String,
    /// What is missing, in the director's words.
    pub need: String,
}

/// Where requests are kept: next to the ledger, outside every production.
#[must_use]
pub fn requests_path(ledger_path: &Path) -> PathBuf {
    ledger_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(REQUESTS_FILE)
}

/// Appends a request and returns how many are on record.
pub fn record(ledger_path: &Path, request: &Request) -> Result<usize> {
    for (name, text) in [("beat", &request.beat), ("need", &request.need)] {
        let length = text.trim().chars().count();
        if length == 0 || length > MAX_TEXT_CHARS {
            bail!("{name} must be 1 to {MAX_TEXT_CHARS} characters");
        }
    }
    let path = requests_path(ledger_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let mut line = serde_json::to_string(request).context("could not serialize the request")?;
    line.push('\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| file.write_all(line.as_bytes()))
        .with_context(|| format!("could not record the request in {}", path.display()))?;
    Ok(load(ledger_path)?.len())
}

/// Every request on record, oldest first.
pub fn load(ledger_path: &Path) -> Result<Vec<Request>> {
    let path = requests_path(ledger_path);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line)
                .with_context(|| format!("invalid request at {}:{}", path.display(), index + 1))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(need: &str) -> Request {
        Request {
            production: "film".to_owned(),
            stage: Stage::Brief,
            kind: Kind::Motion,
            beat: "end by pulling back to reveal the columns".to_owned(),
            need: need.to_owned(),
        }
    }

    #[test]
    fn requests_accumulate_next_to_the_ledger() {
        let directory =
            std::env::temp_dir().join(format!("fractal-harness-vocabulary-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let ledger = directory.join("portfolio/ledger.jsonl");
        assert!(load(&ledger).unwrap().is_empty());

        assert_eq!(record(&ledger, &request("a retreating camera")).unwrap(), 1);
        assert_eq!(record(&ledger, &request("a crane move")).unwrap(), 2);
        let all = load(&ledger).unwrap();
        assert_eq!(all[1].need, "a crane move");
        assert_eq!(
            requests_path(&ledger),
            directory.join("portfolio").join(REQUESTS_FILE)
        );

        assert!(record(&ledger, &request("  ")).is_err());
        assert!(record(&ledger, &request(&"x".repeat(MAX_TEXT_CHARS + 1))).is_err());
        assert_eq!(load(&ledger).unwrap().len(), 2);
        fs::remove_dir_all(directory).unwrap();
    }
}
