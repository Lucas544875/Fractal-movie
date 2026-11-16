//! Every judgement value of the harness in one place: thresholds, limits,
//! windows, and distances that are policy rather than mechanism.
//!
//! A production snapshots its tuning when it is created, so changing the
//! defaults or the tuning file never alters the rules of a film that is
//! already under way. Fields missing from a tuning file keep their default.

use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Tuning file read by `init` when none is named explicitly.
pub const DEFAULT_TUNING_FILE: &str = "harness-tuning.yaml";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Tuning {
    pub diversity: Diversity,
    pub limits: Limits,
    pub scouting: Scouting,
    pub legibility: LegibilityLimits,
    pub classification: Classification,
    pub shotlist: ShotlistRules,
    pub storyboard: StoryboardRules,
    pub look: LookRules,
    pub proof: ProofRules,
    pub encode: EncodeRules,
    pub vocabulary: VocabularyRules,
    pub quality: QualityRules,
}

/// Rules that keep a new film away from the portfolio.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Diversity {
    /// Most recent films whose formula family may not be reused.
    pub family_ban_window: usize,
    /// Most recent films a new film is compared against.
    pub axis_diff_window: usize,
    /// Axes on which a new film must differ from each compared film.
    pub min_differing_axes: usize,
    /// Axes whose value the harness prescribes.
    pub assigned_axes: usize,
    /// Structure distance below which a scouted view repeats a keyframe of
    /// a finished film.
    pub min_structure_distance: f32,
}

impl Default for Diversity {
    fn default() -> Self {
        Self {
            family_ban_window: 3,
            axis_diff_window: 5,
            min_differing_axes: 3,
            assigned_axes: 2,
            min_structure_distance: 0.10,
        }
    }
}

/// How often a production may go back before the harness moves on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    /// Critic `revise` verdicts accepted per stage.
    pub revisions_per_stage: u32,
    /// Revisions, including returns to earlier stages, accepted over all
    /// design stages of one concept. A new concept after a kill starts
    /// afresh.
    pub revisions_per_concept: u32,
    /// Times a production may be sent back to scouting.
    pub kills: u32,
    /// Times an unrealisable mandate assignment is replaced.
    pub mandate_redraws: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            revisions_per_stage: 2,
            revisions_per_concept: 4,
            kills: 2,
            mandate_redraws: 2,
        }
    }
}

/// How locations are sampled, framed, and offered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scouting {
    /// Shapes rendered by a round when no count is given.
    pub default_shapes: usize,
    pub min_selected: usize,
    pub max_selected: usize,
    /// Camera distance of an overview, in radii of the shape.
    pub overview_distance_radii: f64,
    /// Camera clearance of a mid-scale view, relative to the shape's
    /// working radius: lowest and highest.
    pub mid_clearance: [f64; 2],
    /// Camera clearance of a close-up, likewise.
    pub deep_clearance: [f64; 2],
    /// Subject distance the camera is turned towards, in clearances.
    pub subject_distance_clearances: f64,
    pub field_of_view_degrees: f32,
    /// Share of sky mixed into the subject by scouting fog, as an optical
    /// depth.
    pub subject_fog: f64,
    /// Samples per pixel assumed when extrapolating a thumbnail's render
    /// time to a final frame.
    pub estimate_samples_per_pixel: f64,
    /// Share of the budget's longest permitted frame time above which a
    /// viewpoint is too slow to offer.
    pub cost_ceiling_share: f64,
    /// Relative parameter change when sampling around a location.
    pub refine_amount: f64,
}

impl Default for Scouting {
    fn default() -> Self {
        Self {
            default_shapes: 60,
            min_selected: 2,
            max_selected: 5,
            overview_distance_radii: 2.6,
            mid_clearance: [0.08, 0.4],
            deep_clearance: [0.004, 0.03],
            subject_distance_clearances: 6.0,
            field_of_view_degrees: 40.0,
            subject_fog: 0.16,
            estimate_samples_per_pixel: 8.0,
            cost_ceiling_share: 0.5,
            refine_amount: 0.12,
        }
    }
}

