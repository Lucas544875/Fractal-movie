//! The look: material, light, and grade of a film. A director describes it
//! in terms that do not depend on a shape's scale; the harness turns it into
//! render parameters for each cut, previews it, and checks that the result
//! is the palette and light the brief committed to.

use std::{collections::BTreeMap, fs};

use anyhow::{Context, Result, bail};
use fractal_renderer_core::{
    AmbientOcclusionConfig, DslMaterial, DslPaletteStop, FractalConfig, LoadedScene,
    PostProcessConfig, ReflectionConfig, Renderer, SoftShadowConfig, ToneMappingConfig,
    ToneMappingOperator, load_scene,
};
use serde::{Deserialize, Serialize};

use crate::{
    axes::{AxisId, Light, Palette},
    brief,
    budget::Budget,
    classify,
    gpu::{GpuOptions, apply_settings, frame_config},
    metrics::{Frame, Legibility, legibility},
    production::Production,
    quality::Settings,
    shape,
    sheet::{self, Cell},
    shotlist::{Manifest, content_hash},
    stage::{Stage, State},
    tuning::{LookRules, Tuning},
};

pub const LOOK_VERSION: u32 = 1;
pub const LOOK_FILE: &str = "look.yaml";
/// Harness-owned record of the graded cuts the last preview produced.
pub const GRADED_MANIFEST_FILE: &str = "compiled/graded/manifest.json";
pub const SHEET_FILE: &str = "look/sheet.png";
pub const REPORT_FILE: &str = "look/report.json";

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Look {
    pub version: u32,
    pub material: MaterialSpec,
    #[serde(default)]
    pub atmosphere: Atmosphere,
    pub light: LightSpec,
    #[serde(default)]
    pub grade: Grade,
    /// Overrides for single cuts, keyed by cut id.
    #[serde(default)]
    pub per_cut: BTreeMap<u32, CutOverride>,
    #[serde(default)]
    pub effects_requested: EffectsRequested,
}

/// Surface and background colours. Colours are linear RGB.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialSpec {
    pub base_color: [f32; 3],
    pub accent_color: [f32; 3],
    #[serde(default = "white")]
    pub specular_color: [f32; 3],
    pub background_bottom: [f32; 3],
    pub background_top: [f32; 3],
    /// Optional gradient that replaces the base-to-accent blend.
    #[serde(default)]
    pub palette: Vec<PaletteStop>,
    #[serde(default = "default_color_frequency")]
    pub color_frequency: f32,
    /// 0 colours by position, 1 by the fractal's orbit.
    #[serde(default)]
    pub orbit_palette_weight: f32,
    #[serde(default)]
    pub palette_offset: f32,
    #[serde(default)]
    pub camera_palette_weight: f32,
    #[serde(default = "default_normal_palette_weight")]
    pub normal_palette_weight: f32,
    #[serde(default = "default_ambient")]
    pub ambient_strength: f32,
    #[serde(default = "default_diffuse")]
    pub diffuse_strength: f32,
    #[serde(default = "default_specular")]
    pub specular_strength: f32,
    #[serde(default = "default_shininess")]
    pub shininess: f32,
    #[serde(default)]
    pub metallic_specular_strength: f32,
    #[serde(default = "default_metallic_shininess")]
    pub metallic_shininess: f32,
    #[serde(default = "default_rim")]
    pub rim_strength: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaletteStop {
    pub position: f32,
    pub color: [f32; 3],
}

