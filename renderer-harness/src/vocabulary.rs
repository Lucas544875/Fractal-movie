//! Requests for vocabulary the harness does not have, and what became of
//! them. A director that cannot film a beat records what was missing and
//! carries on with what exists. The requests are worked through later,
//! outside any production, so that the vocabulary grows from what films
//! actually needed and never changes under a film that is being made.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::stage::Stage;

const REQUESTS_FILE: &str = "vocabulary-requests.jsonl";
const RESOLUTIONS_FILE: &str = "vocabulary-resolutions.jsonl";
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
    /// Number of the request, assigned when it is recorded.
    #[serde(default)]
    pub id: u32,
    pub production: String,
    pub stage: Stage,
    pub kind: Kind,
    /// The moment of the film that could not be expressed.
    pub beat: String,
    /// What is missing, in the director's words.
    pub need: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Nobody has decided about the request yet.
    Open,
    /// The vocabulary now covers the request.
    Implemented,
    /// The request will not be implemented, for the reason noted.
    Declined,
}

/// The decision taken about one request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resolution {
    pub request: u32,
    pub status: Status,
    /// What was added and how to use it, or why the request was declined.
    pub note: String,
}

/// A request together with its current standing.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Entry {
    #[serde(flatten)]
    pub request: Request,
    pub status: Status,
    pub note: Option<String>,
}

fn sibling(ledger_path: &Path, name: &str) -> PathBuf {
    ledger_path.parent().unwrap_or(Path::new(".")).join(name)
}

/// Where requests are kept: next to the ledger, outside every production.
#[must_use]
pub fn requests_path(ledger_path: &Path) -> PathBuf {
    sibling(ledger_path, REQUESTS_FILE)
}

fn check_text(name: &str, text: &str) -> Result<()> {
    let length = text.trim().chars().count();
    if length == 0 || length > MAX_TEXT_CHARS {
        bail!("{name} must be 1 to {MAX_TEXT_CHARS} characters");
    }
    Ok(())
}

fn append<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let mut line = serde_json::to_string(value).context("could not serialize the record")?;
    line.push('\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(line.as_bytes()))
        .with_context(|| format!("could not write {}", path.display()))
}

fn read_lines<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
    };
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line)
                .with_context(|| format!("invalid record at {}:{}", path.display(), index + 1))
        })
        .collect()
}

/// Records a request and returns the number it was given.
pub fn record(ledger_path: &Path, request: &Request) -> Result<u32> {
    check_text("beat", &request.beat)?;
    check_text("need", &request.need)?;
    let id = load(ledger_path)?
        .iter()
        .map(|existing| existing.id)
        .max()
        .unwrap_or(0)
        + 1;
    append(
        &requests_path(ledger_path),
        &Request {
            id,
            ..request.clone()
        },
    )?;
    Ok(id)
}

/// Every request on record, oldest first.
pub fn load(ledger_path: &Path) -> Result<Vec<Request>> {
    let mut requests: Vec<Request> = read_lines(&requests_path(ledger_path))?;
    // Requests recorded before numbering are numbered by position.
    for (index, request) in requests.iter_mut().enumerate() {
        if request.id == 0 {
            request.id = index as u32 + 1;
        }
    }
    Ok(requests)
}

/// Every request with its standing, oldest first.
pub fn list(ledger_path: &Path) -> Result<Vec<Entry>> {
    let resolutions: Vec<Resolution> = read_lines(&sibling(ledger_path, RESOLUTIONS_FILE))?;
    Ok(load(ledger_path)?
        .into_iter()
        .map(|request| {
            let resolution = resolutions
                .iter()
                .find(|resolution| resolution.request == request.id);
            Entry {
                status: resolution.map_or(Status::Open, |resolution| resolution.status),
                note: resolution.map(|resolution| resolution.note.clone()),
                request,
            }
        })
        .collect())
}