/// What a frame must measure to count as readable at thumbnail size.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LegibilityLimits {
    /// Below this share of flat area a frame is wall-to-wall detail.
    pub min_negative_space: f32,
    /// Above this share there is no subject to look at.
    pub max_negative_space: f32,
    pub max_high_frequency: f32,
    pub min_mean_luminance: f32,
    /// Frame-to-frame instability above which a cut shimmers.
    pub max_flicker: f32,
}

impl Default for LegibilityLimits {
    fn default() -> Self {
        Self {
            min_negative_space: 0.10,
            max_negative_space: 0.85,
            max_high_frequency: 0.35,
            min_mean_luminance: 0.03,
            max_flicker: 0.02,
        }
    }
}

/// Boundaries between the values of each axis.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Classification {
    /// Camera clearance, relative to the working radius, below which a view
    /// is a close-up.
    pub deep_clearance: f64,
    /// Distance from the shape's centre, in radii, from which a bounded
    /// shape is seen from outside.
    pub overview_radii: f64,
    /// Cosine between the light and the direction to the camera above
    /// which the light sits behind the camera.
    pub frontal_cosine: f64,
    /// Cosine below which the light faces the camera.
    pub backlit_cosine: f64,
    pub top_cosine: f64,
    /// Mean cut length, in seconds, from which a film is slow paced.
    pub slow_cut_seconds: f64,
    pub medium_cut_seconds: f64,
    /// Share of pixels that must carry a hue before a palette is not
    /// neutral.
    pub min_coloured_share: f32,
    pub low_key_luminance: f32,
    pub high_key_luminance: f32,
}

impl Default for Classification {
    fn default() -> Self {
        Self {
            deep_clearance: 0.05,
            overview_radii: 1.5,
            frontal_cosine: 0.75,
            backlit_cosine: -0.2,
            top_cosine: 0.6,
            slow_cut_seconds: 12.0,
            medium_cut_seconds: 7.0,
            min_coloured_share: 0.25,
            low_key_luminance: 0.25,
            high_key_luminance: 0.55,
        }
    }
}

/// Rules a shot list is held to.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShotlistRules {
    /// Length of a fade or crossfade between cuts.
    pub transition_seconds: f64,
    /// Shortest cut that may carry a fade or crossfade.
    pub min_transition_cut_seconds: f64,
    pub min_cut_seconds: f64,
    pub max_cut_seconds: f64,
    /// Properties out of role, scale regime, and motion in which
    /// consecutive cuts must differ.
    pub min_cut_differences: usize,
    /// Smallest ratio of camera clearance to subject distance along a
    /// route.
    pub min_clearance_ratio: f64,
}

impl Default for ShotlistRules {
    fn default() -> Self {
        Self {
            transition_seconds: 0.5,
            min_transition_cut_seconds: 2.0,
            min_cut_seconds: 1.0,
            max_cut_seconds: 60.0,
            min_cut_differences: 2,
            min_clearance_ratio: 0.02,
        }
    }
}

/// How the beats of a brief are checked and drawn.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StoryboardRules {
    /// Duration a beat's motion is planned with before the shot list fixes
    /// real durations.
    pub beat_seconds: f64,
    /// Width of storyboard frames.
    pub width: u32,
    pub samples_per_pixel: u32,
    /// Fewest beats a brief may have; the most is the film's cut limit.
    pub min_beats: usize,
}

impl Default for StoryboardRules {
    fn default() -> Self {
        Self {
            beat_seconds: 8.0,
            width: 320,
            samples_per_pixel: 2,
            min_beats: 2,
        }
    }
}

