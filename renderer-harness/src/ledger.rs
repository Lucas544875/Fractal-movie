//! Append-only record of finished films. The ledger is the only knowledge of
//! past work that the diversity rules consult.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::axes::{Axes, AxisId, FormulaFamily};

/// Number of most recent films whose formula family may not be reused.
pub const FAMILY_BAN_WINDOW: usize = 3;
/// Number of most recent films a new film is compared against.
pub const AXIS_DIFF_WINDOW: usize = 5;
/// Minimum number of differing axes against each compared film.
pub const MIN_DIFFERING_AXES: usize = 3;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerEntry {
    pub id: String,
    /// ISO 8601 calendar date of completion.
    pub date: String,
    pub title: String,
    pub axes: Axes,
    /// Image descriptor of the film's keyframes, used by the novelty gate.
    #[serde(default)]
    pub descriptor: Vec<f32>,
    #[serde(default)]
    pub keyframes: Vec<String>,
    pub duration_seconds: f64,
    pub gpu_minutes: f64,
    #[serde(default)]
    pub compromised: bool,
}

/// A film that is too similar to the proposed axes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AxisDiffViolation {
    pub film: String,
    pub differing: Vec<AxisId>,
    pub required: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Ledger {
    entries: Vec<LedgerEntry>,
}

