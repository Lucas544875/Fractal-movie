//! Evidence for new shape vocabulary. Before a formula family is offered to
//! directors, a sample shows what scouting would make of it: how many
//! generated formulas describe a usable shape, how many views pass
//! screening, what they cost, and what they look like.

use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result, bail};
use fractal_renderer_core::Renderer;
use serde::Serialize;

use crate::{
    axes::FormulaFamily,
    budget::Budget,
    gpu::GpuOptions,
    metrics::{Frame, legibility},
    rng::Rng,
    scout::{Rejection, THUMBNAIL_HEIGHT, THUMBNAIL_WIDTH, screen},
    shape,
    sheet::{self, Cell},
    tuning::{Tuning, VocabularyRules},
};

const SHEET_COLUMNS: usize = 6;
const SHEET_ROWS: usize = 4;
/// Final frame the cost estimate extrapolates to.
const FINAL_PIXELS: f64 = 1920.0 * 1080.0;

#[derive(Clone, Debug, Serialize)]
pub struct SampleReport {
    pub family: FormulaFamily,
    pub formulas_tried: usize,
    /// Formulas that survey as a usable shape.
    pub shapes: usize,
    pub bounded_shapes: usize,
    pub viewpoints: usize,
    /// Views that pass screening and would reach a contact sheet.
    pub offered: usize,
    pub rejected: BTreeMap<Rejection, usize>,
    /// Extrapolated GPU seconds per final frame of the offered views.
    pub median_frame_seconds: Option<f64>,
    pub slowest_frame_seconds: Option<f64>,
    /// Sheets of the offered views; labels read "shape-viewpoint".
    pub sheets: Vec<String>,
    pub gpu_seconds: f64,
    /// Why the family does not yet yield enough to be worth scouting;
    /// empty when it does. Whether its shapes look unlike the existing
    /// families is for a person or reviewer to judge from the sheets.
    pub shortfalls: Vec<String>,
}

/// Measures a sample's yield against the vocabulary rules.
fn shortfalls(report: &SampleReport, rules: &VocabularyRules) -> Vec<String> {
    let mut found = Vec::new();
    let shape_yield = report.shapes as f64 / report.formulas_tried.max(1) as f64;
    if shape_yield < rules.min_shape_yield {
        found.push(format!(
            "only {:.0}% of the formulas describe a usable shape; at least {:.0}% is needed",
            shape_yield * 100.0,
            rules.min_shape_yield * 100.0
        ));
    }
    let offered_share = report.offered as f64 / report.viewpoints.max(1) as f64;
    if offered_share < rules.min_offered_share {
        found.push(format!(
            "only {:.0}% of the views pass screening; at least {:.0}% is needed",
            offered_share * 100.0,
            rules.min_offered_share * 100.0
        ));
    }
    found
}

