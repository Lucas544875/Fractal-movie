//! GPU time budget. The budget is fixed before design starts and counts GPU
//! busy time, so a lower duty cycle lengthens wall time without buying more
//! rendering.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    /// Production GPU minutes allowed per second of finished film.
    pub gpu_minutes_per_output_second: f64,
    /// Design-stage GPU time allowed, as a fraction of the production budget.
    pub design_budget_ratio: f64,
    /// Longest permitted render time for a single final frame.
    pub max_frame_seconds: f64,
    /// Upper bound for every GPU command's duty cycle, in percent.
    pub max_gpu_duty_cycle: f64,
    /// Film length assumed until the shot list fixes the real duration.
    pub planned_duration_seconds: f64,
    /// Permitted length and cut count of the film.
    #[serde(default)]
    pub film: FilmLimits,
    #[serde(default)]
    pub spent: Spent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilmLimits {
    pub min_duration_seconds: f64,
    pub max_duration_seconds: f64,
    pub min_cuts: usize,
    pub max_cuts: usize,
}

impl Default for FilmLimits {
    fn default() -> Self {
        Self {
            min_duration_seconds: 45.0,
            max_duration_seconds: 90.0,
            min_cuts: 4,
            max_cuts: 8,
        }
    }
}

/// GPU busy time consumed so far.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spent {
    pub design_gpu_seconds: f64,
    pub production_gpu_seconds: f64,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            gpu_minutes_per_output_second: 2.0,
            design_budget_ratio: 0.25,
            max_frame_seconds: 12.0,
            max_gpu_duty_cycle: 80.0,
            planned_duration_seconds: 60.0,
            film: FilmLimits::default(),
            spent: Spent::default(),
        }
    }
}

impl Budget {
    pub fn validate(&self) -> Result<()> {
        let positive = [
            (
                "gpu_minutes_per_output_second",
                self.gpu_minutes_per_output_second,
            ),
            ("max_frame_seconds", self.max_frame_seconds),
            ("planned_duration_seconds", self.planned_duration_seconds),
        ];
        for (name, value) in positive {
            if !value.is_finite() || value <= 0.0 {
                bail!("budget {name} must be finite and positive");
            }
        }
        if !self.design_budget_ratio.is_finite() || !(0.0..=1.0).contains(&self.design_budget_ratio)
        {
            bail!("budget design_budget_ratio must be in 0..=1");
        }
        if !self.max_gpu_duty_cycle.is_finite() || !(1.0..=100.0).contains(&self.max_gpu_duty_cycle)
        {
            bail!("budget max_gpu_duty_cycle must be in 1..=100");
        }
        let film = &self.film;
        if !film.min_duration_seconds.is_finite()
            || !film.max_duration_seconds.is_finite()
            || film.min_duration_seconds <= 0.0
            || film.max_duration_seconds < film.min_duration_seconds
        {
            bail!("budget film duration limits must be positive and ordered");
        }
        if film.min_cuts == 0 || film.max_cuts < film.min_cuts {
            bail!("budget film cut limits must be positive and ordered");
        }
        for (name, value) in [
            ("design_gpu_seconds", self.spent.design_gpu_seconds),
            ("production_gpu_seconds", self.spent.production_gpu_seconds),
        ] {
            if !value.is_finite() || value < 0.0 {
                bail!("budget spent {name} must be finite and non-negative");
            }
        }
        Ok(())
    }

    /// Production GPU seconds allowed for the planned film length.
    #[must_use]
    pub fn production_limit_seconds(&self) -> f64 {
        self.gpu_minutes_per_output_second * 60.0 * self.planned_duration_seconds
    }

    /// Design-stage GPU seconds allowed for the planned film length.
    #[must_use]
    pub fn design_limit_seconds(&self) -> f64 {
        self.production_limit_seconds() * self.design_budget_ratio
    }

    #[must_use]
    pub fn design_remaining_seconds(&self) -> f64 {
        (self.design_limit_seconds() - self.spent.design_gpu_seconds).max(0.0)
    }

    /// Mean GPU seconds available per final frame at `fps`.
    #[must_use]
    pub fn mean_frame_seconds(&self, fps: f64) -> f64 {
        self.gpu_minutes_per_output_second * 60.0 / fps
    }

    /// Rejects a duty cycle above the production's ceiling.
    pub fn check_duty_cycle(&self, requested: f64) -> Result<()> {
        if !requested.is_finite() || !(1.0..=self.max_gpu_duty_cycle).contains(&requested) {
            bail!(
                "GPU duty cycle {requested} is outside 1..={} allowed for this production",
                self.max_gpu_duty_cycle
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_match_the_spec() {
        let budget = Budget::default();
        budget.validate().unwrap();
        assert_eq!(budget.production_limit_seconds(), 7200.0);
        assert_eq!(budget.design_limit_seconds(), 1800.0);
        assert_eq!(budget.mean_frame_seconds(30.0), 4.0);
    }

    #[test]
    fn rejects_out_of_range_values() {
        for mutate in [
            (|budget: &mut Budget| budget.gpu_minutes_per_output_second = 0.0) as fn(&mut Budget),
            |budget| budget.design_budget_ratio = 1.5,
            |budget| budget.max_frame_seconds = f64::NAN,
            |budget| budget.max_gpu_duty_cycle = 0.0,
            |budget| budget.planned_duration_seconds = -1.0,
            |budget| budget.spent.design_gpu_seconds = -1.0,
            |budget| budget.film.max_duration_seconds = 10.0,
            |budget| budget.film.min_cuts = 0,
        ] {
            let mut budget = Budget::default();
            mutate(&mut budget);
            assert!(budget.validate().is_err());
        }
    }

    #[test]
    fn remaining_design_budget_never_goes_negative() {
        let mut budget = Budget::default();
        budget.spent.design_gpu_seconds = 600.0;
        assert_eq!(budget.design_remaining_seconds(), 1200.0);
        budget.spent.design_gpu_seconds = 5000.0;
        assert_eq!(budget.design_remaining_seconds(), 0.0);
    }

    #[test]
    fn duty_cycle_is_capped_by_the_production() {
        let budget = Budget::default();
        budget.check_duty_cycle(80.0).unwrap();
        budget.check_duty_cycle(40.0).unwrap();
        assert!(budget.check_duty_cycle(81.0).is_err());
        assert!(budget.check_duty_cycle(0.0).is_err());
    }
}
