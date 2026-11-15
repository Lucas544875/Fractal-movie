//! Chooses the render quality of a cut. Agents may request effects, but the
//! harness decides what is rendered: an effect is kept only when removing it
//! is visible, and sampling is set to the cheapest level that still matches a
//! converged reference.

use anyhow::{Result, bail};
use serde::Serialize;

use crate::{
    metrics::{Frame, difference},
    tuning::QualityRules,
};

/// Differences are judged on frames reduced by this factor, so single-pixel
/// noise that neither a viewer nor a codec preserves does not drive quality.
const COMPARISON_REDUCTION: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Effect {
    AmbientOcclusion,
    SoftShadow,
    Reflection,
    DepthOfField,
}

impl Effect {
    pub const ALL: [Self; 4] = [
        Self::AmbientOcclusion,
        Self::SoftShadow,
        Self::Reflection,
        Self::DepthOfField,
    ];
}

/// Quality-relevant values a render is performed with.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Settings {
    pub samples_per_pixel: u32,
    /// Zero disables the effect.
    pub ambient_occlusion_steps: u32,
    pub soft_shadow_steps: u32,
    pub reflection_steps: u32,
    pub depth_of_field: bool,
    pub pixel_epsilon_multiplier: f32,
}

impl Settings {
    fn steps_mut(&mut self, effect: Effect) -> Option<&mut u32> {
        match effect {
            Effect::AmbientOcclusion => Some(&mut self.ambient_occlusion_steps),
            Effect::SoftShadow => Some(&mut self.soft_shadow_steps),
            Effect::Reflection => Some(&mut self.reflection_steps),
            Effect::DepthOfField => None,
        }
    }

    /// Steps of a ray effect, or `None` for depth of field.
    #[must_use]
    pub const fn steps(&self, effect: Effect) -> Option<u32> {
        match effect {
            Effect::AmbientOcclusion => Some(self.ambient_occlusion_steps),
            Effect::SoftShadow => Some(self.soft_shadow_steps),
            Effect::Reflection => Some(self.reflection_steps),
            Effect::DepthOfField => None,
        }
    }

    #[must_use]
    pub const fn enabled(&self, effect: Effect) -> bool {
        match self.steps(effect) {
            Some(steps) => steps > 0,
            None => self.depth_of_field,
        }
    }

    fn without(mut self, effect: Effect) -> Self {
        match self.steps_mut(effect) {
            Some(steps) => *steps = 0,
            None => self.depth_of_field = false,
        }
        self
    }

    fn with_steps(mut self, effect: Effect, value: u32) -> Self {
        if let Some(steps) = self.steps_mut(effect) {
            *steps = value;
        }
        self
    }

    fn with_spp(mut self, samples_per_pixel: u32) -> Self {
        self.samples_per_pixel = samples_per_pixel;
        self
    }
}

/// One measured render of a probe frame.
#[derive(Clone, Debug)]
pub struct Sample {
    pub frame: Frame,
    /// GPU time actually spent on this render.
    pub gpu_seconds: f64,
    /// GPU time a complete final frame would take with these settings.
    pub frame_seconds: f64,
}

/// Renders probe frames of one cut with arbitrary quality settings.
pub trait Probe {
    fn render(&mut self, frame_index: u32, settings: &Settings) -> Result<Sample>;
}

