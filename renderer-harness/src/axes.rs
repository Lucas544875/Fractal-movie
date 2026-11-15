//! Categorical axes that describe a film at the concept level. Diversity is
//! judged on these values rather than on scene parameters, and the harness
//! derives them from artifacts instead of trusting an agent's self-report.

use fractal_renderer_core::{FractalConfig, OrbitTransform};
use serde::{Deserialize, Serialize};

macro_rules! axis_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $label:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $label)] $variant),+
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $label),+ }
            }

            #[must_use]
            pub fn parse(label: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|value| value.as_str() == label)
            }
        }
    };
}

axis_enum!(
    /// Structural family of the distance estimator, classified from the set
    /// of orbit transforms.
    FormulaFamily {
        Mandelbulb => "mandelbulb",
        Mandelbox => "mandelbox",
        RotatedMandelbox => "rotated-mandelbox",
        JuliaBox => "julia-box",
        AmazingSurf => "amazing-surf",
        MandelboxJulia => "mandelbox-julia",
        AmazingHybrid => "amazing-hybrid",
    }
);

axis_enum!(
    /// Camera distance relative to the shape's bounding radius.
    ScaleRegime {
        Overview => "overview",
        Mid => "mid",
        Deep => "deep",
    }
);

axis_enum!(
    /// Camera motion that occupies the most screen time.
    Motion {
        Dive => "dive",
        Orbit => "orbit",
        MultiDive => "multi-dive",
        Flyover => "flyover",
    }
);

axis_enum!(
    /// Dominant hue of the surface palette.
    PaletteHue {
        Red => "red",
        Amber => "amber",
        Green => "green",
        Teal => "teal",
        Blue => "blue",
        Violet => "violet",
        Neutral => "neutral",
    }
);

axis_enum!(
    /// Overall brightness of the image. Recorded with the palette but not
    /// counted as a separate axis.
    PaletteKey {
        Low => "low",
        Mid => "mid",
        High => "high",
    }
);

axis_enum!(
    /// Direction of the key light relative to the view.
    Light {
        Frontal => "frontal",
        Raking => "raking",
        Top => "top",
        Backlit => "backlit",
    }
);

axis_enum!(
    /// Editing tempo, classified from the mean cut length.
    Pacing {
        Slow => "slow",
        Medium => "medium",
        Fast => "fast",
    }
);

axis_enum!(
    /// Identifies one of the six diversity axes.
    AxisId {
        FormulaFamily => "formula_family",
        ScaleRegime => "scale_regime",
        Motion => "motion",
        Palette => "palette",
        Light => "light",
        Pacing => "pacing",
    }
);

