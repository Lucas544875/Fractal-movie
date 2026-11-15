//! Location scouting. Shapes and viewpoints are sampled by the harness,
//! rendered as cheap thumbnails, and screened for legibility and cost before
//! an agent sees them. The agent then chooses locations by looking at the
//! contact sheets.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use fractal_renderer_core::{LoadedScene, Renderer, load_scene};
use serde::{Deserialize, Serialize};

use crate::{
    axes::{FormulaFamily, ScaleRegime, classify_family},
    budget::Budget,
    gpu::GpuOptions,
    mandate::Mandate,
    metrics::{Frame, Legibility, legibility},
    production::Production,
    rng::Rng,
    shape::{self, Shape},
    sheet::{self, Cell},
    stage::{Stage, State},
};

pub const THUMBNAIL_WIDTH: u32 = 320;
pub const THUMBNAIL_HEIGHT: u32 = 180;
pub const DEFAULT_SHAPES: usize = 60;
pub const MIN_SELECTED: usize = 2;
pub const MAX_SELECTED: usize = 5;

const SHEET_COLUMNS: usize = 6;
const SHEET_ROWS: usize = 4;
/// Formulas tried per wanted shape before a round gives up.
const ATTEMPTS_PER_SHAPE: usize = 8;
/// Relative parameter change used by `--around`.
const REFINE_AMOUNT: f64 = 0.12;

/// Final frame the cost estimate extrapolates to.
const FINAL_PIXELS: f64 = 1920.0 * 1080.0;
/// Samples per pixel assumed for the estimate; a typical outcome of the
/// quality decision.
const ESTIMATE_SPP: f64 = 8.0;