/// How a look is applied, previewed, and judged.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LookRules {
    /// Width of look preview frames.
    pub preview_width: u32,
    pub preview_samples_per_pixel: u32,
    /// Steps requested effects are previewed with.
    pub preview_effect_steps: u32,
    /// Most hue clusters a frame may show.
    pub max_hue_clusters: u32,
    /// Largest share of a frame that may be blown out or crushed.
    pub max_clipped_high: f32,
    pub max_clipped_low: f32,
    /// Ambient-occlusion radius, in subject distances.
    pub occlusion_radius_ratio: f64,
    /// Reach of shadow and reflection rays, in subject distances.
    pub shadow_distance_ratio: f64,
    pub reflection_distance_ratio: f64,
    /// Lens radius at full depth-of-field strength, in subject distances.
    pub aperture_ratio: f64,
    /// Steps a requested effect starts from before the quality decision
    /// reduces them.
    pub occlusion_steps: u32,
    pub shadow_steps: u32,
    pub reflection_steps: u32,
}

impl Default for LookRules {
    fn default() -> Self {
        Self {
            preview_width: 480,
            preview_samples_per_pixel: 8,
            preview_effect_steps: 24,
            max_hue_clusters: 3,
            max_clipped_high: 0.02,
            max_clipped_low: 0.6,
            occlusion_radius_ratio: 0.2,
            shadow_distance_ratio: 3.0,
            reflection_distance_ratio: 3.0,
            aperture_ratio: 0.01,
            occlusion_steps: 64,
            shadow_steps: 96,
            reflection_steps: 48,
        }
    }
}

/// How the final quality is measured and a proof clip is made.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProofRules {
    /// Side of the centred region rendered to measure quality and time, as
    /// a fraction of the frame.
    pub region_fraction: f64,
    /// Width of the proof clip.
    pub clip_width: u32,
    /// Seconds of each cut, around its middle, shown in the proof clip.
    pub clip_seconds_per_cut: f64,
    /// Bitrate a delivery platform is assumed to give a 1920x1080 stream;
    /// scaled by pixel count for other sizes.
    pub delivery_kbps_1080p: u32,
    /// Lowest structural similarity between the proof clip and its
    /// delivery-bitrate encode.
    pub min_delivery_ssim: f32,
    /// Every n-th frame is compared after compression.
    pub compare_every: u32,
}

impl Default for ProofRules {
    fn default() -> Self {
        Self {
            region_fraction: 0.5,
            clip_width: 960,
            clip_seconds_per_cut: 1.0,
            delivery_kbps_1080p: 8000,
            min_delivery_ssim: 0.90,
            compare_every: 3,
        }
    }
}

/// How the final file is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EncodeRules {
    /// Constant rate factor of the master file; lower is better and larger.
    /// The delivery platform transcodes from this file.
    pub master_crf: u32,
}

impl Default for EncodeRules {
    fn default() -> Self {
        Self { master_crf: 16 }
    }
}

/// What new shape vocabulary must show before it is offered to directors.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VocabularyRules {
    /// Smallest share of generated formulas that must survey as a usable
    /// shape.
    pub min_shape_yield: f64,
    /// Smallest share of rendered views that must pass screening.
    pub min_offered_share: f64,
}

impl Default for VocabularyRules {
    fn default() -> Self {
        Self {
            min_shape_yield: 0.2,
            min_offered_share: 0.3,
        }
    }
}

/// How the render quality of a cut is decided.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QualityRules {
    /// Sample count of the converged reference.
    pub reference_spp: u32,
    /// Sample count used while comparing effects.
    pub ablation_spp: u32,
    /// Candidate final sample counts, cheapest first.
    pub spp_ladder: Vec<u32>,
    /// Lowest sample count budget fitting may fall back to.
    pub min_spp: u32,
    /// Fewest steps a secondary ray effect is tried with.
    pub min_effect_steps: u32,
    /// Multiples of the look's hit precision that are tried, in order.
    pub epsilon_ladder: Vec<f32>,
    /// An effect whose removal changes the image by less than this is
    /// dropped.
    pub effect_min_difference: f32,
    /// Reducing an effect's steps may change the image by at most this.
    pub step_max_difference: f32,
    /// A sample count is accepted when it is this close to the reference.
    pub spp_max_difference: f32,
    /// Coarser hit precision is accepted while the image stays this close.
    pub epsilon_max_difference: f32,
}

