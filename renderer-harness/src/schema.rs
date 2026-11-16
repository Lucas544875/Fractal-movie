//! Annotated templates of the three files a director writes. They give the
//! vocabulary and every permitted range up front, so a director does not
//! have to discover the format from validation errors.

use anyhow::{Result, bail};

use crate::{axes::AxisId, budget::Budget, tuning::Tuning};

/// Files a template exists for.
pub const FILES: [&str; 3] = ["brief", "shotlist", "look"];

/// The template of `file`, with the limits of this production filled in.
pub fn template(file: &str, tuning: &Tuning, budget: &Budget) -> Result<String> {
    Ok(match file {
        "brief" => brief(tuning, budget),
        "shotlist" => shotlist(tuning, budget),
        "look" => look(),
        other => bail!("there is no template for {other:?}; choose one of {FILES:?}"),
    })
}

fn values(axis: AxisId) -> String {
    axis.domain().join(" | ")
}

fn brief(tuning: &Tuning, budget: &Budget) -> String {
    let scouting = &tuning.scouting;
    format!(
        "\
# brief.yaml: the film's concept. Every field is required.
version: 1
title: Stacked Quarter          # one line, at most 80 characters
subject: A city of stacked stone blocks seen gliding over its rooftops.
                                # one sentence, one line, at most 120 characters;
                                # the critic checks every image against it
axes:                           # the formula family follows from the first location
  scale_regime: mid             # {scale}
  motion: flyover               # {motion}; the motion with the most screen time
  palette: {{ primary_hue: violet, key: high }}
                                # primary_hue: {hue}
                                # key: low | mid | high (recorded, not judged)
  light: backlit                # {light}
  pacing: medium                # {pacing}; from the mean cut length:
                                # slow >= {slow} s, medium >= {medium} s, fast below
locations: [loc-0007, loc-0004] # {fewest} to {most} selected locations; the first is the
                                # primary location and must get the most screen time
arc: Circle the block city, glide over its roofs, and pull back out.
                                # at most 400 characters
beats:                          # the arc as {fewest_beats} to {most_beats} moments, in order. Each is
                                # checked and drawn before any cut is planned, so
                                # write only what a location and a motion can show.
  - text: The whole city floats in the dark.     # one line, at most 120 characters
    location: loc-0007          # a location named above
    viewpoint: vp-1             # a viewpoint of it shown on a contact sheet
    motion: {{ kind: orbit, revolutions: 0.15 }}
                                # any motion of the shot list vocabulary; see
                                # `schema shotlist` for the kinds and their ranges
  - text: Gliding low over the rooftops.
    location: loc-0007
    viewpoint: vp-2
    motion: {{ kind: flyover, travel: 0.6, height: 0.3, look_ahead: 0.3 }}
  - text: Pulling back until the neighbouring cube enters the frame.
    location: loc-0004
    viewpoint: vp-1
    motion: {{ kind: pull-back, factor: 3.0 }}
# A beat that no motion or location can show cannot be filmed. Rewrite it, and
# record what was missing with `request` so the vocabulary can grow later.
",
        scale = values(AxisId::ScaleRegime),
        motion = values(AxisId::Motion),
        hue = values(AxisId::Palette),
        light = values(AxisId::Light),
        pacing = values(AxisId::Pacing),
        slow = tuning.classification.slow_cut_seconds,
        medium = tuning.classification.medium_cut_seconds,
        fewest = 1,
        most = scouting.max_selected,
        fewest_beats = tuning.storyboard.min_beats,
        most_beats = budget.film.max_cuts,
    )
}