const fn white() -> [f32; 3] {
    [1.0; 3]
}
const fn default_color_frequency() -> f32 {
    1.8
}
const fn default_normal_palette_weight() -> f32 {
    0.55
}
const fn default_ambient() -> f32 {
    0.18
}
const fn default_diffuse() -> f32 {
    0.95
}
const fn default_specular() -> f32 {
    0.35
}
const fn default_shininess() -> f32 {
    48.0
}
const fn default_metallic_shininess() -> f32 {
    30.0
}
const fn default_rim() -> f32 {
    0.18
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Atmosphere {
    /// Share of sky mixed into the subject, as an optical depth. Zero turns
    /// fog off. It is scaled to each cut's subject distance.
    pub fog: f64,
}

impl Default for Atmosphere {
    fn default() -> Self {
        Self { fog: 0.1 }
    }
}

/// Key light, relative to the camera at the middle of each cut.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightSpec {
    /// 0 lights from behind the camera, 90 from its right, 180 from behind
    /// the subject.
    pub azimuth_degrees: f64,
    /// Height of the light above the camera's horizon.
    pub elevation_degrees: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Grade {
    pub exposure_stops: f32,
    /// Scene luminance that maps to white.
    pub white_point: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub gamma: f32,
    pub vignette: f32,
}

impl Default for Grade {
    fn default() -> Self {
        Self {
            exposure_stops: 0.0,
            white_point: 4.0,
            contrast: 1.0,
            saturation: 1.0,
            gamma: 1.0,
            vignette: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CutOverride {
    pub light: Option<LightSpec>,
    /// Added to the grade's exposure for this cut.
    pub exposure_stops: f32,
}

/// Effects a director asks for. Whether and how finely each is rendered is
/// decided later from what it visibly contributes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EffectsRequested {
    pub ambient_occlusion: Option<Occlusion>,
    pub soft_shadow: Option<Shadow>,
    pub reflection: Option<Reflection>,
    pub depth_of_field: Option<DepthOfField>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Occlusion {
    pub strength: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shadow {
    /// Angular radius of the light; larger is softer.
    pub softness_degrees: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reflection {
    pub strength: f32,
    #[serde(default)]
    pub roughness: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepthOfField {
    /// 1 is a pronounced blur; the lens always focuses on the subject.
    pub strength: f32,
}

/// A cut with the look applied.
pub struct GradedCut {
    pub scene: LoadedScene,
    pub light: Light,
    /// True when the cut's shape has no adjustable material.
    pub material_ignored: bool,
}

/// Applies a look to one compiled cut.
pub fn grade(look: &Look, cut_id: u32, cut: &LoadedScene, tuning: &Tuning) -> Result<GradedCut> {
    let rules = &tuning.look;
    let animation = cut
        .animation
        .as_ref()
        .context("a cut must have an animation")?;
    // The middle of the cut stands for its framing: light and scale-bound
    // parameters are derived from the camera there.
    let middle = animation.sample(&cut.config, (animation.frame_count - 1) / 2)?;
    let position = middle.config.camera.position.to_f64();
    let target = middle.config.camera.target.to_f64();
    let to_subject = [0, 1, 2].map(|axis| target[axis] - position[axis]);
    let subject_distance = length(to_subject);
    if !(subject_distance.is_finite() && subject_distance > 0.0) {
        bail!("cut {cut_id} has no subject distance to scale the look by");
    }
    let forward = to_subject.map(|value| value / subject_distance);
    let up = orthogonal_up(forward, middle.config.camera.up.map(f64::from));

    let overrides = look.per_cut.get(&cut_id).copied().unwrap_or_default();
    let light = light_direction(overrides.light.unwrap_or(look.light), forward, up)?;

    let mut config = cut.config.clone();
    config.light.direction = light.map(|value| value as f32);

    let material_ignored = match &mut config.fractal {
        FractalConfig::Dsl(dsl) => {
            dsl.material = look
                .material
                .to_material(
                    shape::subject_fog_density(subject_distance, look.atmosphere.fog) as f32,
                );
            false
        }
        FractalConfig::Mandelbulb(_) | FractalConfig::Mandelbox(_) => true,
    };

    let grade = look.grade;
    config.quality.tone_mapping = ToneMappingConfig {
        enabled: true,
        operator: ToneMappingOperator::ExtendedReinhard,
        exposure_stops: grade.exposure_stops + overrides.exposure_stops,
        white_point: grade.white_point,
        ..config.quality.tone_mapping
    };
    config.quality.post_process = PostProcessConfig {
        enabled: true,
        exposure_stops: 0.0,
        contrast: grade.contrast,
        saturation: grade.saturation,
        gamma: grade.gamma,
        vignette_strength: grade.vignette,
    };

    let effects = look.effects_requested;
    config.quality.ambient_occlusion = match effects.ambient_occlusion {
        Some(occlusion) => AmbientOcclusionConfig {
            max_steps: rules.occlusion_steps,
            radius: (rules.occlusion_radius_ratio * subject_distance) as f32,
            strength: occlusion.strength,
        },
        None => AmbientOcclusionConfig {
            max_steps: 0,
            strength: 0.0,
            ..config.quality.ambient_occlusion
        },
    };
    config.quality.soft_shadow = match effects.soft_shadow {
        Some(shadow) => SoftShadowConfig {
            max_steps: rules.shadow_steps,
            angular_radius_degrees: shadow.softness_degrees,
            max_distance: (rules.shadow_distance_ratio * subject_distance) as f32,
        },
        None => SoftShadowConfig {
            max_steps: 0,
            ..config.quality.soft_shadow
        },
    };
    config.quality.reflection = match effects.reflection {
        Some(reflection) => ReflectionConfig {
            max_steps: rules.reflection_steps,
            max_distance: (rules.reflection_distance_ratio * subject_distance) as f32,
            strength: reflection.strength,
            roughness: reflection.roughness,
        },
        None => ReflectionConfig {
            max_steps: 0,
            strength: 0.0,
            ..config.quality.reflection
        },
    };
    config.camera.focus_distance = subject_distance as f32;
    config.camera.aperture_radius = effects.depth_of_field.map_or(0.0, |depth_of_field| {
        (f64::from(depth_of_field.strength) * rules.aperture_ratio * subject_distance) as f32
    });

    config
        .validate()
        .with_context(|| format!("the look is not renderable on cut {cut_id}"))?;
    animation
        .validate(&config)
        .with_context(|| format!("the look is incompatible with the path of cut {cut_id}"))?;

    Ok(GradedCut {
        light: classify::light(light, forward, up, &tuning.classification),
        scene: LoadedScene {
            name: cut.name.clone(),
            config,
            animation: cut.animation.clone(),
            video: None,
        },
        material_ignored,
    })
}

impl MaterialSpec {
    fn to_material(&self, fog_density: f32) -> DslMaterial {
        DslMaterial {
            base_color: self.base_color,
            accent_color: self.accent_color,
            specular_color: self.specular_color,
            background_bottom: self.background_bottom,
            background_top: self.background_top,
            color_frequency: self.color_frequency,
            surface_palette: self
                .palette
                .iter()
                .map(|stop| DslPaletteStop {
                    position: stop.position,
                    color: stop.color,
                })
                .collect(),
            orbit_palette_weight: self.orbit_palette_weight,
            palette_offset: self.palette_offset,
            camera_palette_weight: self.camera_palette_weight,
            normal_palette_weight: self.normal_palette_weight,
            ambient_strength: self.ambient_strength,
            diffuse_strength: self.diffuse_strength,
            specular_strength: self.specular_strength,
            shininess: self.shininess,
            metallic_specular_strength: self.metallic_specular_strength,
            metallic_shininess: self.metallic_shininess,
            rim_strength: self.rim_strength,
            fog_density,
        }
    }
}

/// Direction from the surface towards the light, in world space.
fn light_direction(spec: LightSpec, forward: [f64; 3], up: [f64; 3]) -> Result<[f64; 3]> {
    if !spec.azimuth_degrees.is_finite()
        || !spec.elevation_degrees.is_finite()
        || !(-360.0..=360.0).contains(&spec.azimuth_degrees)
        || !(-90.0..=90.0).contains(&spec.elevation_degrees)
    {
        bail!("light azimuth must be in -360..=360 and elevation in -90..=90 degrees");
    }
    let right = cross(forward, up);
    let (azimuth, elevation) = (
        spec.azimuth_degrees.to_radians(),
        spec.elevation_degrees.to_radians(),
    );
    Ok([0, 1, 2].map(|axis| {
        elevation.cos() * (-forward[axis] * azimuth.cos() + right[axis] * azimuth.sin())
            + elevation.sin() * up[axis]
    }))
}

/// The camera's up vector, made perpendicular to its line of sight.
fn orthogonal_up(forward: [f64; 3], up: [f64; 3]) -> [f64; 3] {
    let along: f64 = (0..3).map(|axis| up[axis] * forward[axis]).sum();
    let mut perpendicular = [0, 1, 2].map(|axis| up[axis] - forward[axis] * along);
    if length(perpendicular) < 1.0e-9 {
        // Up parallel to the view: any perpendicular serves.
        let reference = if forward[2].abs() < 0.9 {
            [0.0, 0.0, 1.0]
        } else {
            [0.0, 1.0, 0.0]
        };
        perpendicular = cross(cross(forward, reference), forward);
    }
    let norm = length(perpendicular);
    perpendicular.map(|value| value / norm)
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn length(a: [f64; 3]) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

/// A graded cut as later stages find it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradedEntry {
    pub id: u32,
    /// Scene file relative to the production directory.
    pub scene: String,
    pub light: Light,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradedManifest {
    pub look_hash: String,
    pub shotlist_hash: String,
    pub effects_requested: EffectsRequested,
    pub cuts: Vec<GradedEntry>,
}

impl GradedManifest {
    /// Loads the manifest and refuses it if the look or the shot list
    /// changed since the preview that wrote it.
    pub fn load_current(production: &Production) -> Result<Self> {
        let path = production.path(GRADED_MANIFEST_FILE);
        let text = fs::read_to_string(&path)
            .context("the look has not been previewed; run `look preview` first")?;
        let manifest: Self =
            serde_json::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
        let look = fs::read_to_string(production.path(LOOK_FILE))
            .with_context(|| format!("could not read {LOOK_FILE}"))?;
        if content_hash(&look) != manifest.look_hash {
            bail!("{LOOK_FILE} changed after it was previewed; run `look preview` again");
        }
        if Manifest::load_current(production)?.shotlist_hash != manifest.shotlist_hash {
            bail!("the shot list changed after the look was previewed; run `look preview` again");
        }
        Ok(manifest)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct FrameMeasurement {
    /// Cut id, hyphen, 1 for the start, 2 for the middle, 3 for the end.
    pub label: String,
    pub legibility: Legibility,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct LookReport {
    pub schema_errors: Vec<String>,
    pub warnings: Vec<String>,
    pub sheet: Option<&'static str>,
    pub frames: Vec<FrameMeasurement>,
    /// Palette and light the previewed film amounts to.
    pub palette: Option<Palette>,
    pub light: Option<Light>,
    pub hue_problems: Vec<String>,
    pub clipping_problems: Vec<String>,
    pub gpu_seconds: f64,
}

/// Reads `look.yaml`, applies it to every cut, renders preview frames, and
/// judges the measured gates of the look stage.
pub fn preview(
    production: &Production,
    state: &mut State,
    budget: &mut Budget,
    gpu: &GpuOptions,
) -> Result<LookReport> {
    if state.stage != Stage::Look {
        bail!(
            "the look is previewed in stage look; the production is in stage {}",
            state.stage.as_str()
        );
    }
    let tuning = production.load_tuning()?;
    let rules = tuning.look;
    let manifest = Manifest::load_current(production)?;
    let axes = brief::load_axes(production)?;
    if budget.design_remaining_seconds() <= 0.0 {
        bail!("the design GPU budget of this production is exhausted");
    }
    let duty_cycle = gpu.duty_cycle_percent.unwrap_or(budget.max_gpu_duty_cycle);
    budget.check_duty_cycle(duty_cycle)?;
    let gpu = GpuOptions {
        duty_cycle_percent: Some(duty_cycle),
        ..gpu.clone()
    };

    // A changed look voids every earlier judgement of it.
    state.reset_gates();
    let _ = fs::remove_file(production.path(GRADED_MANIFEST_FILE));
    let mut report = LookReport::default();
    let fail_schema = |state: &mut State, report: LookReport| -> Result<LookReport> {
        state.record_gate("schema", false, report.schema_errors.join("; "))?;
        Ok(report)
    };

    let text = match fs::read_to_string(production.path(LOOK_FILE)) {
        Ok(text) => text,
        Err(error) => {
            report
                .schema_errors
                .push(format!("could not read {LOOK_FILE}: {error}"));
            return fail_schema(state, report);
        }
    };
    let look: Look = match serde_yaml_ng::from_str(&text) {
        Ok(look) => look,
        Err(error) => {
            report.schema_errors.push(format!(
                "{LOOK_FILE} does not match the look schema: {error}"
            ));
            return fail_schema(state, report);
        }
    };
    if look.version != LOOK_VERSION {
        report.schema_errors.push(format!(
            "version must be {LOOK_VERSION}, not {}",
            look.version
        ));
    }
    for id in look.per_cut.keys() {
        if !manifest.cuts.iter().any(|cut| cut.id == *id) {
            report.schema_errors.push(format!(
                "per_cut names cut {id}, which the shot list does not have"
            ));
        }
    }

    let mut graded = Vec::with_capacity(manifest.cuts.len());
    for cut in &manifest.cuts {
        let scene = load_scene(production.path(&cut.scene))?;
        match grade(&look, cut.id, &scene, &tuning) {
            Ok(result) => {
                if result.material_ignored {
                    report.warnings.push(format!(
                        "cut {}: this shape has a built-in material; the look's material does not apply",
                        cut.id
                    ));
                }
                graded.push((cut, result));
            }
            Err(error) => report.schema_errors.push(format!("{error:#}")),
        }
    }
    if !report.schema_errors.is_empty() {
        return fail_schema(state, report);
    }

    // The film's light is the one most of its running time is lit by.
    let mut light_seconds: BTreeMap<Light, f64> = BTreeMap::new();
    for (cut, result) in &graded {
        *light_seconds.entry(result.light).or_default() += cut.duration_seconds;
    }
    let film_light = light_seconds
        .into_iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(light, _)| light)
        .context("the shot list has no cuts")?;
    report.light = Some(film_light);

    let width = rules.preview_width.min(manifest.format.width);
    let height = (u64::from(manifest.format.height) * u64::from(width)
        / u64::from(manifest.format.width)) as u32
        & !1;
    let mut renderer: Option<Renderer> = None;
    let mut stills: Vec<(String, Vec<u8>)> = Vec::new();
    let mut frames = Vec::new();
    let directory = production.path("look");
    fs::create_dir_all(&directory).context("could not create the look directory")?;

    for (cut, result) in &graded {
        let mut base = result.scene.config.clone();
        base.render.width = width;
        base.render.height = height;
        let preview_steps = |requested: u32| requested.min(rules.preview_effect_steps);
        let settings = Settings {
            samples_per_pixel: rules.preview_samples_per_pixel,
            ambient_occlusion_steps: preview_steps(base.quality.ambient_occlusion.max_steps),
            soft_shadow_steps: preview_steps(base.quality.soft_shadow.max_steps),
            reflection_steps: preview_steps(base.quality.reflection.max_steps),
            depth_of_field: base.camera.aperture_radius > 0.0,
            pixel_epsilon_multiplier: base.render.pixel_epsilon_multiplier,
        };
        match &mut renderer {
            Some(renderer) => pollster::block_on(renderer.replace_fractal(base.clone()))
                .with_context(|| format!("could not compile the look on cut {}", cut.id))?,
            None => renderer = Some(gpu.renderer(base.clone())?),
        }
        let renderer = renderer.as_ref().expect("renderer was just initialised");

        let last = cut.frame_count - 1;
        for (position, index) in [0, last / 2, last].into_iter().enumerate() {
            let (mut config, time_seconds) =
                frame_config(&base, result.scene.animation.as_ref(), index)?;
            apply_settings(&mut config, &settings);
            let image = renderer
                .render_frame_with_config(&config, index, time_seconds)
                .with_context(|| format!("could not render cut {} frame {index}", cut.id))?;
            report.gpu_seconds += image.gpu_time().as_secs_f64();
            let label = format!("{}-{}", cut.id, position + 1);
            sheet::save_png(
                &directory.join(format!("cut-{:02}-{}.png", cut.id, position + 1)),
                image.width(),
                image.height(),
                image.pixels(),
            )?;
            frames.push(Frame::from_rgba8(
                image.width(),
                image.height(),
                image.pixels(),
            )?);
            stills.push((label, image.pixels().to_vec()));
        }
    }

    let cells: Vec<Cell<'_>> = stills
        .iter()
        .map(|(label, rgba)| Cell { label, rgba })
        .collect();
    sheet::compose(&cells, 3, width as usize, height as usize)?
        .save_png(&production.path(SHEET_FILE))?;
    report.sheet = Some(SHEET_FILE);

    let palette = classify::palette(&frames, &tuning.classification);
    report.palette = Some(palette);
    for (label, frame) in stills.iter().map(|(label, _)| label).zip(&frames) {
        let measured = legibility(frame);
        report
            .hue_problems
            .extend(hue_problem(label, &measured, &rules));
        report
            .clipping_problems
            .extend(clipping_problems(label, &measured, &rules));
        report.frames.push(FrameMeasurement {
            label: label.clone(),
            legibility: measured,
        });
    }

    let mut mismatches = Vec::new();
    if palette.primary_hue != axes.palette.primary_hue {
        mismatches.push(format!(
            "the brief commits to {} {}, but the previewed frames are {}",
            AxisId::Palette.as_str(),
            axes.palette.primary_hue.as_str(),
            palette.primary_hue.as_str()
        ));
    }
    if film_light != axes.light {
        mismatches.push(format!(
            "the brief commits to {} {}, but the cuts are lit {}",
            AxisId::Light.as_str(),
            axes.light.as_str(),
            film_light.as_str()
        ));
    }
    budget.spent.design_gpu_seconds += report.gpu_seconds;
    production.save_budget(budget)?;
    if !mismatches.is_empty() {
        report.schema_errors = mismatches;
        save_report(production, &report)?;
        return fail_schema(state, report);
    }
    state.record_gate(
        "schema",
        true,
        "look.yaml is well formed and matches the brief",
    )?;
    let record = |state: &mut State, gate: &str, problems: &[String], passed: &str| {
        state.record_gate(
            gate,
            problems.is_empty(),
            if problems.is_empty() {
                passed.to_owned()
            } else {
                problems.join("; ")
            },
        )
    };
    record(
        state,
        "hue-count",
        &report.hue_problems,
        "every frame keeps to a restrained palette",
    )?;
    record(
        state,
        "clipping",
        &report.clipping_problems,
        "no frame is blown out or crushed",
    )?;

    fs::create_dir_all(production.path("compiled/graded"))
        .context("could not create the graded directory")?;
    let mut entries = Vec::with_capacity(graded.len());
    for (cut, result) in &graded {
        let relative = format!("compiled/graded/cut-{:02}.yaml", cut.id);
        fs::write(production.path(&relative), result.scene.to_yaml()?)
            .with_context(|| format!("could not write {relative}"))?;
        entries.push(GradedEntry {
            id: cut.id,
            scene: relative,
            light: result.light,
        });
    }
    let graded_manifest = GradedManifest {
        look_hash: content_hash(&text),
        shotlist_hash: manifest.shotlist_hash,
        effects_requested: look.effects_requested,
        cuts: entries,
    };
    let mut json = serde_json::to_string_pretty(&graded_manifest)
        .context("could not serialize the graded manifest")?;
    json.push('\n');
    fs::write(production.path(GRADED_MANIFEST_FILE), json)
        .context("could not write the graded manifest")?;
    save_report(production, &report)?;
    Ok(report)
}

fn save_report(production: &Production, report: &LookReport) -> Result<()> {
    let mut json = serde_json::to_string_pretty(report).context("could not serialize report")?;
    json.push('\n');
    fs::write(production.path(REPORT_FILE), json).context("could not write the look report")
}

fn hue_problem(label: &str, measured: &Legibility, rules: &LookRules) -> Option<String> {
    (measured.hue_clusters > rules.max_hue_clusters).then(|| {
        format!(
            "frame {label} shows {} separate hues; at most {}",
            measured.hue_clusters, rules.max_hue_clusters
        )
    })
}

fn clipping_problems(label: &str, measured: &Legibility, rules: &LookRules) -> Vec<String> {
    let mut problems = Vec::new();
    if measured.clipped_high_ratio > rules.max_clipped_high {
        problems.push(format!(
            "frame {label} is blown out over {:.1}% of its area; at most {:.1}%",
            measured.clipped_high_ratio * 100.0,
            rules.max_clipped_high * 100.0
        ));
    }
    if measured.clipped_low_ratio > rules.max_clipped_low {
        problems.push(format!(
            "frame {label} is crushed to black over {:.1}% of its area; at most {:.1}%",
            measured.clipped_low_ratio * 100.0,
            rules.max_clipped_low * 100.0
        ));
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        axes::{FormulaFamily, ScaleRegime},
        cut::{
            self, MotionSpec,
            tests::{HD30, viewpoint_scene},
        },
    };

    const LOOK: &str = "\
version: 1
material:
  base_color: [0.05, 0.2, 0.25]
  accent_color: [0.9, 0.5, 0.2]
  background_bottom: [0.01, 0.01, 0.02]
  background_top: [0.05, 0.08, 0.12]
  palette:
    - { position: 0.0, color: [0.05, 0.2, 0.25] }
    - { position: 1.0, color: [0.9, 0.5, 0.2] }
atmosphere: { fog: 0.2 }
light: { azimuth_degrees: 60, elevation_degrees: 20 }
grade: { exposure_stops: -0.5, contrast: 1.1 }
per_cut:
  2: { light: { azimuth_degrees: 180, elevation_degrees: 10 }, exposure_stops: 0.25 }
effects_requested:
  ambient_occlusion: { strength: 0.6 }
  depth_of_field: { strength: 1.0 }
";

    fn parse(text: &str) -> Look {
        serde_yaml_ng::from_str(text).unwrap()
    }

    fn compiled_cut(regime: ScaleRegime) -> LoadedScene {
        let (viewpoint, survey) = viewpoint_scene(FormulaFamily::JuliaBox, regime, 3);
        cut::compile(
            &viewpoint,
            &survey,
            &MotionSpec::Orbit {
                revolutions: 0.2,
                distance: 1.0,
            },
            6.0,
            &HD30,
            "cut-01",
            &Tuning::default(),
        )
        .unwrap()
    }

    fn subject_distance(scene: &LoadedScene) -> f64 {
        let camera = &scene.config.camera;
        let (position, target) = (camera.position.to_f64(), camera.target.to_f64());
        length([0, 1, 2].map(|axis| target[axis] - position[axis]))
    }

    #[test]
    fn the_schema_rejects_quality_settings_and_unknown_fields() {
        parse(LOOK);
        for addition in [
            "samples_per_pixel: 64\n",
            "render: { width: 3840 }\n",
            "quality: { samples_per_pixel: 64 }\n",
        ] {
            assert!(
                serde_yaml_ng::from_str::<Look>(&format!("{LOOK}{addition}")).is_err(),
                "{addition}"
            );
        }
        // Step counts belong to the harness, not to the request.
        assert!(
            serde_yaml_ng::from_str::<Look>(&LOOK.replace(
                "ambient_occlusion: { strength: 0.6 }",
                "ambient_occlusion: { strength: 0.6, max_steps: 256 }"
            ))
            .is_err()
        );
    }

    #[test]
    fn light_is_placed_relative_to_the_camera() {
        let forward = [0.0, 1.0, 0.0];
        let up = [0.0, 0.0, 1.0];
        let close = |spec: LightSpec, expected: [f64; 3]| {
            let direction = light_direction(spec, forward, up).unwrap();
            (0..3).all(|axis| (direction[axis] - expected[axis]).abs() < 1.0e-9)
        };
        let spec = |azimuth_degrees, elevation_degrees| LightSpec {
            azimuth_degrees,
            elevation_degrees,
        };
        // Behind the camera, to its right, behind the subject, overhead.
        assert!(close(spec(0.0, 0.0), [0.0, -1.0, 0.0]));
        assert!(close(spec(90.0, 0.0), [1.0, 0.0, 0.0]));
        assert!(close(spec(180.0, 0.0), [0.0, 1.0, 0.0]));
        assert!(close(spec(0.0, 90.0), [0.0, 0.0, 1.0]));
        assert!(light_direction(spec(0.0, 120.0), forward, up).is_err());

        // A tilted up vector is straightened before use.
        let straightened = orthogonal_up(forward, [0.0, 0.5, 1.0]);
        assert!((0..3).all(|axis| (straightened[axis] - up[axis]).abs() < 1.0e-9));
        assert!((length(orthogonal_up(forward, forward)) - 1.0).abs() < 1.0e-9);
    }

    #[test]
    fn grading_scales_the_look_to_the_cut() {
        let look = parse(LOOK);
        let tuning = Tuning::default();
        for regime in [ScaleRegime::Mid, ScaleRegime::Deep] {
            let cut = compiled_cut(regime);
            let distance = subject_distance(&cut);
            let graded = grade(&look, 1, &cut, &tuning).unwrap();
            let config = &graded.scene.config;

            assert!(!graded.material_ignored);
            let FractalConfig::Dsl(dsl) = &config.fractal else {
                panic!("generated shapes use the DSL");
            };
            assert_eq!(dsl.material.accent_color, [0.9, 0.5, 0.2]);
            assert_eq!(dsl.material.surface_palette.len(), 2);
            // Fog and effect reach follow the subject distance, so the same
            // look reads alike at any scale.
            let fog = shape::subject_fog_density(distance, 0.2) as f32;
            assert!((dsl.material.fog_density - fog).abs() <= fog * 1.0e-5);
            let occlusion = &config.quality.ambient_occlusion;
            assert_eq!(occlusion.max_steps, tuning.look.occlusion_steps);
            assert!((f64::from(occlusion.radius) - 0.2 * distance).abs() < distance * 1.0e-5);
            assert_eq!(occlusion.strength, 0.6);
            assert!(
                (f64::from(config.camera.aperture_radius) - 0.01 * distance).abs()
                    < distance * 1.0e-5
            );
            // Effects that were not requested are off.
            assert_eq!(config.quality.soft_shadow.max_steps, 0);
            assert_eq!(config.quality.reflection.max_steps, 0);

            assert!(config.quality.tone_mapping.enabled);
            assert_eq!(config.quality.tone_mapping.exposure_stops, -0.5);
            assert_eq!(config.quality.post_process.contrast, 1.1);
            // The path is untouched.
            assert_eq!(
                graded.scene.animation.as_ref().unwrap().frame_count,
                cut.animation.as_ref().unwrap().frame_count
            );
        }
    }

    #[test]
    fn per_cut_overrides_change_light_and_exposure_only_there() {
        let look = parse(LOOK);
        let tuning = Tuning::default();
        let cut = compiled_cut(ScaleRegime::Mid);
        let first = grade(&look, 1, &cut, &tuning).unwrap();
        let second = grade(&look, 2, &cut, &tuning).unwrap();
        assert_eq!(first.light, Light::Raking);
        assert_eq!(second.light, Light::Backlit);
        assert_eq!(
            second.scene.config.quality.tone_mapping.exposure_stops,
            -0.25
        );
        assert_ne!(
            first.scene.config.light.direction,
            second.scene.config.light.direction
        );
    }

    #[test]
    fn unrenderable_looks_are_rejected() {
        let cut = compiled_cut(ScaleRegime::Mid);
        let tuning = Tuning::default();
        for (from, to) in [
            (
                "base_color: [0.05, 0.2, 0.25]",
                "base_color: [-1.0, 0.2, 0.25]",
            ),
            ("exposure_stops: -0.5", "exposure_stops: 99"),
            ("elevation_degrees: 20", "elevation_degrees: 200"),
        ] {
            let look = parse(&LOOK.replace(from, to));
            assert!(grade(&look, 1, &cut, &tuning).is_err(), "{to}");
        }
    }

    #[test]
    fn frame_problems_follow_the_rules() {
        let rules = LookRules::default();
        let mut measured = Legibility {
            high_frequency_ratio: 0.1,
            negative_space_ratio: 0.4,
            hue_clusters: 2,
            clipped_high_ratio: 0.0,
            clipped_low_ratio: 0.3,
            mean_luminance: 0.4,
        };
        assert!(hue_problem("1-1", &measured, &rules).is_none());
        assert!(clipping_problems("1-1", &measured, &rules).is_empty());

        measured.hue_clusters = 5;
        measured.clipped_high_ratio = 0.1;
        measured.clipped_low_ratio = 0.8;
        assert!(
            hue_problem("1-1", &measured, &rules)
                .unwrap()
                .contains("5 separate hues")
        );
        let clipping = clipping_problems("1-1", &measured, &rules).join("\n");
        assert!(clipping.contains("blown out") && clipping.contains("crushed"));

        let tolerant = LookRules {
            max_hue_clusters: 6,
            max_clipped_high: 0.5,
            max_clipped_low: 0.9,
            ..rules
        };
        assert!(hue_problem("1-1", &measured, &tolerant).is_none());
        assert!(clipping_problems("1-1", &measured, &tolerant).is_empty());
    }
}