// Screening thresholds. Provisional until calibrated against finished films.
/// Below this share of flat area a frame is wall-to-wall detail.
const MIN_NEGATIVE_SPACE: f32 = 0.10;
/// Above this share there is no subject to look at.
const MAX_NEGATIVE_SPACE: f32 = 0.85;
const MAX_HIGH_FREQUENCY: f32 = 0.35;
const MIN_MEAN_LUMINANCE: f32 = 0.03;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rejection {
    /// Estimated final frame time exceeds the scouting ceiling.
    Cost,
    /// Detail fills the frame with nowhere for the eye to rest.
    Cluttered,
    /// Almost nothing is in frame.
    Empty,
    /// Too dark to read.
    Dark,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewRecord {
    pub id: String,
    pub scale_regime: ScaleRegime,
    pub legibility: Legibility,
    /// Extrapolated GPU seconds for one final frame.
    pub estimated_frame_seconds: f64,
    pub rejection: Option<Rejection>,
    /// Sheet file the thumbnail appears on, when it was not rejected.
    pub sheet: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocationRecord {
    pub family: FormulaFamily,
    pub bounded: bool,
    pub round: u32,
    pub viewpoints: Vec<ViewRecord>,
}

impl LocationRecord {
    fn usable_viewpoints(&self) -> impl Iterator<Item = &ViewRecord> {
        self.viewpoints
            .iter()
            .filter(|view| view.rejection.is_none())
    }
}

/// Everything scouting has produced for one production.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub rounds: u32,
    pub next_location: u32,
    pub locations: BTreeMap<String, LocationRecord>,
    pub selected: Vec<String>,
}

impl Index {
    pub fn load(production: &Production) -> Result<Self> {
        let path = index_path(production);
        match fs::read_to_string(&path) {
            Ok(text) => {
                serde_json::from_str(&text).with_context(|| format!("invalid {}", path.display()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).with_context(|| format!("could not read {}", path.display())),
        }
    }

    fn save(&self, production: &Production) -> Result<()> {
        let path = index_path(production);
        let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
        let mut text = serde_json::to_string_pretty(self).context("could not serialize index")?;
        text.push('\n');
        fs::write(&temporary, text)
            .with_context(|| format!("could not write {}", temporary.display()))?;
        fs::rename(&temporary, &path)
            .with_context(|| format!("could not replace {}", path.display()))
    }
}

fn index_path(production: &Production) -> PathBuf {
    production.path("scout/index.json")
}

/// Scene file of one viewpoint. Formulas live with the harness-owned compiled
/// files, away from the sheets an agent reads.
fn viewpoint_scene_path(production: &Production, location: &str, viewpoint: &str) -> PathBuf {
    production.path(&format!("compiled/locations/{location}/{viewpoint}.yaml"))
}

#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    pub shapes: usize,
    /// Sample variations of this location instead of new formulas.
    pub around: Option<String>,
    pub gpu: GpuOptions,
}

#[derive(Clone, Debug, Serialize)]
pub struct Candidate {
    /// Label drawn on the sheet: location number, hyphen, viewpoint number.
    pub label: String,
    pub location: String,
    pub viewpoint: String,
    pub family: FormulaFamily,
    pub scale_regime: ScaleRegime,
    pub sheet: String,
    pub legibility: Legibility,
    pub estimated_frame_seconds: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct RunReport {
    pub round: u32,
    pub formulas_tried: usize,
    pub shapes_rendered: usize,
    pub viewpoints_rendered: usize,
    pub rejected: BTreeMap<Rejection, usize>,
    pub sheets: Vec<String>,
    pub candidates: Vec<Candidate>,
    pub gpu_seconds: f64,
    pub design_remaining_gpu_seconds: f64,
}

/// Runs one scouting round and returns the candidates worth looking at.
pub fn run(
    production: &Production,
    state: &State,
    mandate: &Mandate,
    budget: &mut Budget,
    options: &RunOptions,
) -> Result<RunReport> {
    require_scout_stage(state)?;
    if options.shapes == 0 {
        bail!("a scouting round needs at least one shape");
    }
    if budget.design_remaining_seconds() <= 0.0 {
        bail!("the design GPU budget of this production is exhausted");
    }
    let duty_cycle = options
        .gpu
        .duty_cycle_percent
        .unwrap_or(budget.max_gpu_duty_cycle);
    budget.check_duty_cycle(duty_cycle)?;
    let gpu = GpuOptions {
        duty_cycle_percent: Some(duty_cycle),
        ..options.gpu.clone()
    };

    let mut index = Index::load(production)?;
    let round = index.rounds + 1;
    // Each round draws from its own stream, so repeating a round number
    // reproduces it and consecutive rounds do not overlap.
    let mut rng = Rng::new(mandate.seed ^ u64::from(round).wrapping_mul(0x9E37_79B9_7F4A_7C15));

    let (shapes, formulas_tried) = match &options.around {
        Some(location) => refine_shapes(
            production,
            &index,
            state,
            location,
            options.shapes,
            &mut rng,
        )?,
        None => discover_shapes(mandate, options.shapes, &mut rng)?,
    };

    let round_directory = format!("scout/round-{round:02}");
    fs::create_dir_all(production.path(&format!("{round_directory}/thumbs")))
        .context("could not create the round directory")?;
    let cost_ceiling = budget.max_frame_seconds / 2.0;
    let scale_to_final =
        FINAL_PIXELS / f64::from(THUMBNAIL_WIDTH * THUMBNAIL_HEIGHT) * ESTIMATE_SPP;

    let mut gpu_seconds = 0.0;
    let mut viewpoints_rendered = 0;
    let mut rejected: BTreeMap<Rejection, usize> = BTreeMap::new();
    // Thumbnails shown on sheets, in sheet order.
    let mut shown: Vec<(Candidate, Vec<u8>)> = Vec::new();

    // One GPU device serves the whole round; only the shader is swapped.
    let mut renderer: Option<Renderer> = None;
    for shape in &shapes {
        let viewpoints = shape::viewpoints(&mut rng, shape);
        if viewpoints.is_empty() {
            continue;
        }
        let number = index.next_location;
        let location = format!("loc-{number:04}");
        let seed = rng.next_u64() as u32;
        let configs: Vec<_> = viewpoints
            .iter()
            .map(|viewpoint| {
                shape::thumbnail_config(shape, viewpoint, THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT, seed)
            })
            .collect();
        let mut records = Vec::with_capacity(viewpoints.len());
        for (view_index, (viewpoint, config)) in viewpoints.iter().zip(&configs).enumerate() {
            let view_id = format!("vp-{}", view_index + 1);
            // Fog is scaled to each viewpoint and compiled into the shader,
            // so every viewpoint has its own pipeline.
            match &mut renderer {
                Some(renderer) => pollster::block_on(renderer.replace_fractal(config.clone()))
                    .with_context(|| format!("could not compile {location} {view_id}"))?,
                None => renderer = Some(gpu.renderer(config.clone())?),
            }
            let renderer = renderer.as_ref().expect("renderer was just initialised");
            // The driver compiles the shader on first use. Rendering once
            // before measuring keeps that one-off cost out of the frame-time
            // estimate.
            gpu_seconds += renderer
                .render_frame_with_config(config, 0, 0.0)
                .with_context(|| format!("could not warm up {location} {view_id}"))?
                .gpu_time()
                .as_secs_f64();
            let image = renderer
                .render_frame_with_config(config, 0, 0.0)
                .with_context(|| format!("could not render {location} {view_id}"))?;
            gpu_seconds += image.gpu_time().as_secs_f64();
            viewpoints_rendered += 1;

            let measured = legibility(&Frame::from_rgba8(
                image.width(),
                image.height(),
                image.pixels(),
            )?);
            let estimated_frame_seconds = image.gpu_time().as_secs_f64() * scale_to_final;
            let rejection = screen(&measured, estimated_frame_seconds, cost_ceiling);
            if let Some(reason) = rejection {
                *rejected.entry(reason).or_default() += 1;
            }

            save_viewpoint_scene(production, &location, &view_id, config)?;
            sheet::save_png(
                &production.path(&format!(
                    "{round_directory}/thumbs/{location}-{view_id}.png"
                )),
                image.width(),
                image.height(),
                image.pixels(),
            )?;

            if rejection.is_none() {
                shown.push((
                    Candidate {
                        label: format!("{number}-{}", view_index + 1),
                        location: location.clone(),
                        viewpoint: view_id.clone(),
                        family: shape.family,
                        scale_regime: viewpoint.scale_regime,
                        sheet: String::new(),
                        legibility: measured,
                        estimated_frame_seconds,
                    },
                    image.pixels().to_vec(),
                ));
            }
            records.push(ViewRecord {
                id: view_id,
                scale_regime: viewpoint.scale_regime,
                legibility: measured,
                estimated_frame_seconds,
                rejection,
                sheet: None,
            });
        }

        index.next_location += 1;
        index.locations.insert(
            location,
            LocationRecord {
                family: shape.family,
                bounded: shape.survey.bounded,
                round,
                viewpoints: records,
            },
        );
    }

    let mut sheets = Vec::new();
    for (page, chunk) in shown.chunks_mut(SHEET_COLUMNS * SHEET_ROWS).enumerate() {
        let name = format!("{round_directory}/sheet-{:02}.png", page + 1);
        let cells: Vec<Cell<'_>> = chunk
            .iter()
            .map(|(candidate, rgba)| Cell {
                label: &candidate.label,
                rgba,
            })
            .collect();
        sheet::compose(
            &cells,
            SHEET_COLUMNS,
            THUMBNAIL_WIDTH as usize,
            THUMBNAIL_HEIGHT as usize,
        )?
        .save_png(&production.path(&name))?;
        for (candidate, _) in chunk.iter_mut() {
            candidate.sheet.clone_from(&name);
            if let Some(view) = index
                .locations
                .get_mut(&candidate.location)
                .and_then(|record| {
                    record
                        .viewpoints
                        .iter_mut()
                        .find(|view| view.id == candidate.viewpoint)
                })
            {
                view.sheet = Some(name.clone());
            }
        }
        sheets.push(name);
    }

    index.rounds = round;
    index.save(production)?;
    budget.spent.design_gpu_seconds += gpu_seconds;
    production.save_budget(budget)?;

    Ok(RunReport {
        round,
        formulas_tried,
        shapes_rendered: shapes.len(),
        viewpoints_rendered,
        rejected,
        sheets,
        candidates: shown.into_iter().map(|(candidate, _)| candidate).collect(),
        gpu_seconds,
        design_remaining_gpu_seconds: budget.design_remaining_seconds(),
    })
}

/// Decides whether a thumbnail is worth an agent's attention.
fn screen(measured: &Legibility, estimated_seconds: f64, cost_ceiling: f64) -> Option<Rejection> {
    if estimated_seconds > cost_ceiling {
        Some(Rejection::Cost)
    } else if measured.mean_luminance < MIN_MEAN_LUMINANCE {
        Some(Rejection::Dark)
    } else if measured.negative_space_ratio > MAX_NEGATIVE_SPACE {
        Some(Rejection::Empty)
    } else if measured.negative_space_ratio < MIN_NEGATIVE_SPACE
        || measured.high_frequency_ratio > MAX_HIGH_FREQUENCY
    {
        Some(Rejection::Cluttered)
    } else {
        None
    }
}

fn discover_shapes(mandate: &Mandate, wanted: usize, rng: &mut Rng) -> Result<(Vec<Shape>, usize)> {
    let families = mandate.allowed_families();
    if families.is_empty() {
        bail!("the mandate leaves no formula family to scout");
    }
    let mut shapes = Vec::with_capacity(wanted);
    let mut tried = 0;
    while shapes.len() < wanted && tried < wanted * ATTEMPTS_PER_SHAPE {
        // Cycling gives every permitted family the same number of attempts.
        let family = families[tried % families.len()];
        tried += 1;
        if let Some(shape) = shape::discover(rng, family) {
            shapes.push(shape);
        }
    }
    if shapes.is_empty() {
        bail!("no usable shape was found in {tried} formulas");
    }
    Ok((shapes, tried))
}

fn refine_shapes(
    production: &Production,
    index: &Index,
    state: &State,
    location: &str,
    wanted: usize,
    rng: &mut Rng,
) -> Result<(Vec<Shape>, usize)> {
    let record = index
        .locations
        .get(location)
        .with_context(|| format!("unknown location {location:?}"))?;
    if state
        .banned_locations
        .iter()
        .any(|banned| banned == location)
    {
        bail!("location {location} was abandoned by a kill and cannot be refined");
    }
    let first = record
        .viewpoints
        .first()
        .with_context(|| format!("location {location} has no viewpoint"))?;
    let scene = load_scene(viewpoint_scene_path(production, location, &first.id))?;

    let mut shapes = Vec::with_capacity(wanted);
    let mut tried = 0;
    while shapes.len() < wanted && tried < wanted * ATTEMPTS_PER_SHAPE {
        tried += 1;
        let fractal = shape::mutate(rng, &scene.config.fractal, REFINE_AMOUNT);
        if let Some(survey) = shape::survey(rng, &fractal) {
            shapes.push(Shape {
                family: classify_family(&fractal),
                fractal,
                survey,
            });
        }
    }
    if shapes.is_empty() {
        bail!("no usable variation of {location} was found in {tried} attempts");
    }
    Ok((shapes, tried))
}

fn save_viewpoint_scene(
    production: &Production,
    location: &str,
    viewpoint: &str,
    config: &fractal_renderer_core::RenderConfig,
) -> Result<()> {
    let path = viewpoint_scene_path(production, location, viewpoint);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let yaml = LoadedScene {
        name: format!("{location}-{viewpoint}"),
        config: config.clone(),
        animation: None,
        video: None,
    }
    .to_yaml()?;
    write_file(&path, &yaml)
}

fn write_file(path: &Path, text: &str) -> Result<()> {
    fs::write(path, text).with_context(|| format!("could not write {}", path.display()))
}

#[derive(Clone, Debug, Serialize)]
pub struct SelectedLocation {
    pub location: String,
    pub family: FormulaFamily,
    pub viewpoints: Vec<SelectedViewpoint>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SelectedViewpoint {
    pub viewpoint: String,
    pub scale_regime: ScaleRegime,
    pub estimated_frame_seconds: f64,
}

/// Records the director's choice of locations and judges the scouting gates
/// that depend only on that choice.
pub fn select(
    production: &Production,
    state: &mut State,
    budget: &Budget,
    locations: &[String],
) -> Result<Vec<SelectedLocation>> {
    require_scout_stage(state)?;
    if !(MIN_SELECTED..=MAX_SELECTED).contains(&locations.len()) {
        bail!(
            "select {MIN_SELECTED} to {MAX_SELECTED} locations, not {}",
            locations.len()
        );
    }
    let mut index = Index::load(production)?;
    let cost_ceiling = budget.max_frame_seconds / 2.0;

    let mut selected = Vec::with_capacity(locations.len());
    for (position, location) in locations.iter().enumerate() {
        if locations[..position].contains(location) {
            bail!("location {location} is listed more than once");
        }
        if state.banned_locations.contains(location) {
            bail!("location {location} was abandoned by a kill and cannot be selected again");
        }
        let record = index.locations.get(location).with_context(|| {
            format!("unknown location {location:?}; use an id from a scouting round")
        })?;
        let viewpoints: Vec<SelectedViewpoint> = record
            .usable_viewpoints()
            .map(|view| SelectedViewpoint {
                viewpoint: view.id.clone(),
                scale_regime: view.scale_regime,
                estimated_frame_seconds: view.estimated_frame_seconds,
            })
            .collect();
        if viewpoints.is_empty() {
            bail!("location {location} has no viewpoint that passed screening");
        }
        selected.push(SelectedLocation {
            location: location.clone(),
            family: record.family,
            viewpoints,
        });
    }

    let slowest = selected
        .iter()
        .flat_map(|location| &location.viewpoints)
        .map(|view| view.estimated_frame_seconds)
        .fold(0.0, f64::max);
    index.selected = locations.to_vec();
    index.save(production)?;
    state.record_gate(
        "locations-selected",
        true,
        format!("{} locations: {}", locations.len(), locations.join(", ")),
    )?;
    state.record_gate(
        "cost-ceiling",
        slowest <= cost_ceiling,
        format!(
            "slowest usable viewpoint is estimated at {slowest:.2} s per frame; ceiling {cost_ceiling:.2} s"
        ),
    )?;
    Ok(selected)
}

fn require_scout_stage(state: &State) -> Result<()> {
    if state.stage != Stage::Scout {
        bail!(
            "scouting is only possible in stage scout; the production is in stage {}",
            state.stage.as_str()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Ledger;

    fn measured(negative_space: f32, high_frequency: f32, luminance: f32) -> Legibility {
        Legibility {
            high_frequency_ratio: high_frequency,
            negative_space_ratio: negative_space,
            hue_clusters: 0,
            clipped_high_ratio: 0.0,
            clipped_low_ratio: 0.0,
            mean_luminance: luminance,
        }
    }

    fn view(id: &str, rejection: Option<Rejection>, seconds: f64) -> ViewRecord {
        ViewRecord {
            id: id.to_owned(),
            scale_regime: ScaleRegime::Mid,
            legibility: measured(0.4, 0.1, 0.3),
            estimated_frame_seconds: seconds,
            rejection,
            sheet: None,
        }
    }

    fn production(name: &str) -> (Production, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "fractal-harness-scout-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let production =
            Production::create(&root, "film", &Ledger::default(), 1, Budget::default()).unwrap();
        (production, root)
    }

    fn seed_index(production: &Production) {
        let location = |views: Vec<ViewRecord>| LocationRecord {
            family: FormulaFamily::JuliaBox,
            bounded: true,
            round: 1,
            viewpoints: views,
        };
        let index = Index {
            rounds: 1,
            next_location: 4,
            locations: BTreeMap::from([
                (
                    "loc-0000".to_owned(),
                    location(vec![view("vp-1", None, 1.0)]),
                ),
                (
                    "loc-0001".to_owned(),
                    location(vec![
                        view("vp-1", Some(Rejection::Cluttered), 2.0),
                        view("vp-2", None, 3.0),
                    ]),
                ),
                (
                    "loc-0002".to_owned(),
                    location(vec![view("vp-1", Some(Rejection::Empty), 1.0)]),
                ),
                (
                    "loc-0003".to_owned(),
                    location(vec![view("vp-1", None, 1.5)]),
                ),
            ]),
            selected: Vec::new(),
        };
        index.save(production).unwrap();
    }

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn screening_names_the_reason_for_rejection() {
        let ceiling = 6.0;
        assert_eq!(screen(&measured(0.4, 0.1, 0.3), 1.0, ceiling), None);
        assert_eq!(
            screen(&measured(0.4, 0.1, 0.3), 6.5, ceiling),
            Some(Rejection::Cost)
        );
        assert_eq!(
            screen(&measured(0.4, 0.1, 0.01), 1.0, ceiling),
            Some(Rejection::Dark)
        );
        assert_eq!(
            screen(&measured(0.95, 0.1, 0.3), 1.0, ceiling),
            Some(Rejection::Empty)
        );
        assert_eq!(
            screen(&measured(0.02, 0.1, 0.3), 1.0, ceiling),
            Some(Rejection::Cluttered)
        );
        assert_eq!(
            screen(&measured(0.4, 0.5, 0.3), 1.0, ceiling),
            Some(Rejection::Cluttered)
        );
    }

    #[test]
    fn discovery_honours_the_mandate_and_spreads_over_families() {
        let mandate = Mandate {
            seed: 1,
            banned_families: vec![FormulaFamily::Mandelbox, FormulaFamily::AmazingSurf],
            assigned: Vec::new(),
        };
        let (shapes, tried) = discover_shapes(&mandate, 20, &mut Rng::new(1)).unwrap();
        assert_eq!(shapes.len(), 20);
        assert!(tried >= 20);
        let families: std::collections::BTreeSet<_> =
            shapes.iter().map(|shape| shape.family).collect();
        assert!(!families.contains(&FormulaFamily::Mandelbox));
        assert!(!families.contains(&FormulaFamily::AmazingSurf));
        assert!(families.len() >= 4, "{families:?}");
    }

    #[test]
    fn selection_records_gates_and_usable_viewpoints() {
        let (production, root) = production("select");
        seed_index(&production);
        let mut state = production.load_state().unwrap();

        let selected = select(
            &production,
            &mut state,
            &Budget::default(),
            &ids(&["loc-0001", "loc-0003"]),
        )
        .unwrap();
        // The cluttered viewpoint of loc-0001 is not offered for use.
        assert_eq!(selected[0].viewpoints.len(), 1);
        assert_eq!(selected[0].viewpoints[0].viewpoint, "vp-2");
        assert!(state.gates["locations-selected"].passed);
        assert!(state.gates["cost-ceiling"].passed);
        // Novelty is judged against the ledger, not by selection alone.
        assert_eq!(state.unmet_gates(), vec!["novelty"]);
        assert_eq!(
            Index::load(&production).unwrap().selected,
            ids(&["loc-0001", "loc-0003"])
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selection_rejects_invalid_choices() {
        let (production, root) = production("reject");
        seed_index(&production);
        let mut state = production.load_state().unwrap();
        state.ban_locations(["loc-0003".to_owned()]);
        let budget = Budget::default();
        let mut attempt = |values: &[&str]| {
            select(&production, &mut state, &budget, &ids(values))
                .unwrap_err()
                .to_string()
        };

        assert!(attempt(&["loc-0000"]).contains("2 to 5"));
        assert!(attempt(&["loc-0000", "loc-0000"]).contains("more than once"));
        assert!(attempt(&["loc-0000", "loc-0009"]).contains("unknown location"));
        assert!(attempt(&["loc-0000", "loc-0002"]).contains("no viewpoint"));
        assert!(attempt(&["loc-0000", "loc-0003"]).contains("abandoned"));
        assert!(Index::load(&production).unwrap().selected.is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_slow_selection_fails_the_cost_gate() {
        let (production, root) = production("cost");
        seed_index(&production);
        let mut state = production.load_state().unwrap();
        let budget = Budget {
            max_frame_seconds: 4.0,
            ..Budget::default()
        };
        select(
            &production,
            &mut state,
            &budget,
            &ids(&["loc-0000", "loc-0001"]),
        )
        .unwrap();
        assert!(!state.gates["cost-ceiling"].passed);
        assert!(state.gates["cost-ceiling"].detail.contains("3.00"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scouting_is_refused_outside_its_stage() {
        let (production, root) = production("stage");
        seed_index(&production);
        let mut state = production.load_state().unwrap();
        state.stage = Stage::Brief;
        let error = select(
            &production,
            &mut state,
            &Budget::default(),
            &ids(&["loc-0000", "loc-0001"]),
        )
        .unwrap_err();
        assert!(error.to_string().contains("stage brief"));

        fs::remove_dir_all(root).unwrap();
    }
}