/// Generates `formulas` formulas of one family, renders every viewpoint
/// scouting finds, and writes contact sheets of the views it would offer.
pub fn family(
    family: FormulaFamily,
    formulas: usize,
    seed: u64,
    directory: &Path,
    gpu: &GpuOptions,
    tuning: &Tuning,
) -> Result<SampleReport> {
    if formulas == 0 {
        bail!("a sample needs at least one formula");
    }
    fs::create_dir_all(directory)
        .with_context(|| format!("could not create {}", directory.display()))?;
    let cost_ceiling = Budget::default().max_frame_seconds * tuning.scouting.cost_ceiling_share;
    let scale_to_final = FINAL_PIXELS / f64::from(THUMBNAIL_WIDTH * THUMBNAIL_HEIGHT)
        * tuning.scouting.estimate_samples_per_pixel;

    let mut rng = Rng::new(seed);
    let mut renderer: Option<Renderer> = None;
    let mut report = SampleReport {
        family,
        formulas_tried: formulas,
        shapes: 0,
        bounded_shapes: 0,
        viewpoints: 0,
        offered: 0,
        rejected: BTreeMap::new(),
        median_frame_seconds: None,
        slowest_frame_seconds: None,
        sheets: Vec::new(),
        gpu_seconds: 0.0,
        shortfalls: Vec::new(),
    };
    let mut offered: Vec<(String, Vec<u8>)> = Vec::new();
    let mut costs: Vec<f64> = Vec::new();

    for _ in 0..formulas {
        let Some(shape) = shape::discover(&mut rng, family) else {
            continue;
        };
        report.shapes += 1;
        report.bounded_shapes += usize::from(shape.survey.bounded);
        let seed = rng.next_u64() as u32;
        for (view, viewpoint) in shape::viewpoints(&mut rng, &shape, &tuning.scouting)
            .iter()
            .enumerate()
        {
            let config = shape::thumbnail_config(
                &shape,
                viewpoint,
                THUMBNAIL_WIDTH,
                THUMBNAIL_HEIGHT,
                seed,
                &tuning.scouting,
            );
            match &mut renderer {
                Some(renderer) => pollster::block_on(renderer.replace_fractal(config.clone()))
                    .context("the generated formula does not compile to a shader")?,
                None => renderer = Some(gpu.renderer(config.clone())?),
            }
            let renderer = renderer.as_ref().expect("renderer was just initialised");
            // The first render of a shader includes its compilation.
            report.gpu_seconds += renderer
                .render_frame_with_config(&config, 0, 0.0)?
                .gpu_time()
                .as_secs_f64();
            let image = renderer.render_frame_with_config(&config, 0, 0.0)?;
            let seconds = image.gpu_time().as_secs_f64();
            report.gpu_seconds += seconds;
            report.viewpoints += 1;

            let measured = legibility(&Frame::from_rgba8(
                image.width(),
                image.height(),
                image.pixels(),
            )?);
            let estimate = seconds * scale_to_final;
            match screen(&measured, estimate, cost_ceiling, &tuning.legibility) {
                Some(reason) => *report.rejected.entry(reason).or_default() += 1,
                None => {
                    costs.push(estimate);
                    offered.push((
                        format!("{}-{}", report.shapes, view + 1),
                        image.pixels().to_vec(),
                    ));
                }
            }
        }
    }

    report.offered = offered.len();
    costs.sort_by(f64::total_cmp);
    report.median_frame_seconds = costs.get(costs.len() / 2).copied();
    report.slowest_frame_seconds = costs.last().copied();
    for (page, chunk) in offered.chunks(SHEET_COLUMNS * SHEET_ROWS).enumerate() {
        let path = directory.join(format!("{}-{:02}.png", family.as_str(), page + 1));
        let cells: Vec<Cell<'_>> = chunk
            .iter()
            .map(|(label, rgba)| Cell { label, rgba })
            .collect();
        sheet::compose(
            &cells,
            SHEET_COLUMNS,
            THUMBNAIL_WIDTH as usize,
            THUMBNAIL_HEIGHT as usize,
        )?
        .save_png(&path)?;
        report.sheets.push(path.to_string_lossy().into_owned());
    }
    report.shortfalls = shortfalls(&report, &tuning.vocabulary);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sampled(shapes: usize, viewpoints: usize, offered: usize) -> SampleReport {
        SampleReport {
            family: FormulaFamily::Mandelbox,
            formulas_tried: 20,
            shapes,
            bounded_shapes: shapes,
            viewpoints,
            offered,
            rejected: BTreeMap::new(),
            median_frame_seconds: None,
            slowest_frame_seconds: None,
            sheets: Vec::new(),
            gpu_seconds: 0.0,
            shortfalls: Vec::new(),
        }
    }

    #[test]
    fn yield_is_judged_against_the_rules() {
        let rules = VocabularyRules::default();
        assert!(shortfalls(&sampled(10, 30, 15), &rules).is_empty());
        // Exactly at both limits still passes.
        assert!(shortfalls(&sampled(4, 10, 3), &rules).is_empty());

        let few_shapes = shortfalls(&sampled(3, 9, 6), &rules);
        assert_eq!(few_shapes.len(), 1);
        assert!(
            few_shapes[0].contains("15% of the formulas"),
            "{few_shapes:?}"
        );
        let few_views = shortfalls(&sampled(10, 30, 6), &rules);
        assert!(few_views[0].contains("20% of the views"), "{few_views:?}");
        assert_eq!(shortfalls(&sampled(0, 0, 0), &rules).len(), 2);

        let lenient = VocabularyRules {
            min_shape_yield: 0.1,
            min_offered_share: 0.1,
        };
        assert!(shortfalls(&sampled(3, 9, 1), &lenient).is_empty());
    }

    #[test]
    fn an_empty_sample_is_refused_before_the_gpu_is_touched() {
        let directory = std::env::temp_dir().join("fractal-harness-sample-unused");
        let error = family(
            FormulaFamily::Mandelbox,
            0,
            1,
            &directory,
            &GpuOptions::default(),
            &Tuning::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("at least one formula"));
        assert!(!directory.exists());
    }
}