fn shotlist(tuning: &Tuning, budget: &Budget) -> String {
    let rules = &tuning.shotlist;
    let film = &budget.film;
    format!(
        "\
# shotlist.yaml: the cuts of the film, in order.
# The film has {min_cuts} to {max_cuts} cuts and lasts {min_seconds} to {max_seconds} s in total.
# Each cut lasts {min_cut} to {max_cut} s. Consecutive cuts must differ in at least
# {differences} of role, scale regime, and motion.
version: 1
format: {{ width: 1920, height: 1080, fps: 30 }}
                  # width x height: 1280x720 | 1920x1080 | 2560x1440 | 3840x2160
                  # fps: 24 | 25 | 30 | 48 | 50 | 60
cuts:
  - id: 1                     # 1, 2, 3, ... in order
    role: establishing        # establishing | dive | detail | reveal | transit
    location: loc-0007        # a location named in brief.yaml
    viewpoint: vp-1           # a viewpoint of that location shown on a contact sheet
    motion: {{ kind: orbit, revolutions: 0.15, distance: 1.0 }}
    duration_seconds: 8
    transition_out: crossfade # cut | fade-black | crossfade; the last cut must use cut.
                              # A cut next to a fade or crossfade lasts at least {transition_cut} s.

# The five motions. distance scales the start's distance from the subject
# relative to the viewpoint (below 1 frames it tighter) and keeps the view
# direction. Moving the camera too close to a surface fails route-safety.
#
#   {{ kind: orbit, revolutions: 0.15, distance: 1.0 }}
#       circle the viewpoint's subject; revolutions is signed, |0.01..1|;
#       distance 0.2..4
#   {{ kind: dive, depth: 0.3, hold_seconds: 0, distance: 1.0 }}
#       approach the subject; depth = end distance / start distance, 0.001..0.9;
#       hold_seconds 0..half the cut; distance 0.2..4
#   {{ kind: pull-back, factor: 3.0, hold_seconds: 0, distance: 1.0 }}
#       retreat from the subject; factor = end distance / start distance,
#       1.1..20; hold_seconds 0..half the cut; distance 0.2..4
#   {{ kind: flyover, travel: 0.6, height: 0.3, look_ahead: 0.3 }}
#       glide over a surface the harness finds; travel 0.05..2 and height
#       0.02..1 in shape radii; look_ahead 0 (straight down)..2.
#       Bounded shapes only; the viewpoint only supplies the light.
#   {{ kind: multi-dive, depth: 0.05, dives: 2 }}
#       dive towards several features in turn with fades between them;
#       depth 0.0001..0.5; dives 1..4 with at least 3 s per dive.
#       Bounded shapes only.
",
        min_cuts = film.min_cuts,
        max_cuts = film.max_cuts,
        min_seconds = film.min_duration_seconds,
        max_seconds = film.max_duration_seconds,
        min_cut = rules.min_cut_seconds,
        max_cut = rules.max_cut_seconds,
        differences = rules.min_cut_differences,
        transition_cut = rules.min_transition_cut_seconds,
    )
}