/// Records the decision about an open request.
pub fn resolve(ledger_path: &Path, resolution: &Resolution) -> Result<()> {
    if resolution.status == Status::Open {
        bail!("a request is resolved as implemented or declined");
    }
    check_text("note", &resolution.note)?;
    let entries = list(ledger_path)?;
    let Some(entry) = entries
        .iter()
        .find(|entry| entry.request.id == resolution.request)
    else {
        bail!("there is no request {}", resolution.request);
    };
    if entry.status != Status::Open {
        bail!("request {} is already resolved", resolution.request);
    }
    append(&sibling(ledger_path, RESOLUTIONS_FILE), resolution)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(need: &str) -> Request {
        Request {
            id: 0,
            production: "film".to_owned(),
            stage: Stage::Brief,
            kind: Kind::Motion,
            beat: "end by pulling back to reveal the columns".to_owned(),
            need: need.to_owned(),
        }
    }

    fn ledger(name: &str) -> (PathBuf, PathBuf) {
        let directory = std::env::temp_dir().join(format!(
            "fractal-harness-vocabulary-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        (directory.join("portfolio/ledger.jsonl"), directory)
    }

    #[test]
    fn requests_are_numbered_and_kept_next_to_the_ledger() {
        let (ledger, directory) = ledger("record");
        assert!(load(&ledger).unwrap().is_empty());

        assert_eq!(record(&ledger, &request("a retreating camera")).unwrap(), 1);
        assert_eq!(record(&ledger, &request("a crane move")).unwrap(), 2);
        let all = load(&ledger).unwrap();
        assert_eq!((all[1].id, all[1].need.as_str()), (2, "a crane move"));
        assert_eq!(
            requests_path(&ledger),
            directory.join("portfolio").join(REQUESTS_FILE)
        );

        assert!(record(&ledger, &request("  ")).is_err());
        assert!(record(&ledger, &request(&"x".repeat(MAX_TEXT_CHARS + 1))).is_err());
        assert_eq!(load(&ledger).unwrap().len(), 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn requests_stay_open_until_resolved_once() {
        let (ledger, directory) = ledger("resolve");
        record(&ledger, &request("a retreating camera")).unwrap();
        record(&ledger, &request("a crane move")).unwrap();
        assert!(
            list(&ledger)
                .unwrap()
                .iter()
                .all(|entry| entry.status == Status::Open)
        );

        let implemented = Resolution {
            request: 1,
            status: Status::Implemented,
            note: "motion pull-back: { kind: pull-back, factor: 3.0 }".to_owned(),
        };
        resolve(&ledger, &implemented).unwrap();
        let entries = list(&ledger).unwrap();
        assert_eq!(entries[0].status, Status::Implemented);
        assert_eq!(entries[0].note.as_deref(), Some(implemented.note.as_str()));
        assert_eq!((entries[1].status, &entries[1].note), (Status::Open, &None));

        // A decision is final, needs a reason, and needs a real request.
        assert!(resolve(&ledger, &implemented).is_err());
        let decline = |request: u32, note: &str| {
            resolve(
                &ledger,
                &Resolution {
                    request,
                    status: Status::Declined,
                    note: note.to_owned(),
                },
            )
        };
        assert!(decline(2, " ").is_err());
        assert!(decline(9, "no such request").is_err());
        assert!(
            resolve(
                &ledger,
                &Resolution {
                    request: 2,
                    status: Status::Open,
                    note: "reopen".to_owned()
                }
            )
            .is_err()
        );
        decline(2, "an orbit with an elevated viewpoint already shows this").unwrap();
        assert_eq!(list(&ledger).unwrap()[1].status, Status::Declined);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn requests_recorded_before_numbering_are_numbered_by_position() {
        let (ledger, directory) = ledger("legacy");
        fs::create_dir_all(ledger.parent().unwrap()).unwrap();
        fs::write(
            requests_path(&ledger),
            "{\"production\":\"a\",\"stage\":\"brief\",\"kind\":\"motion\",\"beat\":\"b\",\"need\":\"n\"}\n",
        )
        .unwrap();
        assert_eq!(load(&ledger).unwrap()[0].id, 1);
        assert_eq!(record(&ledger, &request("next")).unwrap(), 2);
        fs::remove_dir_all(directory).unwrap();
    }
}
