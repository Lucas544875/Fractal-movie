use std::fmt::Write;

use anyhow::{Result, bail};

use crate::config::MAX_FRACTAL_ITERATIONS;

pub const MAX_DSL_TRANSFORMS: usize = 16;
pub const MAX_DSL_PALETTE_STOPS: usize = 16;
pub const MAX_DSL_COLOR_ITERATIONS: u32 = 512;

/// A deliberately bounded fractal program. Scene files can construct this AST
/// but cannot inject identifiers, statements, or arbitrary WGSL source.
#[derive(Clone, Debug, PartialEq)]
pub struct DslFractalConfig {
    pub iterations: u32,
    /// Optional period for scheduled hybrid transforms. Mandelbulber repeats
    /// its formula sequence independently of the render iteration limit.
    pub orbit_period: Option<u32>,
    /// Orbit iterations used for surface coloring. Mandelbulber evaluates
    /// coloring at four times the geometry iteration limit.
    pub color_iterations: u32,
    pub bailout: Option<f32>,
    pub normal_epsilon: f32,
    pub orbit: Vec<OrbitTransform>,
    pub material: DslMaterial,
}

#[derive(Clone, Debug, PartialEq)]
pub enum OrbitTransform {
    /// Kali's Amazing Surf fold, a PseudoKleinian-derived transform used by
    /// Mandelbulber hybrids. Only X/Y are folded; the radial inversion and DE
    /// derivative update are part of the same bounded operation.
    AmazingSurfFold {
        start_iteration: u32,
        stop_iteration: u32,
        limits: [f32; 2],
        minimum_radius_squared: f32,
        scale: f32,
        rotation_degrees: [f32; 3],
    },
    /// One scheduled Mandelbox step followed by a fixed Julia constant. This
    /// matches Mandelbulber's hybrid sequencing without accepting raw code.
    MandelboxJuliaFold {
        start_iteration: u32,
        stop_iteration: u32,
        fold_limit: f32,
        min_radius_squared: f32,
        fixed_radius_squared: f32,
        scale: f32,
        constant: [f32; 3],
        rotation_degrees: [f32; 3],
    },
    BoxFold {
        limit: f32,
    },
    SphereFold {
        min_radius_squared: f32,
        fixed_radius_squared: f32,
    },
    /// `z = scale * z + p`, including the derivative update required by the
    /// generated distance estimator.
    ScaleAddPoint {
        scale: f32,
    },
    /// Julia-style affine recurrence with a fixed constant.
    ScaleAddConstant {
        scale: f32,
        constant: [f32; 3],
    },
    Rotate {
        axis: [f32; 3],
        degrees: f32,
    },
    Translate {
        offset: [f32; 3],
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct DslMaterial {
    pub base_color: [f32; 3],
    pub accent_color: [f32; 3],
    pub specular_color: [f32; 3],
    pub background_bottom: [f32; 3],
    pub background_top: [f32; 3],
    pub color_frequency: f32,
    /// Optional piecewise-linear surface palette. An empty palette retains the
    /// legacy two-color interpolation between `base_color` and `accent_color`.
    pub surface_palette: Vec<DslPaletteStop>,
    /// Blend weight from the legacy spatial palette coordinate to the orbit
    /// coloring accumulated by Mandelbox folds.
    pub orbit_palette_weight: f32,
    /// Cyclic offset applied after selecting the spatial/orbit coordinate.
    pub palette_offset: f32,
    /// Blend weight from world coordinates to camera-relative coordinates.
    /// The latter keeps palette detail at a stable apparent scale while zooming.
    pub camera_palette_weight: f32,
    /// Blend weight from position-based palette coordinates to normal-based
    /// coordinates. Higher values retain color variation during deep zooms.
    pub normal_palette_weight: f32,
    pub ambient_strength: f32,
    pub diffuse_strength: f32,
    pub specular_strength: f32,
    pub shininess: f32,
    /// A second, broader highlight tinted by the sampled surface palette.
    pub metallic_specular_strength: f32,
    pub metallic_shininess: f32,
    pub rim_strength: f32,
    pub fog_density: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DslPaletteStop {
    pub position: f32,
    pub color: [f32; 3],
}

impl Default for DslMaterial {
    fn default() -> Self {
        Self {
            base_color: [0.08, 0.24, 0.52],
            accent_color: [1.05, 0.32, 0.08],
            specular_color: [1.0, 0.82, 0.58],
            background_bottom: [0.008, 0.012, 0.025],
            background_top: [0.08, 0.13, 0.22],
            color_frequency: 1.8,
            surface_palette: Vec::new(),
            orbit_palette_weight: 0.0,
            palette_offset: 0.0,
            camera_palette_weight: 0.0,
            normal_palette_weight: 0.55,
            ambient_strength: 0.18,
            diffuse_strength: 0.95,
            specular_strength: 0.35,
            shininess: 48.0,
            metallic_specular_strength: 0.0,
            metallic_shininess: 30.0,
            rim_strength: 0.18,
            fog_density: 0.0,
        }
    }
}

impl Default for DslFractalConfig {
    fn default() -> Self {
        Self {
            iterations: 16,
            orbit_period: None,
            color_iterations: 16,
            bailout: None,
            normal_epsilon: 1.0e-5,
            orbit: vec![
                OrbitTransform::BoxFold { limit: 1.14 },
                OrbitTransform::SphereFold {
                    min_radius_squared: 0.60,
                    fixed_radius_squared: 2.65,
                },
                OrbitTransform::ScaleAddPoint { scale: -2.18 },
            ],
            material: DslMaterial::default(),
        }
    }
}

impl DslFractalConfig {
    pub fn validate(&self) -> Result<()> {
        if self.iterations == 0 || self.iterations > MAX_FRACTAL_ITERATIONS {
            bail!("DSL iterations must be in 1..={MAX_FRACTAL_ITERATIONS}");
        }
        if self.color_iterations == 0 || self.color_iterations > MAX_DSL_COLOR_ITERATIONS {
            bail!("DSL color_iterations must be in 1..={MAX_DSL_COLOR_ITERATIONS}");
        }
        if let Some(period) = self.orbit_period {
            if period == 0 || period > MAX_DSL_COLOR_ITERATIONS {
                bail!("DSL orbit_period must be in 1..={MAX_DSL_COLOR_ITERATIONS}");
            }
        }
        if let Some(bailout) = self.bailout {
            finite_range("DSL bailout", bailout, 1.0e-6, 1.0e6)?;
        }
        finite_range("DSL normal_epsilon", self.normal_epsilon, 1.0e-12, 0.099)?;
        if self.orbit.is_empty() || self.orbit.len() > MAX_DSL_TRANSFORMS {
            bail!("DSL orbit must contain 1..={MAX_DSL_TRANSFORMS} transforms");
        }

        let schedule_length = self.orbit_period.unwrap_or(self.iterations);
        let mut recurrence_count = 0usize;
        for (index, transform) in self.orbit.iter().enumerate() {
            match transform {
                OrbitTransform::AmazingSurfFold {
                    start_iteration,
                    stop_iteration,
                    limits,
                    minimum_radius_squared,
                    scale,
                    rotation_degrees,
                } => {
                    validate_iteration_range(
                        index,
                        *start_iteration,
                        *stop_iteration,
                        schedule_length,
                    )?;
                    if limits
                        .iter()
                        .any(|limit| !limit.is_finite() || !(1.0e-6..=100.0).contains(limit))
                    {
                        bail!(
                            "DSL orbit[{index}] amazing-surf-fold limits must be finite and in 1e-6..=100"
                        );
                    }
                    finite_range(
                        &format!("DSL orbit[{index}] amazing-surf-fold minimum radius squared"),
                        *minimum_radius_squared,
                        0.0,
                        1.0,
                    )?;
                    if !scale.is_finite() || !(1.0e-3..=4.0).contains(&scale.abs()) {
                        bail!(
                            "DSL orbit[{index}] amazing-surf-fold scale magnitude must be in 0.001..=4.0"
                        );
                    }
                    finite_vec3(
                        &format!("DSL orbit[{index}] amazing-surf-fold rotation"),
                        *rotation_degrees,
                    )?;
                    if rotation_degrees
                        .iter()
                        .any(|degrees| degrees.abs() > 3_600.0)
                    {
                        bail!(
                            "DSL orbit[{index}] amazing-surf-fold rotation must be in -3600..=3600"
                        );
                    }
                }
                OrbitTransform::MandelboxJuliaFold {
                    start_iteration,
                    stop_iteration,
                    fold_limit,
                    min_radius_squared,
                    fixed_radius_squared,
                    scale,
                    constant,
                    rotation_degrees,
                } => {
                    validate_iteration_range(
                        index,
                        *start_iteration,
                        *stop_iteration,
                        schedule_length,
                    )?;
                    finite_range(
                        &format!("DSL orbit[{index}] Mandelbox-Julia fold limit"),
                        *fold_limit,
                        1.0e-6,
                        100.0,
                    )?;
                    finite_range(
                        &format!("DSL orbit[{index}] Mandelbox-Julia minimum radius squared"),
                        *min_radius_squared,
                        1.0e-12,
                        1.0e6,
                    )?;
                    finite_range(
                        &format!("DSL orbit[{index}] Mandelbox-Julia fixed radius squared"),
                        *fixed_radius_squared,
                        1.0e-12,
                        1.0e6,
                    )?;
                    if fixed_radius_squared <= min_radius_squared {
                        bail!(
                            "DSL orbit[{index}] Mandelbox-Julia fixed radius squared must exceed minimum radius squared"
                        );
                    }
                    if !scale.is_finite() || !(1.000_001..=4.0).contains(&scale.abs()) {
                        bail!(
                            "DSL orbit[{index}] Mandelbox-Julia scale magnitude must be in 1.000001..=4.0"
                        );
                    }
                    finite_vec3(
                        &format!("DSL orbit[{index}] Mandelbox-Julia constant"),
                        *constant,
                    )?;
                    finite_vec3(
                        &format!("DSL orbit[{index}] Mandelbox-Julia rotation"),
                        *rotation_degrees,
                    )?;
                    if constant.iter().any(|component| component.abs() > 100.0)
                        || rotation_degrees
                            .iter()
                            .any(|degrees| degrees.abs() > 3_600.0)
                    {
                        bail!(
                            "DSL orbit[{index}] Mandelbox-Julia constant or rotation is outside the supported range"
                        );
                    }
                }
                OrbitTransform::BoxFold { limit } => {
                    finite_range(
                        &format!("DSL orbit[{index}] box-fold limit"),
                        *limit,
                        1.0e-6,
                        100.0,
                    )?;
                }
                OrbitTransform::SphereFold {
                    min_radius_squared,
                    fixed_radius_squared,
                } => {
                    finite_range(
                        &format!("DSL orbit[{index}] minimum radius squared"),
                        *min_radius_squared,
                        1.0e-12,
                        1.0e6,
                    )?;
                    finite_range(
                        &format!("DSL orbit[{index}] fixed radius squared"),
                        *fixed_radius_squared,
                        1.0e-12,
                        1.0e6,
                    )?;
                    if fixed_radius_squared <= min_radius_squared {
                        bail!(
                            "DSL orbit[{index}] fixed radius squared must exceed minimum radius squared"
                        );
                    }
                }
                OrbitTransform::ScaleAddPoint { scale } => {
                    if !scale.is_finite() || !(1.000_001..=4.0).contains(&scale.abs()) {
                        bail!(
                            "DSL orbit[{index}] scale-add-point magnitude must be in 1.000001..=4.0"
                        );
                    }
                    recurrence_count += 1;
                }
                OrbitTransform::ScaleAddConstant { scale, constant } => {
                    if !scale.is_finite() || !(1.000_001..=4.0).contains(&scale.abs()) {
                        bail!(
                            "DSL orbit[{index}] scale-add-constant magnitude must be in 1.000001..=4.0"
                        );
                    }
                    finite_vec3(
                        &format!("DSL orbit[{index}] scale-add-constant constant"),
                        *constant,
                    )?;
                    if constant.iter().any(|component| component.abs() > 100.0) {
                        bail!(
                            "DSL orbit[{index}] scale-add-constant components must be in -100..=100"
                        );
                    }
                    recurrence_count += 1;
                }
                OrbitTransform::Rotate { axis, degrees } => {
                    finite_vec3(&format!("DSL orbit[{index}] rotation axis"), *axis)?;
                    if squared_length(*axis) < 1.0e-12 {
                        bail!("DSL orbit[{index}] rotation axis must not be zero");
                    }
                    finite_range(
                        &format!("DSL orbit[{index}] rotation degrees"),
                        *degrees,
                        -3_600.0,
                        3_600.0,
                    )?;
                }
                OrbitTransform::Translate { offset } => {
                    finite_vec3(&format!("DSL orbit[{index}] translation"), *offset)?;
                    if offset.iter().any(|component| component.abs() > 100.0) {
                        bail!("DSL orbit[{index}] translation components must be in -100..=100");
                    }
                }
            }
        }
        let has_self_recurrence = self.orbit.iter().any(|transform| {
            matches!(
                transform,
                OrbitTransform::AmazingSurfFold { .. } | OrbitTransform::MandelboxJuliaFold { .. }
            )
        });
        if recurrence_count > 1 || (recurrence_count == 0 && !has_self_recurrence) {
            bail!(
                "DSL orbit must contain one affine recurrence, unless an amazing-surf-fold supplies the recurrence"
            );
        }
        self.material.validate()
    }

    /// Generates the complete fractal shader fragment consumed by the common
    /// camera, shading, and ray-marching modules.
    pub fn generate_wgsl(&self) -> Result<String> {
        self.validate()?;
        let mut source = String::with_capacity(8_192);
        writeln!(
            source,
            "const MAX_DSL_ORBIT_ITERATIONS: u32 = {MAX_DSL_COLOR_ITERATIONS}u;"
        )?;

        if self.orbit.iter().any(|transform| {
            matches!(
                transform,
                OrbitTransform::Rotate { .. }
                    | OrbitTransform::AmazingSurfFold { .. }
                    | OrbitTransform::MandelboxJuliaFold { .. }
            )
        }) {
            source.push_str(
                "fn dsl_rotate(value: vec3<f32>, axis_value: vec3<f32>, radians: f32) -> vec3<f32> {\n\
                 \x20   let axis = safe_normalize(axis_value, vec3<f32>(0.0, 1.0, 0.0));\n\
                 \x20   let cosine = cos(radians);\n\
                 \x20   let sine = sin(radians);\n\
                 \x20   return value * cosine + cross(axis, value) * sine\n\
                 \x20       + axis * dot(axis, value) * (1.0 - cosine);\n\
                 }\n\n",
            );
        }

        source.push_str(
            "struct DslOrbitResult {\n\
             \x20   distance: f32,\n\
             \x20   color_coordinate: f32,\n\
             }\n\n\
             fn dsl_orbit(\n\
             \x20   p: vec3<f32>,\n\
             \x20   iteration_limit: u32,\n\
             \x20   geometry_bailout: bool,\n\
             ) -> DslOrbitResult {\n\
             \x20   var z = p;\n\
             \x20   var derivative = 1.0;\n\
             \x20   var auxiliary_color = 1.0;\n\
             \x20   for (var iteration = 0u; iteration < MAX_DSL_ORBIT_ITERATIONS; iteration += 1u) {\n\
             \x20       if iteration >= iteration_limit { break; }\n",
        );

        if let Some(period) = self.orbit_period {
            writeln!(
                source,
                "        let scheduled_iteration = iteration % {period}u;"
            )?;
        } else {
            source.push_str("        let scheduled_iteration = iteration;\n");
        }
        source.push_str("        var color_bailout_allowed = false;\n");

        for (index, transform) in self.orbit.iter().enumerate() {
            match transform {
                OrbitTransform::AmazingSurfFold {
                    start_iteration,
                    stop_iteration,
                    limits,
                    minimum_radius_squared,
                    scale,
                    rotation_degrees,
                } => {
                    let limit_x = wgsl_float(limits[0]);
                    let limit_y = wgsl_float(limits[1]);
                    let minimum = wgsl_float(*minimum_radius_squared);
                    let scale = wgsl_float(*scale);
                    writeln!(
                        source,
                        "        if scheduled_iteration >= {start_iteration}u && scheduled_iteration < {stop_iteration}u {{"
                    )?;
                    source.push_str("            color_bailout_allowed = true;\n");
                    writeln!(
                        source,
                        "            z.x = abs(z.x + {limit_x}) - abs(z.x - {limit_x}) - z.x;"
                    )?;
                    writeln!(
                        source,
                        "            z.y = abs(z.y + {limit_y}) - abs(z.y - {limit_y}) - z.y;"
                    )?;
                    writeln!(
                        source,
                        "            let surf_radius_squared_{index} = dot(z, z);"
                    )?;
                    writeln!(
                        source,
                        "            let surf_divisor_{index} = clamp(surf_radius_squared_{index}, max({minimum}, 1.0e-12), 1.0);"
                    )?;
                    writeln!(
                        source,
                        "            let surf_multiplier_{index} = {scale} / surf_divisor_{index};"
                    )?;
                    writeln!(source, "            z *= surf_multiplier_{index};")?;
                    writeln!(
                        source,
                        "            derivative = derivative * abs(surf_multiplier_{index}) + 1.0;"
                    )?;
                    for (axis, degrees) in [
                        ([1.0, 0.0, 0.0], rotation_degrees[0]),
                        ([0.0, 1.0, 0.0], rotation_degrees[1]),
                        ([0.0, 0.0, 1.0], rotation_degrees[2]),
                    ] {
                        writeln!(
                            source,
                            "            z = dsl_rotate(z, {}, {});",
                            wgsl_vec3(axis),
                            wgsl_float(degrees.to_radians()),
                        )?;
                    }
                    source.push_str("        }\n");
                }
                OrbitTransform::MandelboxJuliaFold {
                    start_iteration,
                    stop_iteration,
                    fold_limit,
                    min_radius_squared,
                    fixed_radius_squared,
                    scale,
                    constant,
                    rotation_degrees,
                } => {
                    let fold_limit = wgsl_float(*fold_limit);
                    let minimum = wgsl_float(*min_radius_squared);
                    let fixed = wgsl_float(*fixed_radius_squared);
                    let scale = wgsl_float(*scale);
                    writeln!(
                        source,
                        "        if scheduled_iteration >= {start_iteration}u && scheduled_iteration < {stop_iteration}u {{"
                    )?;
                    writeln!(
                        source,
                        "            auxiliary_color += select(0.0, 0.03, abs(z.x) > {fold_limit});"
                    )?;
                    writeln!(
                        source,
                        "            auxiliary_color += select(0.0, 0.05, abs(z.y) > {fold_limit});"
                    )?;
                    writeln!(
                        source,
                        "            auxiliary_color += select(0.0, 0.07, abs(z.z) > {fold_limit});"
                    )?;
                    writeln!(
                        source,
                        "            z = clamp(z, vec3<f32>(-{fold_limit}), vec3<f32>({fold_limit})) * 2.0 - z;"
                    )?;
                    writeln!(
                        source,
                        "            let julia_radius_squared_{index} = dot(z, z);"
                    )?;
                    writeln!(
                        source,
                        "            if julia_radius_squared_{index} < {minimum} {{"
                    )?;
                    writeln!(
                        source,
                        "                let julia_factor_{index} = {fixed} / {minimum};"
                    )?;
                    writeln!(source, "                z *= julia_factor_{index};")?;
                    writeln!(
                        source,
                        "                derivative *= julia_factor_{index};"
                    )?;
                    source.push_str("                auxiliary_color += 0.2;\n");
                    writeln!(
                        source,
                        "            }} else if julia_radius_squared_{index} < {fixed} {{"
                    )?;
                    writeln!(
                        source,
                        "                let julia_factor_{index} = {fixed} / max(julia_radius_squared_{index}, 1.0e-12);"
                    )?;
                    writeln!(source, "                z *= julia_factor_{index};")?;
                    writeln!(
                        source,
                        "                derivative *= julia_factor_{index};"
                    )?;
                    source.push_str("                auxiliary_color += 0.2;\n");
                    source.push_str("            }\n");
                    for (axis, degrees) in [
                        ([1.0, 0.0, 0.0], rotation_degrees[0]),
                        ([0.0, 1.0, 0.0], rotation_degrees[1]),
                        ([0.0, 0.0, 1.0], rotation_degrees[2]),
                    ] {
                        writeln!(
                            source,
                            "            z = dsl_rotate(z, {}, {});",
                            wgsl_vec3(axis),
                            wgsl_float(degrees.to_radians()),
                        )?;
                    }
                    writeln!(
                        source,
                        "            z = {scale} * z + {};",
                        wgsl_vec3(*constant)
                    )?;
                    writeln!(
                        source,
                        "            derivative = derivative * abs({scale}) + 1.0;"
                    )?;
                    source.push_str("        }\n");
                }
                OrbitTransform::BoxFold { limit } => {
                    let limit = wgsl_float(*limit);
                    writeln!(
                        source,
                        "        auxiliary_color += select(0.0, 0.03, abs(z.x) > {limit});"
                    )?;
                    writeln!(
                        source,
                        "        auxiliary_color += select(0.0, 0.05, abs(z.y) > {limit});"
                    )?;
                    writeln!(
                        source,
                        "        auxiliary_color += select(0.0, 0.07, abs(z.z) > {limit});"
                    )?;
                    writeln!(
                        source,
                        "        z = clamp(z, vec3<f32>(-{limit}), vec3<f32>({limit})) * 2.0 - z;"
                    )?;
                }
                OrbitTransform::SphereFold {
                    min_radius_squared,
                    fixed_radius_squared,
                } => {
                    let minimum = wgsl_float(*min_radius_squared);
                    let fixed = wgsl_float(*fixed_radius_squared);
                    writeln!(source, "        let radius_squared_{index} = dot(z, z);")?;
                    writeln!(source, "        if radius_squared_{index} < {minimum} {{")?;
                    writeln!(
                        source,
                        "            let factor_{index} = {fixed} / {minimum};"
                    )?;
                    writeln!(source, "            z *= factor_{index};")?;
                    writeln!(source, "            derivative *= factor_{index};")?;
                    source.push_str("            auxiliary_color += 0.2;\n");
                    writeln!(
                        source,
                        "        }} else if radius_squared_{index} < {fixed} {{"
                    )?;
                    writeln!(
                        source,
                        "            let factor_{index} = {fixed} / max(radius_squared_{index}, 1.0e-12);"
                    )?;
                    writeln!(source, "            z *= factor_{index};")?;
                    writeln!(source, "            derivative *= factor_{index};")?;
                    source.push_str("            auxiliary_color += 0.2;\n");
                    source.push_str("        }\n");
                }
                OrbitTransform::ScaleAddPoint { scale } => {
                    let scale = wgsl_float(*scale);
                    writeln!(source, "        z = {scale} * z + p;")?;
                    writeln!(
                        source,
                        "        derivative = derivative * abs({scale}) + 1.0;"
                    )?;
                }
                OrbitTransform::ScaleAddConstant { scale, constant } => {
                    let scale = wgsl_float(*scale);
                    writeln!(
                        source,
                        "        z = {scale} * z + {};",
                        wgsl_vec3(*constant)
                    )?;
                    writeln!(
                        source,
                        "        derivative = derivative * abs({scale}) + 1.0;"
                    )?;
                }
                OrbitTransform::Rotate { axis, degrees } => {
                    writeln!(
                        source,
                        "        z = dsl_rotate(z, {}, {});",
                        wgsl_vec3(*axis),
                        wgsl_float(degrees.to_radians()),
                    )?;
                }
                OrbitTransform::Translate { offset } => {
                    writeln!(source, "        z += {};", wgsl_vec3(*offset))?;
                }
            }
        }
        if let Some(bailout) = self.bailout {
            writeln!(
                source,
                "        if dot(z, z) > {} && (geometry_bailout || color_bailout_allowed) {{ break; }}",
                wgsl_float(bailout * bailout)
            )?;
        }
        source.push_str(
            "    }\n\
             \x20   return DslOrbitResult(\n\
             \x20       length(z) / max(abs(derivative), 1.0e-12),\n\
             \x20       fract(auxiliary_color * 0.1),\n\
             \x20   );\n\
             }\n\n\
             fn map(p: vec3<f32>) -> f32 {\n\
             \x20   return dsl_orbit(p, uniforms.limits.x, true).distance;\n\
             }\n\n",
        );

        self.generate_material_wgsl(&mut source)?;
        Ok(source)
    }

    fn generate_material_wgsl(&self, source: &mut String) -> Result<()> {
        let material = &self.material;
        writeln!(
            source,
            "fn fractal_normal_epsilon(hit_epsilon: f32, base_epsilon: f32) -> f32 {{\n    return max(max(hit_epsilon * 0.5, base_epsilon), {});\n}}\n",
            wgsl_float(self.normal_epsilon),
        )?;

        if material.surface_palette.is_empty() {
            writeln!(
                source,
                "fn dsl_surface_palette(coordinate: f32) -> vec3<f32> {{\n    return mix({}, {}, clamp(coordinate, 0.0, 1.0));\n}}\n",
                wgsl_vec3(material.base_color),
                wgsl_vec3(material.accent_color),
            )?;
        } else {
            source.push_str(
                "fn dsl_surface_palette(coordinate_value: f32) -> vec3<f32> {\n\
                 \x20   let coordinate = clamp(coordinate_value, 0.0, 1.0);\n",
            );
            for pair in material.surface_palette.windows(2) {
                let first = &pair[0];
                let second = &pair[1];
                writeln!(
                    source,
                    "    if coordinate <= {} {{",
                    wgsl_float(second.position)
                )?;
                writeln!(
                    source,
                    "        let amount = clamp((coordinate - {}) / {}, 0.0, 1.0);",
                    wgsl_float(first.position),
                    wgsl_float(second.position - first.position),
                )?;
                writeln!(
                    source,
                    "        return mix({}, {}, amount);",
                    wgsl_vec3(first.color),
                    wgsl_vec3(second.color),
                )?;
                source.push_str("    }\n");
            }
            writeln!(
                source,
                "    return {};\n}}\n",
                wgsl_vec3(
                    material
                        .surface_palette
                        .last()
                        .expect("validated non-empty palette")
                        .color
                ),
            )?;
        }

        writeln!(
            source,
            "fn shade_fractal(\n    p: vec3<f32>,\n    ray_direction: vec3<f32>,\n    normal: vec3<f32>,\n    step_ratio: f32,\n    light: vec3<f32>,\n    direct_visibility: f32,\n    ambient_visibility: f32,\n) -> vec3<f32> {{\n    let world_palette = 0.5 + 0.5 * sin(p.z * {});\n    let basis = camera_basis();\n    let camera_relative_position = (p - uniforms.camera_target.xyz)\n        / max(uniforms.camera_lens.y, 1.0e-12);\n    let camera_palette_coordinate = dot(\n        camera_relative_position,\n        0.7 * basis.right + basis.up,\n    );\n    let camera_palette = 0.5 + 0.5 * sin(camera_palette_coordinate * {});\n    let position_palette = mix(world_palette, camera_palette, {});\n    let normal_palette = 0.5 + 0.5 * normal.z;\n    let spatial_palette = mix(position_palette, normal_palette, {});\n    let orbit_palette = dsl_orbit(p, {}u, false).color_coordinate;\n    let palette = fract(mix(spatial_palette, orbit_palette, {}) + {});\n    let base = dsl_surface_palette(palette);\n    let diffuse = max(dot(normal, light), 0.0);\n    let half_vector = safe_normalize(light - ray_direction, light);\n    let half_alignment = max(dot(normal, half_vector), 0.0);\n    let specular = pow(half_alignment, {});\n    let metallic_specular = pow(half_alignment, {});\n    let rim = pow(1.0 - max(dot(normal, -ray_direction), 0.0), 2.5);\n    let march_occlusion = mix(1.0, 0.76, clamp(step_ratio, 0.0, 1.0));\n    return max(\n        base * ({} * ambient_visibility + {} * diffuse * direct_visibility) * march_occlusion\n            + {} * specular * {} * direct_visibility\n            + base * metallic_specular * {} * direct_visibility\n            + base * rim * {} * ambient_visibility,\n        vec3<f32>(0.0),\n    );\n}}\n",
            wgsl_float(material.color_frequency),
            wgsl_float(material.color_frequency),
            wgsl_float(material.camera_palette_weight),
            wgsl_float(material.normal_palette_weight),
            self.color_iterations,
            wgsl_float(material.orbit_palette_weight),
            wgsl_float(material.palette_offset),
            wgsl_float(material.shininess),
            wgsl_float(material.metallic_shininess),
            wgsl_float(material.ambient_strength),
            wgsl_float(material.diffuse_strength),
            wgsl_vec3(material.specular_color),
            wgsl_float(material.specular_strength),
            wgsl_float(material.metallic_specular_strength),
            wgsl_float(material.rim_strength),
        )?;
        writeln!(
            source,
            "fn fractal_background(ray_direction: vec3<f32>) -> vec3<f32> {{\n    let horizon = clamp(0.5 + 0.5 * ray_direction.y, 0.0, 1.0);\n    return mix({}, {}, horizon);\n}}\n",
            wgsl_vec3(material.background_bottom),
            wgsl_vec3(material.background_top),
        )?;
        writeln!(
            source,
            "fn apply_fractal_atmosphere(surface: vec3<f32>, sky: vec3<f32>, travel: f32) -> vec3<f32> {{\n    let fog = 1.0 - exp(-{} * travel * travel);\n    return mix(surface, sky, fog);\n}}",
            wgsl_float(material.fog_density),
        )?;
        Ok(())
    }
}

impl DslMaterial {
    fn validate(&self) -> Result<()> {
        validate_color("DSL material base_color", self.base_color)?;
        validate_color("DSL material accent_color", self.accent_color)?;
        validate_color("DSL material specular_color", self.specular_color)?;
        validate_color("DSL material background_bottom", self.background_bottom)?;
        validate_color("DSL material background_top", self.background_top)?;
        finite_range(
            "DSL material color_frequency",
            self.color_frequency,
            0.0,
            1_000.0,
        )?;
        if !self.surface_palette.is_empty() {
            if !(2..=MAX_DSL_PALETTE_STOPS).contains(&self.surface_palette.len()) {
                bail!(
                    "DSL material surface_palette must contain 2..={MAX_DSL_PALETTE_STOPS} stops"
                );
            }
            for (index, stop) in self.surface_palette.iter().enumerate() {
                finite_range(
                    &format!("DSL material surface_palette[{index}] position"),
                    stop.position,
                    0.0,
                    1.0,
                )?;
                validate_color(
                    &format!("DSL material surface_palette[{index}] color"),
                    stop.color,
                )?;
                if index > 0 && stop.position <= self.surface_palette[index - 1].position {
                    bail!("DSL material surface_palette positions must be strictly increasing");
                }
            }
            let first = self.surface_palette.first().expect("validated palette");
            let last = self.surface_palette.last().expect("validated palette");
            if first.position != 0.0 || last.position != 1.0 {
                bail!("DSL material surface_palette must start at 0.0 and end at 1.0");
            }
        }
        finite_range(
            "DSL material orbit_palette_weight",
            self.orbit_palette_weight,
            0.0,
            1.0,
        )?;
        finite_range("DSL material palette_offset", self.palette_offset, 0.0, 1.0)?;
        finite_range(
            "DSL material camera_palette_weight",
            self.camera_palette_weight,
            0.0,
            1.0,
        )?;
        finite_range(
            "DSL material normal_palette_weight",
            self.normal_palette_weight,
            0.0,
            1.0,
        )?;
        finite_range(
            "DSL material ambient_strength",
            self.ambient_strength,
            0.0,
            16.0,
        )?;
        finite_range(
            "DSL material diffuse_strength",
            self.diffuse_strength,
            0.0,
            16.0,
        )?;
        finite_range(
            "DSL material specular_strength",
            self.specular_strength,
            0.0,
            16.0,
        )?;
        finite_range("DSL material shininess", self.shininess, 1.0, 512.0)?;
        finite_range(
            "DSL material metallic_specular_strength",
            self.metallic_specular_strength,
            0.0,
            16.0,
        )?;
        finite_range(
            "DSL material metallic_shininess",
            self.metallic_shininess,
            1.0,
            512.0,
        )?;
        finite_range("DSL material rim_strength", self.rim_strength, 0.0, 16.0)?;
        finite_range("DSL material fog_density", self.fog_density, 0.0, 100.0)
    }
}

fn validate_color(name: &str, color: [f32; 3]) -> Result<()> {
    finite_vec3(name, color)?;
    if color
        .iter()
        .any(|component| !(0.0..=64.0).contains(component))
    {
        bail!("{name} components must be in 0.0..=64.0");
    }
    Ok(())
}

fn validate_iteration_range(index: usize, start: u32, stop: u32, iterations: u32) -> Result<()> {
    if start >= stop || stop > iterations {
        bail!("DSL orbit[{index}] iteration range must satisfy start < stop <= fractal iterations");
    }
    Ok(())
}

fn finite_vec3(name: &str, value: [f32; 3]) -> Result<()> {
    if value.iter().any(|component| !component.is_finite()) {
        bail!("{name} must contain only finite values");
    }
    Ok(())
}

fn finite_range(name: &str, value: f32, minimum: f32, maximum: f32) -> Result<()> {
    if !value.is_finite() || !(minimum..=maximum).contains(&value) {
        bail!("{name} must be finite and in {minimum}..={maximum}");
    }
    Ok(())
}

fn squared_length(value: [f32; 3]) -> f32 {
    value.iter().map(|component| component * component).sum()
}

fn wgsl_float(value: f32) -> String {
    format!("{value:.9e}")
}

fn wgsl_vec3(value: [f32; 3]) -> String {
    format!(
        "vec3<f32>({}, {}, {})",
        wgsl_float(value[0]),
        wgsl_float(value[1]),
        wgsl_float(value[2]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_program_validates_and_generates_the_shader_contract() {
        let program = DslFractalConfig::default();
        let source = program.generate_wgsl().expect("default DSL must compile");
        assert!(source.contains("fn map("));
        assert!(source.contains("fn shade_fractal("));
        assert!(source.contains("radius_squared_1"));
        assert!(source.contains("derivative = derivative * abs("));
        assert!(!source.contains("@group"));
    }

    #[test]
    fn rejects_unbounded_or_invalid_programs() {
        let mut program = DslFractalConfig {
            orbit: vec![OrbitTransform::Translate {
                offset: [0.0, 0.0, 0.0],
            }],
            ..DslFractalConfig::default()
        };
        assert!(program.validate().is_err());

        program = DslFractalConfig::default();
        program
            .orbit
            .extend((0..MAX_DSL_TRANSFORMS).map(|_| OrbitTransform::Translate {
                offset: [0.0, 0.0, 0.0],
            }));
        assert!(program.validate().is_err());

        program = DslFractalConfig::default();
        program.material.base_color[0] = f32::NAN;
        assert!(program.validate().is_err());
    }

    #[test]
    fn generates_only_fixed_identifiers_for_rotation_and_translation() {
        let mut program = DslFractalConfig::default();
        program.orbit.insert(
            0,
            OrbitTransform::Rotate {
                axis: [0.0, 0.0, 1.0],
                degrees: 17.0,
            },
        );
        program.orbit.insert(
            1,
            OrbitTransform::Translate {
                offset: [0.1, -0.2, 0.3],
            },
        );
        let source = program.generate_wgsl().unwrap();
        assert!(source.contains("fn dsl_rotate("));
        assert!(source.contains("z += vec3<f32>("));
    }

    #[test]
    fn generates_bounded_orbit_palette_and_two_specular_lobes() {
        let mut program = DslFractalConfig {
            orbit_period: Some(16),
            color_iterations: 64,
            ..DslFractalConfig::default()
        };
        program.material.surface_palette = vec![
            DslPaletteStop {
                position: 0.0,
                color: [0.02, 0.01, 0.0],
            },
            DslPaletteStop {
                position: 1.0,
                color: [1.0, 0.6, 0.1],
            },
        ];
        program.material.orbit_palette_weight = 1.0;
        program.material.palette_offset = 0.25;
        program.material.metallic_specular_strength = 4.0;

        let source = program.generate_wgsl().expect("palette shader");
        assert!(source.contains("scheduled_iteration = iteration % 16u"));
        assert!(source.contains("dsl_orbit(p, 64u, false).color_coordinate"));
        assert!(source.contains("fn dsl_surface_palette("));
        assert!(source.contains("metallic_specular"));

        program.material.surface_palette[1].position = 0.0;
        assert!(program.validate().is_err());
    }
}