impl Ledger {
    /// Loads the ledger. A missing file is an empty portfolio.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("could not read ledger {}", path.display()));
            }
        };

        let mut entries = Vec::new();
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let entry: LedgerEntry = serde_json::from_str(line).with_context(|| {
                format!("invalid ledger entry at {}:{}", path.display(), index + 1)
            })?;
            entries.push(entry);
        }
        Self::from_entries(entries)
    }

    pub fn from_entries(entries: Vec<LedgerEntry>) -> Result<Self> {
        for (index, entry) in entries.iter().enumerate() {
            if entries[..index]
                .iter()
                .any(|earlier| earlier.id == entry.id)
            {
                bail!("ledger contains duplicate film id {:?}", entry.id);
            }
        }
        Ok(Self { entries })
    }

    /// Appends one finished film to the ledger file and to this instance.
    pub fn append(&mut self, path: &Path, entry: LedgerEntry) -> Result<()> {
        if self.entries.iter().any(|existing| existing.id == entry.id) {
            bail!("ledger already contains film id {:?}", entry.id);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        let mut line = serde_json::to_string(&entry).context("could not serialize ledger entry")?;
        line.push('\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("could not open ledger {}", path.display()))?;
        file.write_all(line.as_bytes())
            .with_context(|| format!("could not append to ledger {}", path.display()))?;
        self.entries.push(entry);
        Ok(())
    }

    #[must_use]
    pub fn entries(&self) -> &[LedgerEntry] {
        &self.entries
    }

    /// The most recent `count` films, newest first.
    pub fn recent(&self, count: usize) -> impl Iterator<Item = &LedgerEntry> {
        self.entries.iter().rev().take(count)
    }

    /// Families used by the most recent films, which a new film may not reuse.
    #[must_use]
    pub fn banned_families(&self) -> Vec<FormulaFamily> {
        let mut banned = Vec::new();
        for entry in self.recent(FAMILY_BAN_WINDOW) {
            if !banned.contains(&entry.axes.formula_family) {
                banned.push(entry.axes.formula_family);
            }
        }
        banned
    }

    /// How many films in the whole ledger used `value` on `axis`.
    #[must_use]
    pub fn usage(&self, axis: AxisId, value: &str) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.axes.value(axis) == value)
            .count()
    }

    /// Recent films from which `axes` does not differ on enough axes.
    #[must_use]
    pub fn axis_diff_violations(&self, axes: &Axes) -> Vec<AxisDiffViolation> {
        self.recent(AXIS_DIFF_WINDOW)
            .filter_map(|entry| {
                let differing = axes.differing(&entry.axes);
                (differing.len() < MIN_DIFFERING_AXES).then(|| AxisDiffViolation {
                    film: entry.id.clone(),
                    differing,
                    required: MIN_DIFFERING_AXES,
                })
            })
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::axes::{Light, Motion, Pacing, Palette, PaletteHue, PaletteKey, ScaleRegime};

    pub(crate) fn entry(id: &str, family: FormulaFamily, motion: Motion) -> LedgerEntry {
        LedgerEntry {
            id: id.to_owned(),
            date: "2026-11-15".to_owned(),
            title: id.to_owned(),
            axes: Axes {
                formula_family: family,
                scale_regime: ScaleRegime::Mid,
                motion,
                palette: Palette {
                    primary_hue: PaletteHue::Teal,
                    key: PaletteKey::Low,
                },
                light: Light::Raking,
                pacing: Pacing::Slow,
            },
            descriptor: Vec::new(),
            keyframes: Vec::new(),
            duration_seconds: 10.0,
            gpu_minutes: 99.0,
            compromised: false,
        }
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fractal-harness-ledger-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn missing_file_is_an_empty_ledger() {
        let ledger = Ledger::load(Path::new("/nonexistent/ledger.jsonl")).unwrap();
        assert!(ledger.entries().is_empty());
        assert!(ledger.banned_families().is_empty());
    }

    #[test]
    fn append_persists_and_rejects_duplicates() {
        let path = temp_path("append.jsonl");
        let _ = fs::remove_file(&path);
        let mut ledger = Ledger::load(&path).unwrap();
        ledger
            .append(&path, entry("a", FormulaFamily::Mandelbox, Motion::Dive))
            .unwrap();
        ledger
            .append(&path, entry("b", FormulaFamily::AmazingSurf, Motion::Orbit))
            .unwrap();
        assert!(
            ledger
                .append(&path, entry("a", FormulaFamily::Mandelbulb, Motion::Dive))
                .is_err()
        );

        let reloaded = Ledger::load(&path).unwrap();
        assert_eq!(reloaded.entries(), ledger.entries());
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn invalid_line_reports_its_position() {
        let path = temp_path("invalid.jsonl");
        fs::write(&path, "\n{\"id\": 1}\n").unwrap();
        let error = format!("{:#}", Ledger::load(&path).unwrap_err());
        assert!(error.contains(":2"), "{error}");
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn only_recent_families_are_banned() {
        let ledger = Ledger::from_entries(vec![
            entry("a", FormulaFamily::Mandelbulb, Motion::Dive),
            entry("b", FormulaFamily::Mandelbox, Motion::Dive),
            entry("c", FormulaFamily::Mandelbox, Motion::Orbit),
            entry("d", FormulaFamily::AmazingSurf, Motion::Orbit),
        ])
        .unwrap();
        assert_eq!(
            ledger.banned_families(),
            vec![FormulaFamily::AmazingSurf, FormulaFamily::Mandelbox]
        );
        assert_eq!(ledger.usage(AxisId::Motion, "orbit"), 2);
        assert_eq!(ledger.usage(AxisId::Motion, "flyover"), 0);
    }

    #[test]
    fn near_duplicate_axes_are_reported() {
        let ledger = Ledger::from_entries(vec![
            entry("old", FormulaFamily::Mandelbox, Motion::Dive),
            entry("recent", FormulaFamily::AmazingSurf, Motion::Orbit),
        ])
        .unwrap();

        let mut proposal = entry("new", FormulaFamily::Mandelbulb, Motion::Flyover).axes;
        let violations = ledger.axis_diff_violations(&proposal);
        assert_eq!(violations.len(), 2);
        assert_eq!(violations[0].film, "recent");
        assert_eq!(
            violations[0].differing,
            vec![AxisId::FormulaFamily, AxisId::Motion]
        );

        proposal.light = Light::Backlit;
        assert!(ledger.axis_diff_violations(&proposal).is_empty());
    }
}
