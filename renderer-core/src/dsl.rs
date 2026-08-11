use std::fmt::Write;

use anyhow::{Result, bail};

use crate::config::MAX_FRACTAL_ITERATIONS;

pub const MAX_DSL_TRANSFORMS: usize = 16;

/// A deliberately bounded fractal program. Scene files can construct this AST
/// but cannot inject identifiers, statements, or arbitrary WGSL source.
#[derive(Clone, Debug, PartialEq)]
pub struct DslFractalConfig {
    pub iterations: u32,
    pub bailout: Option<f32>,
    pub normal_epsilon: f32,
    pub orbit: Vec<OrbitTransform>,
    pub material: DslMaterial,
}

#[derive(Clone, Debug, PartialEq)]
pub enum OrbitTransform {
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
    pub rim_strength: f32,
    pub fog_density: f32,
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
            camera_palette_weight: 0.0,
            normal_palette_weight: 0.55,
            ambient_strength: 0.18,
            diffuse_strength: 0.95,
            specular_strength: 0.35,
            shininess: 48.0,
            rim_strength: 0.18,
            fog_density: 0.0,
        }
    }
}

impl Default for DslFractalConfig {
    fn default() -> Self {
        Self {
            iterations: 16,
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
        if let Some(bailout) = self.bailout {
            finite_range("DSL bailout", bailout, 1.0e-6, 1.0e6)?;
        }
        finite_range("DSL normal_epsilon", self.normal_epsilon, 1.0e-12, 0.099)?;
        if self.orbit.is_empty() || self.orbit.len() > MAX_DSL_TRANSFORMS {
            bail!("DSL orbit must contain 1..={MAX_DSL_TRANSFORMS} transforms");
        }

        let mut scale_add_point_count = 0usize;
        for (index, transform) in self.orbit.iter().enumerate() {
            match transform {
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
                    scale_add_point_count += 1;
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
        if scale_add_point_count != 1 {
            bail!("DSL orbit must contain exactly one scale-add-point transform");
        }
        self.material.validate()
    }

    /// Generates the complete fractal shader fragment consumed by the common
    /// camera, shading, and ray-marching modules.
    pub fn generate_wgsl(&self) -> Result<String> {
        self.validate()?;
        let mut source = String::with_capacity(8_192);
        writeln!(source, "const MAX_FRACTAL_ITERATIONS: u32 = 96u;")?;

        if self
            .orbit
            .iter()
            .any(|transform| matches!(transform, OrbitTransform::Rotate { .. }))
        {
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
            "fn map(p: vec3<f32>) -> f32 {\n\
             \x20   var z = p;\n\
             \x20   var derivative = 1.0;\n\
             \x20   for (var iteration = 0u; iteration < MAX_FRACTAL_ITERATIONS; iteration += 1u) {\n\
             \x20       if iteration >= uniforms.limits.x { break; }\n",
        );

        for (index, transform) in self.orbit.iter().enumerate() {
            match transform {
                OrbitTransform::BoxFold { limit } => {
                    let limit = wgsl_float(*limit);
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
                "        if dot(z, z) > {} {{ break; }}",
                wgsl_float(bailout * bailout)
            )?;
        }
        source.push_str(
            "    }\n\
             \x20   return length(z) / max(abs(derivative), 1.0e-12);\n\
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
        writeln!(
            source,
            "fn shade_fractal(\n    p: vec3<f32>,\n    ray_direction: vec3<f32>,\n    normal: vec3<f32>,\n    step_ratio: f32,\n    light: vec3<f32>,\n    direct_visibility: f32,\n    ambient_visibility: f32,\n) -> vec3<f32> {{\n    let world_palette = 0.5 + 0.5 * sin(p.z * {});\n    let basis = camera_basis();\n    let camera_relative_position = (p - uniforms.camera_target.xyz)\n        / max(uniforms.camera_lens.y, 1.0e-12);\n    let camera_palette_coordinate = dot(\n        camera_relative_position,\n        0.7 * basis.right + basis.up,\n    );\n    let camera_palette = 0.5 + 0.5 * sin(camera_palette_coordinate * {});\n    let position_palette = mix(world_palette, camera_palette, {});\n    let normal_palette = 0.5 + 0.5 * normal.z;\n    let palette = mix(position_palette, normal_palette, {});\n    let base = mix({}, {}, palette);\n    let diffuse = max(dot(normal, light), 0.0);\n    let half_vector = safe_normalize(light - ray_direction, light);\n    let specular = pow(max(dot(normal, half_vector), 0.0), {});\n    let rim = pow(1.0 - max(dot(normal, -ray_direction), 0.0), 2.5);\n    let march_occlusion = mix(1.0, 0.76, clamp(step_ratio, 0.0, 1.0));\n    return max(\n        base * ({} * ambient_visibility + {} * diffuse * direct_visibility) * march_occlusion\n            + {} * specular * {} * direct_visibility\n            + base * rim * {} * ambient_visibility,\n        vec3<f32>(0.0),\n    );\n}}\n",
            wgsl_float(material.color_frequency),
            wgsl_float(material.color_frequency),
            wgsl_float(material.camera_palette_weight),
            wgsl_float(material.normal_palette_weight),
            wgsl_vec3(material.base_color),
            wgsl_vec3(material.accent_color),
            wgsl_float(material.shininess),
            wgsl_float(material.ambient_strength),
            wgsl_float(material.diffuse_strength),
            wgsl_vec3(material.specular_color),
            wgsl_float(material.specular_strength),
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
}