impl Default for QualityRules {
    fn default() -> Self {
        Self {
            reference_spp: 64,
            ablation_spp: 16,
            spp_ladder: vec![4, 8, 16, 32],
            min_spp: 2,
            min_effect_steps: 4,
            epsilon_ladder: vec![1.5, 2.0, 3.0],
            effect_min_difference: 0.02,
            step_max_difference: 0.01,
            spp_max_difference: 0.02,
            epsilon_max_difference: 0.02,
        }
    }
}

impl Tuning {
    /// Loads a tuning file. Fields it omits keep their defaults.
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("could not read tuning file {}", path.display()))?;
        let tuning: Self = serde_yaml_ng::from_str(&text)
            .with_context(|| format!("invalid tuning file {}", path.display()))?;
        tuning
            .validate()
            .with_context(|| format!("invalid tuning file {}", path.display()))?;
        Ok(tuning)
    }

    /// Rejects values under which the harness could not reach a decision.
    pub fn validate(&self) -> Result<()> {
        let diversity = &self.diversity;
        if diversity.min_differing_axes > 6 || diversity.assigned_axes > 6 {
            bail!("diversity counts cannot exceed the six axes");
        }
        if !diversity.min_structure_distance.is_finite()
            || !(0.0..=1.0).contains(&diversity.min_structure_distance)
        {
            bail!("diversity min_structure_distance must be in 0..=1");
        }

        let scouting = &self.scouting;
        if scouting.default_shapes == 0
            || scouting.min_selected == 0
            || scouting.max_selected < scouting.min_selected
        {
            bail!("scouting counts must be positive and ordered");
        }
        for (name, [low, high]) in [
            ("mid_clearance", scouting.mid_clearance),
            ("deep_clearance", scouting.deep_clearance),
        ] {
            if !(low.is_finite() && high.is_finite() && 0.0 < low && low < high) {
                bail!("scouting {name} must be an increasing pair of positive numbers");
            }
        }
        let positive = [
            (
                "scouting overview_distance_radii",
                scouting.overview_distance_radii - 1.0,
            ),
            (
                "scouting subject_distance_clearances",
                scouting.subject_distance_clearances - 1.0,
            ),
            ("scouting subject_fog", scouting.subject_fog),
            (
                "scouting estimate_samples_per_pixel",
                scouting.estimate_samples_per_pixel,
            ),
            ("scouting cost_ceiling_share", scouting.cost_ceiling_share),
            ("scouting refine_amount", scouting.refine_amount),
            (
                "shotlist transition_seconds",
                self.shotlist.transition_seconds,
            ),
            ("shotlist min_cut_seconds", self.shotlist.min_cut_seconds),
            (
                "shotlist max_cut_seconds",
                self.shotlist.max_cut_seconds - self.shotlist.min_cut_seconds,
            ),
            (
                "shotlist min_transition_cut_seconds",
                self.shotlist.min_transition_cut_seconds - 2.0 * self.shotlist.transition_seconds
                    + f64::EPSILON,
            ),
            (
                "shotlist min_clearance_ratio",
                self.shotlist.min_clearance_ratio,
            ),
            (
                "classification slow_cut_seconds",
                self.classification.slow_cut_seconds - self.classification.medium_cut_seconds,
            ),
            (
                "classification medium_cut_seconds",
                self.classification.medium_cut_seconds,
            ),
            (
                "classification deep_clearance",
                self.classification.deep_clearance,
            ),
            (
                "classification overview_radii",
                self.classification.overview_radii - 1.0,
            ),
        ];
        for (name, value) in positive {
            if !value.is_finite() || value <= 0.0 {
                bail!("{name} is out of range");
            }
        }
        if !(1.0..179.0).contains(&scouting.field_of_view_degrees) {
            bail!("scouting field_of_view_degrees must be in 1..179");
        }
        if self.shotlist.min_cut_differences > 3 {
            bail!("shotlist min_cut_differences cannot exceed the three compared properties");
        }

        let legibility = &self.legibility;
        if !(0.0 <= legibility.min_negative_space
            && legibility.min_negative_space < legibility.max_negative_space
            && legibility.max_negative_space <= 1.0)
        {
            bail!("legibility negative-space limits must be ordered within 0..=1");
        }

        let storyboard = &self.storyboard;
        if !(storyboard.beat_seconds.is_finite()
            && (self.shotlist.min_cut_seconds..=self.shotlist.max_cut_seconds)
                .contains(&storyboard.beat_seconds))
            || storyboard.width < 64
            || storyboard.samples_per_pixel == 0
            || storyboard.samples_per_pixel > fractal_renderer_core::MAX_SAMPLES_PER_PIXEL
            || storyboard.min_beats == 0
        {
            bail!("storyboard settings are out of range; beat_seconds must be a valid cut length");
        }

        let look = &self.look;
        let steps_limit = fractal_renderer_core::MAX_SECONDARY_RAY_STEPS;
        if look.preview_width < 64
            || look.preview_samples_per_pixel == 0
            || look.preview_samples_per_pixel > fractal_renderer_core::MAX_SAMPLES_PER_PIXEL
        {
            bail!("look preview size and sample count are out of range");
        }
        if [
            look.preview_effect_steps,
            look.occlusion_steps,
            look.shadow_steps,
            look.reflection_steps,
        ]
        .iter()
        .any(|steps| *steps == 0 || *steps > steps_limit)
        {
            bail!("look effect steps must be in 1..={steps_limit}");
        }
        for (name, value) in [
            ("occlusion_radius_ratio", look.occlusion_radius_ratio),
            ("shadow_distance_ratio", look.shadow_distance_ratio),
            ("reflection_distance_ratio", look.reflection_distance_ratio),
            ("aperture_ratio", look.aperture_ratio),
        ] {
            if !value.is_finite() || value <= 0.0 {
                bail!("look {name} must be positive");
            }
        }
        for (name, value) in [
            ("max_clipped_high", look.max_clipped_high),
            ("max_clipped_low", look.max_clipped_low),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                bail!("look {name} must be in 0..=1");
            }
        }

        let proof = &self.proof;
        if !proof.region_fraction.is_finite() || !(0.05..=1.0).contains(&proof.region_fraction) {
            bail!("proof region_fraction must be in 0.05..=1");
        }
        if proof.clip_width < 64
            || !proof.clip_seconds_per_cut.is_finite()
            || proof.clip_seconds_per_cut <= 0.0
            || proof.delivery_kbps_1080p == 0
            || proof.compare_every == 0
            || !(0.0..=1.0).contains(&proof.min_delivery_ssim)
        {
            bail!("proof clip settings are out of range");
        }

        for (name, value) in [
            ("min_shape_yield", self.vocabulary.min_shape_yield),
            ("min_offered_share", self.vocabulary.min_offered_share),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                bail!("vocabulary {name} must be in 0..=1");
            }
        }
        if self.encode.master_crf > 51 {
            bail!("encode master_crf must be in 0..=51");
        }

        let quality = &self.quality;
        if quality.spp_ladder.is_empty()
            || quality.spp_ladder.windows(2).any(|pair| pair[0] >= pair[1])
            || quality.spp_ladder[0] == 0
        {
            bail!("quality spp_ladder must be a non-empty increasing list of positive counts");
        }
        let top = quality.spp_ladder[quality.spp_ladder.len() - 1];
        if quality.reference_spp <= top
            || quality.reference_spp > fractal_renderer_core::MAX_SAMPLES_PER_PIXEL
        {
            bail!(
                "quality reference_spp must exceed the ladder's top ({top}) and not exceed {}",
                fractal_renderer_core::MAX_SAMPLES_PER_PIXEL
            );
        }
        if quality.min_spp == 0 || quality.ablation_spp == 0 || quality.min_effect_steps == 0 {
            bail!("quality sample and step counts must be positive");
        }
        if quality
            .epsilon_ladder
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
            || quality.epsilon_ladder.iter().any(|factor| *factor <= 1.0)
        {
            bail!("quality epsilon_ladder must be an increasing list of factors above 1");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_round_trip() {
        let tuning = Tuning::default();
        tuning.validate().unwrap();
        let yaml = serde_yaml_ng::to_string(&tuning).unwrap();
        assert_eq!(serde_yaml_ng::from_str::<Tuning>(&yaml).unwrap(), tuning);
        let json = serde_json::to_string(&tuning).unwrap();
        assert_eq!(serde_json::from_str::<Tuning>(&json).unwrap(), tuning);
    }

    #[test]
    fn a_file_overrides_only_what_it_names() {
        let directory =
            std::env::temp_dir().join(format!("fractal-harness-tuning-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("tuning.yaml");
        fs::write(
            &path,
            "scouting:\n  overview_distance_radii: 1.8\nlegibility:\n  max_flicker: 0.05\n",
        )
        .unwrap();
        let tuning = Tuning::load(&path).unwrap();
        assert_eq!(tuning.scouting.overview_distance_radii, 1.8);
        assert_eq!(tuning.legibility.max_flicker, 0.05);
        // Everything else is untouched.
        assert_eq!(tuning.scouting.default_shapes, 60);
        assert_eq!(tuning.limits, Limits::default());
        assert_eq!(tuning.quality, QualityRules::default());

        // Misspelt names must not be silently ignored.
        fs::write(&path, "scouting:\n  overview_distance: 1.8\n").unwrap();
        assert!(Tuning::load(&path).is_err());
        fs::write(&path, "legibilty:\n  max_flicker: 0.05\n").unwrap();
        assert!(Tuning::load(&path).is_err());
        assert!(Tuning::load(&directory.join("missing.yaml")).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unusable_values_are_rejected() {
        for edit in [
            (|tuning: &mut Tuning| tuning.diversity.min_differing_axes = 7) as fn(&mut Tuning),
            |tuning| tuning.diversity.min_structure_distance = 1.5,
            |tuning| tuning.scouting.max_selected = 1,
            |tuning| tuning.scouting.mid_clearance = [0.4, 0.08],
            |tuning| tuning.scouting.overview_distance_radii = 0.9,
            |tuning| tuning.scouting.field_of_view_degrees = 200.0,
            |tuning| tuning.legibility.min_negative_space = 0.9,
            |tuning| tuning.classification.medium_cut_seconds = 20.0,
            |tuning| tuning.shotlist.min_cut_differences = 4,
            |tuning| tuning.shotlist.min_transition_cut_seconds = 0.5,
            |tuning| tuning.storyboard.beat_seconds = 600.0,
            |tuning| tuning.storyboard.min_beats = 0,
            |tuning| tuning.look.preview_effect_steps = 0,
            |tuning| tuning.look.aperture_ratio = 0.0,
            |tuning| tuning.look.max_clipped_high = 2.0,
            |tuning| tuning.proof.region_fraction = 0.0,
            |tuning| tuning.proof.compare_every = 0,
            |tuning| tuning.vocabulary.min_shape_yield = 1.5,
            |tuning| tuning.encode.master_crf = 60,
            |tuning| tuning.quality.spp_ladder = vec![8, 4],
            |tuning| tuning.quality.spp_ladder = Vec::new(),
            |tuning| tuning.quality.reference_spp = 32,
            |tuning| tuning.quality.epsilon_ladder = vec![0.5],
        ] {
            let mut tuning = Tuning::default();
            edit(&mut tuning);
            assert!(tuning.validate().is_err(), "{tuning:?}");
        }
    }
}