fn look() -> String {
    "\
# look.yaml: material, light, and grade. Only version, material, and light
# are required. Colours are linear RGB, each component 0..64 (1 is white).
version: 1
material:
  base_color: [0.22, 0.08, 0.62]
  accent_color: [0.85, 0.45, 1.0]
  background_bottom: [0.10, 0.05, 0.22]
  background_top: [0.50, 0.36, 0.82]
  # Optional, with defaults:
  specular_color: [1.0, 1.0, 1.0]
  palette:                      # replaces the base-to-accent blend; positions
    - { position: 0.0, color: [0.22, 0.08, 0.62] }   # rise from 0.0 to 1.0
    - { position: 1.0, color: [0.85, 0.45, 1.0] }
  color_frequency: 1.8          # 0..1000
  orbit_palette_weight: 0.0     # 0..1; 0 colours by position, 1 by the fractal orbit
  palette_offset: 0.0           # 0..1
  camera_palette_weight: 0.0    # 0..1
  normal_palette_weight: 0.55   # 0..1
  ambient_strength: 0.18        # 0..16
  diffuse_strength: 0.95        # 0..16
  specular_strength: 0.35       # 0..16
  shininess: 48                 # 1..512
  metallic_specular_strength: 0 # 0..16
  metallic_shininess: 30        # 1..512
  rim_strength: 0.18            # 0..16
atmosphere: { fog: 0.1 }        # share of sky mixed in at the subject; 0 turns fog off
light: { azimuth_degrees: 150, elevation_degrees: 25 }
                                # relative to the camera: azimuth 0 = behind the
                                # camera, 90 = its right, 180 = behind the subject
                                # (-360..360); elevation -90..90
grade:                          # optional, with defaults
  exposure_stops: 0.0           # -20..20
  white_point: 4.0              # above 0
  contrast: 1.0                 # 0..4
  saturation: 1.0               # 0..4
  gamma: 1.0                    # 0.1..4
  vignette: 0.0                 # 0..1
per_cut:                        # optional overrides by cut id
  3: { light: { azimuth_degrees: 90, elevation_degrees: 40 }, exposure_stops: 0.3 }
effects_requested:              # optional; omit an effect to leave it off. Each is
                                # kept only if removing it is visibly different.
  ambient_occlusion: { strength: 0.5 }
  soft_shadow: { softness_degrees: 1.5 }
  reflection: { strength: 0.1, roughness: 0.05 }
  depth_of_field: { strength: 1.0 }
"
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        axes::ScaleRegime,
        brief::Brief,
        look::{Look, grade, tests::compiled_cut},
        shotlist::Shotlist,
    };

    fn render(file: &str) -> String {
        template(file, &Tuning::default(), &Budget::default()).unwrap()
    }

    #[test]
    fn every_template_parses_as_its_file() {
        let brief: Brief = serde_yaml_ng::from_str(&render("brief")).unwrap();
        assert_eq!(brief.locations.len(), 2);
        assert_eq!(brief.beats.len(), 3);
        for beat in &brief.beats {
            beat.motion.validate(8.0, 60.0).unwrap();
        }
        let shotlist: Shotlist = serde_yaml_ng::from_str(&render("shotlist")).unwrap();
        shotlist.format.validate().unwrap();
        let look: Look = serde_yaml_ng::from_str(&render("look")).unwrap();
        assert_eq!(look.per_cut.len(), 1);
        assert!(template("budget", &Tuning::default(), &Budget::default()).is_err());
    }

    #[test]
    fn the_example_values_pass_validation() {
        let shotlist: Shotlist = serde_yaml_ng::from_str(&render("shotlist")).unwrap();
        for cut in &shotlist.cuts {
            cut.motion.validate(cut.duration_seconds, 60.0).unwrap();
        }
        // Every motion written in the comments must be valid as written.
        let text = render("shotlist");
        let motions: Vec<&str> = text
            .lines()
            .filter_map(|line| line.trim_start_matches(['#', ' ']).strip_prefix("{ kind:"))
            .collect();
        assert_eq!(motions.len(), 5);
        for motion in motions {
            let spec: crate::cut::MotionSpec =
                serde_yaml_ng::from_str(&format!("{{ kind:{motion}")).unwrap();
            spec.validate(12.0, 60.0).unwrap();
        }

        // The look applies to a real cut without error, effects included.
        let look: Look = serde_yaml_ng::from_str(&render("look")).unwrap();
        grade(
            &look,
            3,
            &compiled_cut(ScaleRegime::Mid),
            &Tuning::default(),
        )
        .unwrap();
    }

    #[test]
    fn limits_come_from_the_production() {
        let mut tuning = Tuning::default();
        tuning.shotlist.max_cut_seconds = 20.0;
        let mut budget = Budget::default();
        budget.film.max_cuts = 5;
        let text = template("shotlist", &tuning, &budget).unwrap();
        assert!(text.contains("4 to 5 cuts"), "{text}");
        assert!(text.contains("1 to 20 s"), "{text}");
        assert!(render("brief").contains("overview | mid | deep"));
    }
}