/// GPU time a final frame may take.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct FrameLimits {
    pub mean_frame_seconds: f64,
    pub max_frame_seconds: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct EffectDecision {
    pub effect: Effect,
    /// Steps the look asked for; `None` for depth of field.
    pub requested_steps: Option<u32>,
    /// How much the image changes when the effect is removed.
    pub visible_difference: f32,
    pub kept: bool,
    /// Steps the effect is rendered with when kept.
    pub steps: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Trial<T> {
    pub value: T,
    pub difference: f32,
    pub accepted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Degradation {
    DroppedEffect {
        effect: Effect,
        visible_difference: f32,
    },
    ReducedSamples {
        from: u32,
        to: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct FrameTime {
    pub frame: u32,
    pub seconds: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Estimate {
    pub mean_frame_seconds: f64,
    pub max_frame_seconds: f64,
    pub frames: Vec<FrameTime>,
}

impl Estimate {
    #[must_use]
    pub fn fits(&self, limits: &FrameLimits) -> bool {
        self.mean_frame_seconds <= limits.mean_frame_seconds
            && self.max_frame_seconds <= limits.max_frame_seconds
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Decision {
    pub settings: Settings,
    pub effects: Vec<EffectDecision>,
    pub sample_trials: Vec<Trial<u32>>,
    pub epsilon_trials: Vec<Trial<f32>>,
    /// Quality given up, in order, to bring the estimate inside the budget.
    pub degradations: Vec<Degradation>,
    pub estimate: Estimate,
    pub limits: FrameLimits,
    pub fits_budget: bool,
    /// GPU time the decision itself consumed, charged to the design budget.
    pub gpu_seconds_spent: f64,
    pub renders: u32,
}

/// Decides the quality settings of one cut.
///
/// `requested` carries the look's wishes: effect steps (zero when an effect
/// was not requested), depth of field, and the base hit precision. Its sample
/// count is ignored.
pub fn decide(
    probe: &mut dyn Probe,
    frames: &[u32],
    requested: &Settings,
    limits: &FrameLimits,
    rules: &QualityRules,
) -> Result<Decision> {
    if frames.is_empty() {
        bail!("quality decision needs at least one probe frame");
    }
    let Some(top_spp) = rules.spp_ladder.last().copied() else {
        bail!("quality decision needs a sample ladder");
    };
    let mut session = Session {
        probe,
        frames,
        cache: Vec::new(),
        gpu_seconds_spent: 0.0,
        renders: 0,
    };

    let mut current = requested.with_spp(rules.ablation_spp);
    let mut effects = Vec::new();
    for effect in Effect::ALL {
        if !requested.enabled(effect) {
            continue;
        }
        let requested_steps = requested.steps(effect);
        let visible_difference = session.difference(&current, &current.without(effect))?;
        if visible_difference < rules.effect_min_difference {
            current = current.without(effect);
            effects.push(EffectDecision {
                effect,
                requested_steps,
                visible_difference,
                kept: false,
                steps: None,
            });
            continue;
        }
        if let Some(full_steps) = requested_steps {
            for steps in reduced_steps(full_steps, rules.min_effect_steps) {
                let candidate = current.with_steps(effect, steps);
                if session.difference(&current, &candidate)? < rules.step_max_difference {
                    current = candidate;
                    break;
                }
            }
        }
        effects.push(EffectDecision {
            effect,
            requested_steps,
            visible_difference,
            kept: true,
            steps: current.steps(effect),
        });
    }

    let reference = current.with_spp(rules.reference_spp);
    let mut sample_trials = Vec::new();
    let mut chosen_spp = top_spp;
    for spp in rules.spp_ladder.iter().copied() {
        let difference = session.difference(&reference, &current.with_spp(spp))?;
        let accepted = difference < rules.spp_max_difference;
        sample_trials.push(Trial {
            value: spp,
            difference,
            accepted,
        });
        if accepted {
            chosen_spp = spp;
            break;
        }
    }
    current = current.with_spp(chosen_spp);

    // A fixed (non-projected) hit precision has no multiplier to coarsen.
    let mut epsilon_trials = Vec::new();
    if requested.pixel_epsilon_multiplier > 0.0 {
        let base = current;
        for factor in rules.epsilon_ladder.iter().copied() {
            let mut candidate = base;
            candidate.pixel_epsilon_multiplier = requested.pixel_epsilon_multiplier * factor;
            let difference = session.difference(&base, &candidate)?;
            let accepted = difference < rules.epsilon_max_difference;
            epsilon_trials.push(Trial {
                value: candidate.pixel_epsilon_multiplier,
                difference,
                accepted,
            });
            if !accepted {
                break;
            }
            current = candidate;
        }
    }

    let mut estimate = session.estimate(&current)?;
    let mut degradations = Vec::new();
    while !estimate.fits(limits) {
        // Give up whatever is least visible: an effect, or one halving of
        // the sample count. A halving is ranked by how far that count was
        // from the reference; counts the ladder never tried rank last, so
        // they are used only once no effect is left.
        let least_visible = effects
            .iter_mut()
            .filter(|decision| decision.kept)
            .min_by(|a, b| a.visible_difference.total_cmp(&b.visible_difference));
        let halved = (current.samples_per_pixel > rules.min_spp)
            .then(|| (current.samples_per_pixel / 2).max(rules.min_spp));
        let halving_difference = halved.map(|to| {
            sample_trials
                .iter()
                .find(|trial| trial.value == to)
                .map_or(f32::INFINITY, |trial| trial.difference)
        });
        match (least_visible, halved, halving_difference) {
            (Some(decision), _, halving)
                if halving.is_none_or(|halving| decision.visible_difference <= halving) =>
            {
                decision.kept = false;
                decision.steps = None;
                current = current.without(decision.effect);
                degradations.push(Degradation::DroppedEffect {
                    effect: decision.effect,
                    visible_difference: decision.visible_difference,
                });
            }
            (_, Some(to), _) => {
                let from = current.samples_per_pixel;
                current = current.with_spp(to);
                degradations.push(Degradation::ReducedSamples { from, to });
            }
            _ => break,
        }
        estimate = session.estimate(&current)?;
    }

    Ok(Decision {
        settings: current,
        effects,
        sample_trials,
        epsilon_trials,
        degradations,
        fits_budget: estimate.fits(limits),
        estimate,
        limits: *limits,
        gpu_seconds_spent: session.gpu_seconds_spent,
        renders: session.renders,
    })
}

/// Step counts below `full` worth trying, cheapest first.
fn reduced_steps(full: u32, min_steps: u32) -> Vec<u32> {
    let mut steps: Vec<u32> = [full / 4, full / 2]
        .into_iter()
        .map(|value| value.max(min_steps))
        .filter(|value| *value < full)
        .collect();
    steps.dedup();
    steps
}

struct Session<'a> {
    probe: &'a mut dyn Probe,
    frames: &'a [u32],
    /// Renders are expensive and the ladders revisit settings, so every
    /// result is kept for the duration of one decision.
    cache: Vec<(Settings, Vec<Sample>)>,
    gpu_seconds_spent: f64,
    renders: u32,
}

impl Session<'_> {
    fn samples(&mut self, settings: &Settings) -> Result<usize> {
        if let Some(index) = self.cache.iter().position(|(cached, _)| cached == settings) {
            return Ok(index);
        }
        let mut samples = Vec::with_capacity(self.frames.len());
        for frame in self.frames {
            let mut sample = self.probe.render(*frame, settings)?;
            self.gpu_seconds_spent += sample.gpu_seconds;
            self.renders += 1;
            sample.frame = sample.frame.reduced(COMPARISON_REDUCTION);
            samples.push(sample);
        }
        self.cache.push((*settings, samples));
        Ok(self.cache.len() - 1)
    }

    /// Largest difference between two settings over all probe frames.
    fn difference(&mut self, reference: &Settings, variant: &Settings) -> Result<f32> {
        let reference = self.samples(reference)?;
        let variant = self.samples(variant)?;
        let mut worst = 0.0f32;
        for (a, b) in self.cache[reference].1.iter().zip(&self.cache[variant].1) {
            worst = worst.max(difference(&a.frame, &b.frame)?.score());
        }
        Ok(worst)
    }

    fn estimate(&mut self, settings: &Settings) -> Result<Estimate> {
        let index = self.samples(settings)?;
        let frames: Vec<FrameTime> = self
            .frames
            .iter()
            .zip(&self.cache[index].1)
            .map(|(frame, sample)| FrameTime {
                frame: *frame,
                seconds: sample.frame_seconds,
            })
            .collect();
        let total: f64 = frames.iter().map(|frame| frame.seconds).sum();
        Ok(Estimate {
            mean_frame_seconds: total / frames.len() as f64,
            max_frame_seconds: frames.iter().map(|frame| frame.seconds).fold(0.0, f64::max),
            frames,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decides with the default rules.
    fn decide(
        probe: &mut dyn Probe,
        frames: &[u32],
        requested: &Settings,
        limits: &FrameLimits,
    ) -> Result<Decision> {
        super::decide(probe, frames, requested, limits, &QualityRules::default())
    }

    fn reduced_steps(full: u32) -> Vec<u32> {
        super::reduced_steps(full, QualityRules::default().min_effect_steps)
    }
    use crate::{metrics::tests::frame, rng::Rng};

    const SIZE: usize = 96;

    /// A renderer stand-in with known sensitivities, so each rule of the
    /// decision can be exercised without a GPU.
    struct Synthetic {
        /// Image contribution of each effect at full steps.
        ambient_occlusion: f32,
        soft_shadow: f32,
        reflection: f32,
        depth_of_field: f32,
        /// Noise amplitude at one sample per pixel.
        noise: f32,
        /// Detail lost per unit of hit-precision multiplier.
        epsilon_sensitivity: f32,
        /// Seconds per frame at one sample without effects.
        base_seconds: f64,
        full_steps: u32,
        calls: u32,
    }

    impl Synthetic {
        fn new() -> Self {
            Self {
                ambient_occlusion: 0.0,
                soft_shadow: 0.0,
                reflection: 0.0,
                depth_of_field: 0.0,
                noise: 0.0,
                epsilon_sensitivity: 0.0,
                base_seconds: 0.01,
                full_steps: 64,
                calls: 0,
            }
        }
    }

    impl Probe for Synthetic {
        fn render(&mut self, frame_index: u32, settings: &Settings) -> Result<Sample> {
            self.calls += 1;
            let saturating = |steps: u32| {
                // Quarter steps already deliver most of an effect.
                (steps as f32 / self.full_steps as f32 * 4.0).min(1.0)
            };
            let ambient_occlusion =
                self.ambient_occlusion * saturating(settings.ambient_occlusion_steps);
            let soft_shadow = self.soft_shadow * saturating(settings.soft_shadow_steps);
            let reflection = self.reflection * saturating(settings.reflection_steps);
            let depth_of_field = if settings.depth_of_field {
                self.depth_of_field
            } else {
                0.0
            };
            let noise = self.noise / (settings.samples_per_pixel as f32).sqrt();
            let softening = self.epsilon_sensitivity * settings.pixel_epsilon_multiplier;

            let mut rng = Rng::new(u64::from(frame_index));
            let grain: Vec<f32> = (0..SIZE * SIZE).map(|_| rng.unit() as f32 - 0.5).collect();
            let image = frame(SIZE, SIZE, |x, y| {
                let checker = ((x / 12 + y / 12) % 2) as f32;
                let stripes = ((x / 6) % 2) as f32;
                let rings = (((x + y) / 10) % 2) as f32;
                let value = 0.25 + 0.3 * checker * (1.0 - softening)
                    - ambient_occlusion * stripes
                    - soft_shadow * rings
                    + reflection * (1.0 - stripes)
                    + depth_of_field * (y as f32 / SIZE as f32)
                    + noise * grain[y * SIZE + x];
                [value.clamp(0.0, 1.0); 3]
            });

            let steps = settings.ambient_occlusion_steps
                + settings.soft_shadow_steps
                + settings.reflection_steps;
            let frame_seconds = self.base_seconds
                * f64::from(settings.samples_per_pixel)
                * (1.0 + f64::from(steps) / 64.0);
            Ok(Sample {
                frame: image,
                gpu_seconds: frame_seconds * 0.25,
                frame_seconds,
            })
        }
    }

    fn requested() -> Settings {
        Settings {
            samples_per_pixel: 1,
            ambient_occlusion_steps: 64,
            soft_shadow_steps: 64,
            reflection_steps: 0,
            depth_of_field: false,
            pixel_epsilon_multiplier: 1.0,
        }
    }

    const GENEROUS: FrameLimits = FrameLimits {
        mean_frame_seconds: 1.0e6,
        max_frame_seconds: 1.0e6,
    };

    fn effect(decision: &Decision, effect: Effect) -> EffectDecision {
        *decision
            .effects
            .iter()
            .find(|entry| entry.effect == effect)
            .unwrap()
    }

    #[test]
    fn invisible_effects_are_dropped_and_visible_ones_kept() {
        let mut probe = Synthetic::new();
        probe.ambient_occlusion = 0.15;
        probe.soft_shadow = 0.0005;
        let decision = decide(&mut probe, &[0, 5], &requested(), &GENEROUS).unwrap();

        let ambient_occlusion = effect(&decision, Effect::AmbientOcclusion);
        assert!(ambient_occlusion.kept);
        assert!(
            ambient_occlusion.visible_difference >= QualityRules::default().effect_min_difference
        );
        let soft_shadow = effect(&decision, Effect::SoftShadow);
        assert!(!soft_shadow.kept);
        assert_eq!(soft_shadow.steps, None);
        assert_eq!(decision.settings.soft_shadow_steps, 0);
        // Effects that were never requested are not evaluated.
        assert_eq!(decision.effects.len(), 2);
        assert_eq!(decision.settings.reflection_steps, 0);
        assert!(decision.degradations.is_empty());
    }

    #[test]
    fn kept_effects_use_the_fewest_steps_that_look_the_same() {
        let mut probe = Synthetic::new();
        probe.ambient_occlusion = 0.15;
        probe.soft_shadow = 0.15;
        let decision = decide(&mut probe, &[0], &requested(), &GENEROUS).unwrap();
        // The synthetic effect saturates at a quarter of the requested steps.
        assert_eq!(effect(&decision, Effect::AmbientOcclusion).steps, Some(16));
        assert_eq!(decision.settings.soft_shadow_steps, 16);
    }

    #[test]
    fn depth_of_field_is_decided_without_a_step_ladder() {
        let mut request = requested();
        request.ambient_occlusion_steps = 0;
        request.soft_shadow_steps = 0;
        request.depth_of_field = true;

        let mut visible = Synthetic::new();
        visible.depth_of_field = 0.3;
        let decision = decide(&mut visible, &[0], &request, &GENEROUS).unwrap();
        let entry = effect(&decision, Effect::DepthOfField);
        assert!(entry.kept && entry.requested_steps.is_none() && entry.steps.is_none());
        assert!(decision.settings.depth_of_field);

        let mut invisible = Synthetic::new();
        let decision = decide(&mut invisible, &[0], &request, &GENEROUS).unwrap();
        assert!(!decision.settings.depth_of_field);
    }

    #[test]
    fn sampling_stops_at_the_first_level_matching_the_reference() {
        let mut clean = Synthetic::new();
        let decision = decide(&mut clean, &[0], &requested(), &GENEROUS).unwrap();
        assert_eq!(
            decision.settings.samples_per_pixel,
            QualityRules::default().spp_ladder[0]
        );
        assert_eq!(decision.sample_trials.len(), 1);

        let mut noisy = Synthetic::new();
        noisy.noise = 0.5;
        let decision = decide(&mut noisy, &[0], &requested(), &GENEROUS).unwrap();
        let chosen = decision.settings.samples_per_pixel;
        assert!(
            chosen > QualityRules::default().spp_ladder[0],
            "noise must demand more samples"
        );
        let (rejected, accepted) = decision
            .sample_trials
            .split_at(decision.sample_trials.len() - 1);
        assert!(rejected.iter().all(|trial| !trial.accepted));
        assert!(accepted[0].accepted && accepted[0].value == chosen);
        // More samples must never look worse than fewer.
        assert!(
            decision
                .sample_trials
                .windows(2)
                .all(|pair| pair[1].difference < pair[0].difference)
        );
    }

    #[test]
    fn unconverged_sampling_falls_back_to_the_top_of_the_ladder() {
        let mut probe = Synthetic::new();
        probe.noise = 8.0;
        let decision = decide(&mut probe, &[0], &requested(), &GENEROUS).unwrap();
        assert_eq!(decision.settings.samples_per_pixel, 32);
        assert!(decision.sample_trials.iter().all(|trial| !trial.accepted));
    }

    #[test]
    fn hit_precision_coarsens_only_while_the_image_holds() {
        let mut insensitive = Synthetic::new();
        let decision = decide(&mut insensitive, &[0], &requested(), &GENEROUS).unwrap();
        assert_eq!(decision.settings.pixel_epsilon_multiplier, 3.0);
        assert_eq!(
            decision.epsilon_trials.len(),
            QualityRules::default().epsilon_ladder.len()
        );

        let mut sensitive = Synthetic::new();
        sensitive.epsilon_sensitivity = 0.2;
        let decision = decide(&mut sensitive, &[0], &requested(), &GENEROUS).unwrap();
        assert_eq!(decision.settings.pixel_epsilon_multiplier, 1.0);
        assert_eq!(decision.epsilon_trials.len(), 1);
        assert!(!decision.epsilon_trials[0].accepted);
    }

    #[test]
    fn fixed_hit_precision_is_left_alone() {
        let mut request = requested();
        request.pixel_epsilon_multiplier = 0.0;
        let decision = decide(&mut Synthetic::new(), &[0], &request, &GENEROUS).unwrap();
        assert_eq!(decision.settings.pixel_epsilon_multiplier, 0.0);
        assert!(decision.epsilon_trials.is_empty());
    }

    #[test]
    fn over_budget_drops_the_least_visible_effect_first() {
        let mut probe = Synthetic::new();
        probe.ambient_occlusion = 0.20;
        probe.soft_shadow = 0.08;
        // 4 spp with both effects at 16 steps costs 0.06 s; one effect 0.05 s.
        let limits = FrameLimits {
            mean_frame_seconds: 0.055,
            max_frame_seconds: 1.0,
        };
        let decision = decide(&mut probe, &[0], &requested(), &limits).unwrap();
        assert_eq!(
            decision.degradations,
            vec![Degradation::DroppedEffect {
                effect: Effect::SoftShadow,
                visible_difference: effect(&decision, Effect::SoftShadow).visible_difference,
            }]
        );
        assert!(effect(&decision, Effect::AmbientOcclusion).kept);
        assert!(!effect(&decision, Effect::SoftShadow).kept);
        assert!(decision.fits_budget);
        assert!(decision.estimate.mean_frame_seconds <= limits.mean_frame_seconds);
    }

    #[test]
    fn sampling_is_reduced_only_after_every_effect_is_gone() {
        let mut probe = Synthetic::new();
        probe.ambient_occlusion = 0.20;
        let limits = FrameLimits {
            mean_frame_seconds: 0.025,
            max_frame_seconds: 1.0,
        };
        let decision = decide(&mut probe, &[0], &requested(), &limits).unwrap();
        assert!(matches!(
            decision.degradations.as_slice(),
            [
                Degradation::DroppedEffect { .. },
                Degradation::ReducedSamples { from: 4, to: 2 }
            ]
        ));
        assert_eq!(decision.settings.samples_per_pixel, 2);
        assert!(decision.fits_budget);
    }

    #[test]
    fn sampling_is_reduced_before_a_more_visible_effect_is_dropped() {
        let mut probe = Synthetic::new();
        probe.ambient_occlusion = 0.30;
        probe.noise = 2.5;
        // Noise leaves the ladder at 32 spp, where one effect at 16 steps
        // costs 0.40 s. Halving the samples is far less visible than losing
        // the effect, and is enough to fit.
        let limits = FrameLimits {
            mean_frame_seconds: 0.25,
            max_frame_seconds: 1.0,
        };
        let mut request = requested();
        request.soft_shadow_steps = 0;
        let decision = decide(&mut probe, &[0], &request, &limits).unwrap();
        assert_eq!(
            decision.degradations,
            vec![Degradation::ReducedSamples { from: 32, to: 16 }]
        );
        assert!(effect(&decision, Effect::AmbientOcclusion).kept);
        assert!(decision.fits_budget);
    }

    #[test]
    fn an_impossible_budget_is_reported_instead_of_hidden() {
        let limits = FrameLimits {
            mean_frame_seconds: 1.0e-6,
            max_frame_seconds: 1.0,
        };
        let decision = decide(&mut Synthetic::new(), &[0, 1], &requested(), &limits).unwrap();
        assert!(!decision.fits_budget);
        assert_eq!(
            decision.settings.samples_per_pixel,
            QualityRules::default().min_spp
        );
        assert!(decision.estimate.mean_frame_seconds > limits.mean_frame_seconds);
    }

    #[test]
    fn the_slowest_frame_is_held_to_its_own_limit() {
        let limits = FrameLimits {
            mean_frame_seconds: 1.0,
            max_frame_seconds: 0.03,
        };
        let decision = decide(&mut Synthetic::new(), &[0], &requested(), &limits).unwrap();
        assert!(decision.estimate.max_frame_seconds <= 0.03);
        assert!(!decision.degradations.is_empty());
    }

    #[test]
    fn renders_are_cached_and_their_cost_is_accounted() {
        let mut probe = Synthetic::new();
        probe.ambient_occlusion = 0.15;
        let frames = [0, 3, 9];
        let decision = decide(&mut probe, &frames, &requested(), &GENEROUS).unwrap();
        assert_eq!(decision.renders, probe.calls);
        assert_eq!(decision.renders % frames.len() as u32, 0);
        assert!(decision.gpu_seconds_spent > 0.0);
        assert_eq!(decision.estimate.frames.len(), frames.len());

        // The chosen settings were rendered during the ladders, so estimating
        // them again must not have cost another render.
        let mut again = Synthetic::new();
        again.ambient_occlusion = 0.15;
        decide(&mut again, &frames, &requested(), &GENEROUS).unwrap();
        assert_eq!(again.calls, probe.calls);
    }

    #[test]
    fn ladders_and_thresholds_follow_the_rules_given() {
        let mut noisy = Synthetic::new();
        noisy.noise = 0.5;
        let default_spp = decide(&mut noisy, &[0], &requested(), &GENEROUS)
            .unwrap()
            .settings
            .samples_per_pixel;

        // A looser tolerance accepts the first rung of a custom ladder.
        let rules = QualityRules {
            spp_ladder: vec![3, 6, 12],
            spp_max_difference: 0.9,
            reference_spp: 24,
            epsilon_ladder: vec![2.5],
            ..QualityRules::default()
        };
        let mut noisy = Synthetic::new();
        noisy.noise = 0.5;
        let decision = super::decide(&mut noisy, &[0], &requested(), &GENEROUS, &rules).unwrap();
        assert_eq!(decision.settings.samples_per_pixel, 3);
        assert!(default_spp > 3);
        assert_eq!(decision.settings.pixel_epsilon_multiplier, 2.5);
        assert_eq!(decision.epsilon_trials.len(), 1);

        // A stricter visibility bar drops an effect the default keeps.
        let mut probe = Synthetic::new();
        probe.ambient_occlusion = 0.15;
        let rules = QualityRules {
            effect_min_difference: 0.9,
            ..QualityRules::default()
        };
        let decision = super::decide(&mut probe, &[0], &requested(), &GENEROUS, &rules).unwrap();
        assert!(!effect(&decision, Effect::AmbientOcclusion).kept);

        let empty = QualityRules {
            spp_ladder: Vec::new(),
            ..QualityRules::default()
        };
        assert!(
            super::decide(&mut Synthetic::new(), &[0], &requested(), &GENEROUS, &empty).is_err()
        );
    }

    #[test]
    fn probe_frames_are_required() {
        assert!(decide(&mut Synthetic::new(), &[], &requested(), &GENEROUS).is_err());
    }

    #[test]
    fn reduced_step_candidates_stay_below_the_request() {
        assert_eq!(reduced_steps(64), vec![16, 32]);
        assert_eq!(reduced_steps(8), vec![4]);
        assert_eq!(reduced_steps(4), Vec::<u32>::new());
        assert_eq!(reduced_steps(6), vec![4]);
    }
}