impl AxisId {
    /// Every value the axis can take, in declaration order.
    #[must_use]
    pub fn domain(self) -> Vec<&'static str> {
        fn labels<T: Copy>(all: &[T], label: fn(T) -> &'static str) -> Vec<&'static str> {
            all.iter().copied().map(label).collect()
        }
        match self {
            Self::FormulaFamily => labels(FormulaFamily::ALL, FormulaFamily::as_str),
            Self::ScaleRegime => labels(ScaleRegime::ALL, ScaleRegime::as_str),
            Self::Motion => labels(Motion::ALL, Motion::as_str),
            Self::Palette => labels(PaletteHue::ALL, PaletteHue::as_str),
            Self::Light => labels(Light::ALL, Light::as_str),
            Self::Pacing => labels(Pacing::ALL, Pacing::as_str),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Palette {
    pub primary_hue: PaletteHue,
    pub key: PaletteKey,
}

/// The concept-level signature of one film.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Axes {
    pub formula_family: FormulaFamily,
    pub scale_regime: ScaleRegime,
    pub motion: Motion,
    pub palette: Palette,
    pub light: Light,
    pub pacing: Pacing,
}

impl Axes {
    /// The comparable value of one axis. The palette compares by hue only, so
    /// that a brightness change alone does not count as a different film.
    #[must_use]
    pub const fn value(&self, axis: AxisId) -> &'static str {
        match axis {
            AxisId::FormulaFamily => self.formula_family.as_str(),
            AxisId::ScaleRegime => self.scale_regime.as_str(),
            AxisId::Motion => self.motion.as_str(),
            AxisId::Palette => self.palette.primary_hue.as_str(),
            AxisId::Light => self.light.as_str(),
            AxisId::Pacing => self.pacing.as_str(),
        }
    }

    /// Axes on which the two films differ.
    #[must_use]
    pub fn differing(&self, other: &Self) -> Vec<AxisId> {
        AxisId::ALL
            .iter()
            .copied()
            .filter(|axis| self.value(*axis) != other.value(*axis))
            .collect()
    }
}

/// Classifies a fractal definition into its structural family.
#[must_use]
pub fn classify_family(fractal: &FractalConfig) -> FormulaFamily {
    let dsl = match fractal {
        FractalConfig::Mandelbulb(_) => return FormulaFamily::Mandelbulb,
        FractalConfig::Mandelbox(_) => return FormulaFamily::Mandelbox,
        FractalConfig::Dsl(dsl) => dsl,
    };

    let has = |predicate: fn(&OrbitTransform) -> bool| dsl.orbit.iter().any(predicate);
    let amazing = has(|op| matches!(op, OrbitTransform::AmazingSurfFold { .. }));
    let box_julia = has(|op| matches!(op, OrbitTransform::MandelboxJuliaFold { .. }));
    let julia = has(|op| matches!(op, OrbitTransform::ScaleAddConstant { .. }));
    let rotated = has(|op| matches!(op, OrbitTransform::Rotate { .. }));

    match (amazing, box_julia) {
        (true, true) => FormulaFamily::AmazingHybrid,
        (true, false) => FormulaFamily::AmazingSurf,
        (false, true) => FormulaFamily::MandelboxJulia,
        (false, false) if julia => FormulaFamily::JuliaBox,
        (false, false) if rotated => FormulaFamily::RotatedMandelbox,
        (false, false) => FormulaFamily::Mandelbox,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fractal_renderer_core::{DslFractalConfig, MandelbulbConfig};

    fn axes() -> Axes {
        Axes {
            formula_family: FormulaFamily::Mandelbox,
            scale_regime: ScaleRegime::Mid,
            motion: Motion::Orbit,
            palette: Palette {
                primary_hue: PaletteHue::Teal,
                key: PaletteKey::Low,
            },
            light: Light::Raking,
            pacing: Pacing::Slow,
        }
    }

    #[test]
    fn labels_round_trip_through_parse_and_serde() {
        for family in FormulaFamily::ALL {
            assert_eq!(FormulaFamily::parse(family.as_str()), Some(*family));
            let json = serde_json::to_string(family).unwrap();
            assert_eq!(json, format!("\"{}\"", family.as_str()));
            assert_eq!(
                serde_json::from_str::<FormulaFamily>(&json).unwrap(),
                *family
            );
        }
        assert_eq!(FormulaFamily::parse("unknown"), None);
    }

    #[test]
    fn every_axis_has_a_domain_containing_its_value() {
        let axes = axes();
        for axis in AxisId::ALL {
            assert!(axis.domain().contains(&axes.value(*axis)));
        }
    }

    #[test]
    fn palette_key_alone_is_not_a_difference() {
        let base = axes();
        let mut brighter = base;
        brighter.palette.key = PaletteKey::High;
        assert!(base.differing(&brighter).is_empty());

        let mut other = base;
        other.palette.primary_hue = PaletteHue::Amber;
        other.motion = Motion::Dive;
        assert_eq!(
            base.differing(&other),
            vec![AxisId::Motion, AxisId::Palette]
        );
    }

    #[test]
    fn classifies_builtin_and_dsl_families() {
        assert_eq!(
            classify_family(&FractalConfig::Mandelbulb(MandelbulbConfig::default())),
            FormulaFamily::Mandelbulb
        );

        let mut dsl = DslFractalConfig::default();
        assert_eq!(
            classify_family(&FractalConfig::Dsl(dsl.clone())),
            FormulaFamily::Mandelbox
        );

        dsl.orbit.insert(
            0,
            OrbitTransform::Rotate {
                axis: [0.0, 0.0, 1.0],
                degrees: 7.5,
            },
        );
        assert_eq!(
            classify_family(&FractalConfig::Dsl(dsl.clone())),
            FormulaFamily::RotatedMandelbox
        );

        dsl.orbit.push(OrbitTransform::ScaleAddConstant {
            scale: 2.0,
            constant: [0.1, 0.2, 0.3],
        });
        assert_eq!(
            classify_family(&FractalConfig::Dsl(dsl.clone())),
            FormulaFamily::JuliaBox
        );

        dsl.orbit.push(OrbitTransform::AmazingSurfFold {
            start_iteration: 0,
            stop_iteration: 4,
            limits: [1.0, 1.0],
            minimum_radius_squared: 0.5,
            scale: 1.5,
            rotation_degrees: [0.0; 3],
        });
        assert_eq!(
            classify_family(&FractalConfig::Dsl(dsl.clone())),
            FormulaFamily::AmazingSurf
        );

        dsl.orbit.push(OrbitTransform::MandelboxJuliaFold {
            start_iteration: 4,
            stop_iteration: 8,
            fold_limit: 1.0,
            min_radius_squared: 0.25,
            fixed_radius_squared: 1.0,
            scale: 2.0,
            constant: [0.0; 3],
            rotation_degrees: [0.0; 3],
        });
        assert_eq!(
            classify_family(&FractalConfig::Dsl(dsl)),
            FormulaFamily::AmazingHybrid
        );
    }
}
