//! Camera-relative evaluation of DSL fractals.
//!
//! Absolute `f32` coordinates stop resolving a surface once the camera is
//! within about a ten-thousandth of the shape's size of it. Here the shader
//! instead works in coordinates relative to the camera. The camera's own
//! orbit is computed on the CPU in `f64`, and the shader iterates only the
//! difference between a sample's orbit and that reference, which `f32`
//! represents with full relative precision however small it is.
//!
//! Every fold compares the orbit against a threshold. The CPU supplies the
//! reference's distance to each threshold, rounded only after the
//! subtraction, so the comparison for a sample stays exact near the
//! boundary. Once a sample's orbit has separated from the reference by a
//! visible fraction of its own size, the difference no longer carries extra
//! precision and the shader continues with the ordinary absolute orbit.

use std::fmt::Write;

use anyhow::{Result, bail};

use crate::{
    DslFractalConfig, KifsSymmetry, OrbitTransform,
    dsl::{wgsl_float, wgsl_vec3},
    fractal::rotate,
};

/// `vec4` slots in the reference uniform block.
pub(crate) const REFERENCE_CAPACITY: usize = 2048;
/// A sample leaves the reference once its difference exceeds this share of
/// its own magnitude.
const REBASE_RATIO: f32 = 1.0 / 64.0;
/// The reference is followed until it is this many escape radii away, by
/// which point every sample still relative to it has escaped or left it.
const REFERENCE_ESCAPE_RADII: f64 = 4.0;

/// The camera's orbit, laid out as the shader reads it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReferenceOrbit {
    pub origin: [f64; 3],
    /// Iterations the slots cover.
    pub iterations: u32,
    /// Slots per iteration.
    pub stride: u32,
    pub slots: Vec<[f32; 4]>,
}

/// Number of slots a transform records per iteration.
fn slot_count(transform: &OrbitTransform) -> u32 {
    match transform {
        OrbitTransform::BoxFold { .. } | OrbitTransform::SphereFold { .. } => 2,
        OrbitTransform::AmazingSurfFold { .. }
        | OrbitTransform::MandelboxJuliaFold { .. }
        | OrbitTransform::MandelboxFold { .. } => 4,
        OrbitTransform::KifsFold { symmetry, .. } => match symmetry {
            KifsSymmetry::Menger | KifsSymmetry::Octahedral => 2,
            KifsSymmetry::Tetrahedral => 1,
        },
        OrbitTransform::ScaleAddPoint { .. }
        | OrbitTransform::ScaleAddConstant { .. }
        | OrbitTransform::Rotate { .. }
        | OrbitTransform::Translate { .. } => 0,
    }
}

fn scheduled(transform: &OrbitTransform, iteration: u32) -> bool {
    match transform {
        OrbitTransform::AmazingSurfFold {
            start_iteration,
            stop_iteration,
            ..
        }
        | OrbitTransform::MandelboxJuliaFold {
            start_iteration,
            stop_iteration,
            ..
        }
        | OrbitTransform::MandelboxFold {
            start_iteration,
            stop_iteration,
            ..
        }
        | OrbitTransform::KifsFold {
            start_iteration,
            stop_iteration,
            ..
        } => (*start_iteration..*stop_iteration).contains(&iteration),
        _ => true,
    }
}

fn rotate_xyz(mut value: [f64; 3], degrees: [f32; 3]) -> [f64; 3] {
    for (axis, degrees) in [
        ([1.0, 0.0, 0.0], degrees[0]),
        ([0.0, 1.0, 0.0], degrees[1]),
        ([0.0, 0.0, 1.0], degrees[2]),
    ] {
        if degrees != 0.0 {
            value = rotate(value, axis, f64::from(degrees).to_radians());
        }
    }
    value
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn narrow(value: [f64; 3], w: f64) -> [f32; 4] {
    [value[0] as f32, value[1] as f32, value[2] as f32, w as f32]
}

/// Box fold of the reference. Records its distance over the upper and under
/// the lower limit of each folded axis.
fn fold_box(z: &mut [f64; 3], limits: [f64; 3], axes: usize, slots: &mut [[f32; 4]]) {
    let mut over = [0.0; 3];
    let mut under = [0.0; 3];
    for axis in 0..axes {
        over[axis] = z[axis] - limits[axis];
        under[axis] = z[axis] + limits[axis];
        z[axis] = z[axis].clamp(-limits[axis], limits[axis]) * 2.0 - z[axis];
    }
    slots[0] = narrow(over, 0.0);
    slots[1] = narrow(under, 0.0);
}

/// Radial fold `z *= numerator / clamp(|z|^2, low, high)` of the reference.
/// Returns the factor applied.
fn fold_radial(
    z: &mut [f64; 3],
    numerator: f64,
    low: f64,
    high: f64,
    slots: &mut [[f32; 4]],
) -> f64 {
    let radius_squared = dot(*z, *z);
    slots[0] = narrow(*z, radius_squared);
    slots[1] = [
        (radius_squared - low) as f32,
        (radius_squared - high) as f32,
        0.0,
        0.0,
    ];
    let factor = if radius_squared < low {
        numerator / low
    } else if radius_squared < high {
        numerator / radius_squared
    } else {
        numerator / high
    };
    *z = z.map(|component| component * factor);
    factor
}

impl DslFractalConfig {
    pub(crate) fn validate_relative(&self) -> Result<()> {
        if self.bailout.is_none() {
            bail!(
                "camera-relative precision needs a DSL fractal with a bailout; without one orbits never leave the camera's"
            );
        }
        if self.reference_stride() as usize * 2 > REFERENCE_CAPACITY {
            bail!("the DSL orbit is too long for camera-relative precision");
        }
        Ok(())
    }

    /// Slots one iteration occupies: the reference position, then each
    /// transform's own.
    pub(crate) fn reference_stride(&self) -> u32 {
        1 + self.orbit.iter().map(slot_count).sum::<u32>()
    }

    /// Follows the orbit of `origin` in `f64` and records what the shader
    /// needs to iterate differences from it.
    pub(crate) fn reference_orbit(&self, origin: [f64; 3]) -> ReferenceOrbit {
        let stride = self.reference_stride() as usize;
        let capacity = (REFERENCE_CAPACITY / stride - 1) as u32;
        let limit = self.iterations.min(capacity);
        let escape = self.bailout.map_or(f64::INFINITY, |bailout| {
            f64::from(bailout) * REFERENCE_ESCAPE_RADII
        });
        let mut slots = vec![[0.0_f32; 4]; REFERENCE_CAPACITY];
        let mut z = origin;
        let mut iterations = 0;

        for iteration in 0..=limit {
            let base = iteration as usize * stride;
            slots[base] = narrow(z, 0.0);
            // `f32` slots cannot hold an orbit that has left by far, and no
            // sample is still relative to it by then.
            if iteration == limit || dot(z, z) > escape * escape {
                break;
            }
            iterations = iteration + 1;
            let scheduled_iteration = self
                .orbit_period
                .map_or(iteration, |period| iteration % period);
            let mut offset = base + 1;
            for transform in &self.orbit {
                let count = slot_count(transform) as usize;
                let record = &mut slots[offset..offset + count];
                offset += count;
                if !scheduled(transform, scheduled_iteration) {
                    continue;
                }
                match transform {
                    OrbitTransform::BoxFold { limit } => {
                        fold_box(&mut z, [f64::from(*limit); 3], 3, record);
                    }
                    OrbitTransform::SphereFold {
                        min_radius_squared,
                        fixed_radius_squared,
                    } => {
                        let fixed = f64::from(*fixed_radius_squared);
                        fold_radial(&mut z, fixed, f64::from(*min_radius_squared), fixed, record);
                    }
                    OrbitTransform::AmazingSurfFold {
                        limits,
                        minimum_radius_squared,
                        scale,
                        rotation_degrees,
                        ..
                    } => {
                        fold_box(
                            &mut z,
                            [f64::from(limits[0]), f64::from(limits[1]), 0.0],
                            2,
                            &mut record[..2],
                        );
                        fold_radial(
                            &mut z,
                            f64::from(*scale),
                            f64::from(minimum_radius_squared.max(1.0e-12)),
                            1.0,
                            &mut record[2..],
                        );
                        z = rotate_xyz(z, *rotation_degrees);
                    }
                    OrbitTransform::MandelboxJuliaFold {
                        fold_limit,
                        min_radius_squared,
                        fixed_radius_squared,
                        scale,
                        constant,
                        rotation_degrees,
                        ..
                    } => {
                        fold_box(&mut z, [f64::from(*fold_limit); 3], 3, &mut record[..2]);
                        let fixed = f64::from(*fixed_radius_squared);
                        fold_radial(
                            &mut z,
                            fixed,
                            f64::from(*min_radius_squared),
                            fixed,
                            &mut record[2..],
                        );
                        z = rotate_xyz(z, *rotation_degrees);
                        z = [0, 1, 2]
                            .map(|axis| f64::from(*scale) * z[axis] + f64::from(constant[axis]));
                    }
                    OrbitTransform::MandelboxFold {
                        fold_limit,
                        min_radius_squared,
                        fixed_radius_squared,
                        scale,
                        rotation_degrees,
                        ..
                    } => {
                        fold_box(&mut z, [f64::from(*fold_limit); 3], 3, &mut record[..2]);
                        let fixed = f64::from(*fixed_radius_squared);
                        fold_radial(
                            &mut z,
                            fixed,
                            f64::from(*min_radius_squared),
                            fixed,
                            &mut record[2..],
                        );
                        z = rotate_xyz(z, *rotation_degrees);
                        z = [0, 1, 2].map(|axis| f64::from(*scale) * z[axis] + origin[axis]);
                    }
                    OrbitTransform::KifsFold {
                        symmetry,
                        scale,
                        offset: kifs_offset,
                        rotation_degrees,
                        ..
                    } => {
                        let shift =
                            kifs_offset.map(|component| f64::from(component * (scale - 1.0)));
                        let scale = f64::from(*scale);
                        match symmetry {
                            KifsSymmetry::Menger | KifsSymmetry::Octahedral => {
                                let entered = z;
                                z = z.map(f64::abs);
                                let first = z[0] - z[1];
                                if first < 0.0 {
                                    z.swap(0, 1);
                                }
                                let second = z[0] - z[2];
                                if second < 0.0 {
                                    z.swap(0, 2);
                                }
                                let third = z[1] - z[2];
                                if third < 0.0 {
                                    z.swap(1, 2);
                                }
                                z = rotate_xyz(z, *rotation_degrees);
                                z = [0, 1, 2].map(|axis| scale * z[axis] - shift[axis]);
                                let fold = z[2] + 0.5 * shift[2];
                                if *symmetry == KifsSymmetry::Menger && fold < 0.0 {
                                    z[2] = -shift[2] - z[2];
                                }
                                record[0] = narrow(entered, first);
                                record[1] = [second as f32, third as f32, fold as f32, 0.0];
                            }
                            KifsSymmetry::Tetrahedral => {
                                let first = z[0] + z[1];
                                if first < 0.0 {
                                    z = [-z[1], -z[0], z[2]];
                                }
                                let second = z[0] + z[2];
                                if second < 0.0 {
                                    z = [-z[2], z[1], -z[0]];
                                }
                                let third = z[1] + z[2];
                                if third < 0.0 {
                                    z = [z[0], -z[2], -z[1]];
                                }
                                z = rotate_xyz(z, *rotation_degrees);
                                z = [0, 1, 2].map(|axis| scale * z[axis] - shift[axis]);
                                record[0] = [first as f32, second as f32, third as f32, 0.0];
                            }
                        }
                    }
                    OrbitTransform::ScaleAddPoint { scale } => {
                        z = [0, 1, 2].map(|axis| f64::from(*scale) * z[axis] + origin[axis]);
                    }
                    OrbitTransform::ScaleAddConstant { scale, constant } => {
                        z = [0, 1, 2]
                            .map(|axis| f64::from(*scale) * z[axis] + f64::from(constant[axis]));
                    }
                    OrbitTransform::Rotate { axis, degrees } => {
                        z = rotate(z, axis.map(f64::from), f64::from(*degrees).to_radians());
                    }
                    OrbitTransform::Translate { offset } => {
                        z = [0, 1, 2].map(|axis| z[axis] + f64::from(offset[axis]));
                    }
                }
            }
        }

        ReferenceOrbit {
            origin,
            iterations,
            stride: stride as u32,
            slots,
        }
    }

    /// Appends the shader functions that iterate a sample relative to the
    /// reference orbit and hand over to the absolute orbit.
    pub(crate) fn generate_relative_orbit(&self, source: &mut String) -> Result<()> {
        let bailout = self.bailout.expect("validated bailout");
        writeln!(
            source,
            "struct DslReference {{\n    // x: iterations the slots cover, y: slots per iteration\n    extent: vec4<u32>,\n    // xyz: camera position, which the shader's coordinates are relative to\n    origin: vec4<f32>,\n    slots: array<vec4<f32>, {REFERENCE_CAPACITY}>,\n}}\n\n@group(0) @binding(1)\nvar<uniform> dsl_reference: DslReference;\n"
        )?;
        source.push_str(RELATIVE_HELPERS);
        source.push_str(
            "fn dsl_orbit(p: vec3<f32>, iteration_limit: u32, geometry_bailout: bool) -> DslOrbitResult {\n\
             \x20   let stride = dsl_reference.extent.y;\n\
             \x20   let reference_iterations = min(dsl_reference.extent.x, iteration_limit);\n\
             \x20   var d = p;\n\
             \x20   var z = dsl_reference.slots[0].xyz + p;\n\
             \x20   var derivative = 1.0;\n\
             \x20   var auxiliary_color = 1.0;\n\
             \x20   var iteration = 0u;\n\
             \x20   for (var pass_index = 0u; pass_index < MAX_DSL_ORBIT_ITERATIONS; pass_index += 1u) {\n\
             \x20       if iteration >= reference_iterations { break; }\n\
             \x20       let slot_base = iteration * stride;\n",
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

        let mut offset = 1_u32;
        for transform in &self.orbit {
            let slot = |index: u32| format!("dsl_reference.slots[slot_base + {}u]", offset + index);
            let rotations = |degrees: [f32; 3]| -> String {
                let mut text = String::new();
                for (axis, degrees) in [
                    ([1.0, 0.0, 0.0], degrees[0]),
                    ([0.0, 1.0, 0.0], degrees[1]),
                    ([0.0, 0.0, 1.0], degrees[2]),
                ] {
                    if degrees != 0.0 {
                        let _ = writeln!(
                            text,
                            "            d = dsl_rotate(d, {}, {});",
                            wgsl_vec3(axis),
                            wgsl_float(degrees.to_radians()),
                        );
                    }
                }
                text
            };
            // Box fold of the difference on the first `axes` axes.
            let box_fold = |first: u32, axes: usize| -> String {
                let mut text = String::new();
                for (name, weight) in [("x", "0.03"), ("y", "0.05"), ("z", "0.07")]
                    .iter()
                    .take(axes)
                {
                    let (over, under) = (
                        format!("{}.{name}", slot(first)),
                        format!("{}.{name}", slot(first + 1)),
                    );
                    let _ = writeln!(
                        text,
                        "            auxiliary_color += select(0.0, {weight}, {over} + d.{name} > 0.0 || {under} + d.{name} < 0.0);\n            d.{name} = dl_box(d.{name}, {over}, {under});"
                    );
                }
                text
            };
            // Radial fold of the difference; leaves `radial` in scope.
            let radial_fold = |first: u32, numerator: f32, low: f32, high: f32| -> String {
                format!(
                    "            let radial_{offset} = dl_radial(d, {}, {}, {}, {}, {});\n            d = radial_{offset}.d;\n",
                    slot(first),
                    slot(first + 1),
                    wgsl_float(numerator),
                    wgsl_float(low),
                    wgsl_float(high),
                )
            };
            let schedule = |start: u32, stop: u32| {
                format!(
                    "        if scheduled_iteration >= {start}u && scheduled_iteration < {stop}u {{\n"
                )
            };

            match transform {
                OrbitTransform::BoxFold { .. } => {
                    source.push_str("        {\n");
                    source.push_str(&box_fold(0, 3));
                    source.push_str("        }\n");
                }
                OrbitTransform::SphereFold {
                    min_radius_squared,
                    fixed_radius_squared,
                } => {
                    source.push_str("        {\n");
                    source.push_str(&radial_fold(
                        0,
                        *fixed_radius_squared,
                        *min_radius_squared,
                        *fixed_radius_squared,
                    ));
                    writeln!(
                        source,
                        "            derivative *= radial_{offset}.factor;\n            auxiliary_color += select(0.0, 0.2, radial_{offset}.folded);\n        }}"
                    )?;
                }
                OrbitTransform::AmazingSurfFold {
                    start_iteration,
                    stop_iteration,
                    minimum_radius_squared,
                    scale,
                    rotation_degrees,
                    ..
                } => {
                    source.push_str(&schedule(*start_iteration, *stop_iteration));
                    source.push_str("            color_bailout_allowed = true;\n");
                    // This fold does not colour the orbit.
                    source.push_str("            let color_before = auxiliary_color;\n");
                    source.push_str(&box_fold(0, 2));
                    source.push_str("            auxiliary_color = color_before;\n");
                    source.push_str(&radial_fold(
                        2,
                        *scale,
                        minimum_radius_squared.max(1.0e-12),
                        1.0,
                    ));
                    writeln!(
                        source,
                        "            derivative = derivative * abs(radial_{offset}.factor) + 1.0;"
                    )?;
                    source.push_str(&rotations(*rotation_degrees));
                    source.push_str("        }\n");
                }
                OrbitTransform::MandelboxJuliaFold {
                    start_iteration,
                    stop_iteration,
                    min_radius_squared,
                    fixed_radius_squared,
                    scale,
                    rotation_degrees,
                    ..
                }
                | OrbitTransform::MandelboxFold {
                    start_iteration,
                    stop_iteration,
                    min_radius_squared,
                    fixed_radius_squared,
                    scale,
                    rotation_degrees,
                    ..
                } => {
                    let adds_point = matches!(transform, OrbitTransform::MandelboxFold { .. });
                    source.push_str(&schedule(*start_iteration, *stop_iteration));
                    if adds_point {
                        source.push_str("            color_bailout_allowed = true;\n");
                    }
                    source.push_str(&box_fold(0, 3));
                    source.push_str(&radial_fold(
                        2,
                        *fixed_radius_squared,
                        *min_radius_squared,
                        *fixed_radius_squared,
                    ));
                    writeln!(
                        source,
                        "            derivative *= radial_{offset}.factor;\n            auxiliary_color += select(0.0, 0.2, radial_{offset}.folded);"
                    )?;
                    source.push_str(&rotations(*rotation_degrees));
                    // The constant of the Julia form is shared with the
                    // reference and cancels; the sample point does not.
                    writeln!(
                        source,
                        "            d = {} * d{};\n            derivative = derivative * abs({}) + 1.0;\n        }}",
                        wgsl_float(*scale),
                        if adds_point { " + p" } else { "" },
                        wgsl_float(*scale),
                    )?;
                }
                OrbitTransform::KifsFold {
                    start_iteration,
                    stop_iteration,
                    symmetry,
                    scale,
                    offset: kifs_offset,
                    rotation_degrees,
                } => {
                    source.push_str(&schedule(*start_iteration, *stop_iteration));
                    source.push_str("            color_bailout_allowed = true;\n");
                    match symmetry {
                        KifsSymmetry::Menger | KifsSymmetry::Octahedral => {
                            writeln!(
                                source,
                                "            d = dl_abs(d, {first}.xyz);\n\
                                 \x20           let sort_xy_{offset} = dl_sort(d.xy, {first}.w);\n\
                                 \x20           d = vec3<f32>(sort_xy_{offset}.d, d.z);\n\
                                 \x20           auxiliary_color += select(0.0, 0.03, sort_xy_{offset}.swapped);\n\
                                 \x20           let sort_xz_{offset} = dl_sort(d.xz, {second}.x);\n\
                                 \x20           d = vec3<f32>(sort_xz_{offset}.d.x, d.y, sort_xz_{offset}.d.y);\n\
                                 \x20           auxiliary_color += select(0.0, 0.05, sort_xz_{offset}.swapped);\n\
                                 \x20           let sort_yz_{offset} = dl_sort(d.yz, {second}.y);\n\
                                 \x20           d = vec3<f32>(d.x, sort_yz_{offset}.d);\n\
                                 \x20           auxiliary_color += select(0.0, 0.07, sort_yz_{offset}.swapped);",
                                first = slot(0),
                                second = slot(1),
                            )?;
                        }
                        KifsSymmetry::Tetrahedral => {
                            writeln!(
                                source,
                                "            let mirror_xy_{offset} = dl_mirror(d.xy, {first}.x);\n\
                                 \x20           d = vec3<f32>(mirror_xy_{offset}.d, d.z);\n\
                                 \x20           auxiliary_color += select(0.0, 0.03, mirror_xy_{offset}.swapped);\n\
                                 \x20           let mirror_xz_{offset} = dl_mirror(d.xz, {first}.y);\n\
                                 \x20           d = vec3<f32>(mirror_xz_{offset}.d.x, d.y, mirror_xz_{offset}.d.y);\n\
                                 \x20           auxiliary_color += select(0.0, 0.05, mirror_xz_{offset}.swapped);\n\
                                 \x20           let mirror_yz_{offset} = dl_mirror(d.yz, {first}.z);\n\
                                 \x20           d = vec3<f32>(d.x, mirror_yz_{offset}.d);\n\
                                 \x20           auxiliary_color += select(0.0, 0.07, mirror_yz_{offset}.swapped);",
                                first = slot(0),
                            )?;
                        }
                    }
                    source.push_str(&rotations(*rotation_degrees));
                    writeln!(source, "            d *= {};", wgsl_float(*scale))?;
                    if *symmetry == KifsSymmetry::Menger {
                        writeln!(
                            source,
                            "            let fold_{offset} = {}.z;\n\
                             \x20           let probe_folds_{offset} = fold_{offset} + d.z < 0.0;\n\
                             \x20           if probe_folds_{offset} && fold_{offset} < 0.0 {{\n\
                             \x20               d.z = -d.z;\n\
                             \x20           }} else if probe_folds_{offset} {{\n\
                             \x20               d.z = -2.0 * fold_{offset} - d.z;\n\
                             \x20           }} else if fold_{offset} < 0.0 {{\n\
                             \x20               d.z = 2.0 * fold_{offset} + d.z;\n\
                             \x20           }}\n\
                             \x20           auxiliary_color += select(0.0, 0.2, probe_folds_{offset});",
                            slot(1),
                        )?;
                    }
                    let _ = kifs_offset;
                    writeln!(
                        source,
                        "            derivative *= {};\n        }}",
                        wgsl_float(*scale)
                    )?;
                }
                OrbitTransform::ScaleAddPoint { scale } => {
                    writeln!(
                        source,
                        "        d = {} * d + p;\n        derivative = derivative * abs({}) + 1.0;",
                        wgsl_float(*scale),
                        wgsl_float(*scale),
                    )?;
                }
                OrbitTransform::ScaleAddConstant { scale, .. } => {
                    writeln!(
                        source,
                        "        d *= {};\n        derivative = derivative * abs({}) + 1.0;",
                        wgsl_float(*scale),
                        wgsl_float(*scale),
                    )?;
                }
                OrbitTransform::Rotate { axis, degrees } => {
                    writeln!(
                        source,
                        "        d = dsl_rotate(d, {}, {});",
                        wgsl_vec3(*axis),
                        wgsl_float(degrees.to_radians()),
                    )?;
                }
                OrbitTransform::Translate { .. } => {}
            }
            offset += slot_count(transform);
        }

        writeln!(
            source,
            "        iteration += 1u;\n\
             \x20       z = dsl_reference.slots[iteration * stride].xyz + d;\n\
             \x20       let radius_squared = dot(z, z);\n\
             \x20       if radius_squared > {} && (geometry_bailout || color_bailout_allowed) {{\n\
             \x20           return DslOrbitResult(\n\
             \x20               length(z) / max(abs(derivative), 1.0e-12),\n\
             \x20               fract(auxiliary_color * 0.1),\n\
             \x20           );\n\
             \x20       }}\n\
             \x20       // Past this the difference carries no precision the absolute\n\
             \x20       // orbit lacks.\n\
             \x20       if dot(d, d) > {} * radius_squared {{ break; }}\n\
             \x20   }}\n\
             \x20   return dsl_orbit_from(\n\
             \x20       dsl_reference.origin.xyz + p,\n\
             \x20       z,\n\
             \x20       derivative,\n\
             \x20       auxiliary_color,\n\
             \x20       iteration,\n\
             \x20       iteration_limit,\n\
             \x20       geometry_bailout,\n\
             \x20   );\n\
             }}\n\n\
             fn dsl_world_position(p: vec3<f32>) -> vec3<f32> {{\n\
             \x20   return dsl_reference.origin.xyz + p;\n\
             }}\n",
            wgsl_float(bailout * bailout),
            wgsl_float(REBASE_RATIO * REBASE_RATIO),
        )?;
        Ok(())
    }
}

/// Folds applied to a difference `d` from a reference value. Each takes the
/// reference's distance to the fold's threshold and returns the difference
/// after sample and reference have both been folded, each on its own side.
const RELATIVE_HELPERS: &str = "\
// One axis of a box fold. `over` and `under` are the reference minus the
// upper and the lower limit.
fn dl_box(d: f32, over: f32, under: f32) -> f32 {
    var excess = 0.0;
    let probe_over = over + d > 0.0;
    if probe_over && over > 0.0 {
        excess = d;
    } else if probe_over {
        excess = over + d;
    } else if over > 0.0 {
        excess = -over;
    }
    let probe_under = under + d < 0.0;
    if probe_under && under < 0.0 {
        excess += d;
    } else if probe_under {
        excess += under + d;
    } else if under < 0.0 {
        excess -= under;
    }
    return d - 2.0 * excess;
}

struct DlRadial {
    d: vec3<f32>,
    factor: f32,
    folded: bool,
}

// `z *= numerator / clamp(|z|^2, low, high)`. `anchor` is the reference
// value and its squared length; `margin` is that squared length minus `low`
// and minus `high`.
fn dl_radial(
    d: vec3<f32>,
    anchor: vec4<f32>,
    margin: vec4<f32>,
    numerator: f32,
    low: f32,
    high: f32,
) -> DlRadial {
    let r = anchor.xyz;
    let r2 = max(anchor.w, 1.0e-30);
    let q = 2.0 * dot(r, d) + dot(d, d);
    let z2 = max(r2 + q, 1.0e-30);
    let probe_low = margin.x + q;
    let probe_high = margin.y + q;
    // 0: below `low`, 1: between, 2: at or above `high`.
    var probe_zone = 1;
    if probe_low < 0.0 { probe_zone = 0; } else if probe_high >= 0.0 { probe_zone = 2; }
    var reference_zone = 1;
    if margin.x < 0.0 { reference_zone = 0; } else if margin.y >= 0.0 { reference_zone = 2; }

    var factor = numerator / z2;
    if probe_zone == 0 { factor = numerator / low; } else if probe_zone == 2 { factor = numerator / high; }

    // Sample factor minus reference factor, formed without cancellation.
    var difference = 0.0;
    if probe_zone == 1 && reference_zone == 1 {
        difference = -numerator * q / (z2 * r2);
    } else if probe_zone == 1 && reference_zone == 0 {
        difference = -numerator * probe_low / (z2 * low);
    } else if probe_zone == 0 && reference_zone == 1 {
        difference = numerator * margin.x / (r2 * low);
    } else if probe_zone == 1 && reference_zone == 2 {
        difference = -numerator * probe_high / (z2 * high);
    } else if probe_zone == 2 && reference_zone == 1 {
        difference = numerator * margin.y / (r2 * high);
    } else if probe_zone != reference_zone {
        difference = numerator / low - numerator / high;
        if probe_zone == 2 { difference = -difference; }
    }
    return DlRadial(factor * d + difference * r, factor, probe_zone != 2);
}

fn dl_abs(d: vec3<f32>, r: vec3<f32>) -> vec3<f32> {
    let probe_sign = select(vec3<f32>(1.0), vec3<f32>(-1.0), r + d < vec3<f32>(0.0));
    let reference_sign = select(vec3<f32>(1.0), vec3<f32>(-1.0), r < vec3<f32>(0.0));
    return probe_sign * select(d, 2.0 * r + d, probe_sign != reference_sign);
}

struct DlPair {
    d: vec2<f32>,
    swapped: bool,
}

// Swap that orders two components descending. `gap` is the reference's first
// component minus its second.
fn dl_sort(d: vec2<f32>, gap: f32) -> DlPair {
    let probe_swaps = gap + (d.x - d.y) < 0.0;
    if probe_swaps && gap < 0.0 {
        return DlPair(d.yx, true);
    } else if probe_swaps {
        return DlPair(vec2<f32>(d.y - gap, d.x + gap), true);
    } else if gap < 0.0 {
        return DlPair(vec2<f32>(d.x + gap, d.y - gap), false);
    }
    return DlPair(d, false);
}

// Reflection in the plane where two components sum to zero. `sum` is the
// reference's sum.
fn dl_mirror(d: vec2<f32>, sum: f32) -> DlPair {
    let probe_mirrors = sum + d.x + d.y < 0.0;
    if probe_mirrors && sum < 0.0 {
        return DlPair(-d.yx, true);
    } else if probe_mirrors {
        return DlPair(vec2<f32>(-sum - d.y, -sum - d.x), true);
    } else if sum < 0.0 {
        return DlPair(vec2<f32>(sum + d.x, sum + d.y), false);
    }
    return DlPair(d, false);
}

";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DistanceEstimator;

    // An `f32` transcription of the shader's relative orbit, so that the
    // arithmetic the GPU runs can be checked against the `f64` estimator.

    fn dl_box(d: f32, over: f32, under: f32) -> f32 {
        let mut excess = 0.0;
        let sample_over = over + d > 0.0;
        if sample_over && over > 0.0 {
            excess = d;
        } else if sample_over {
            excess = over + d;
        } else if over > 0.0 {
            excess = -over;
        }
        let sample_under = under + d < 0.0;
        if sample_under && under < 0.0 {
            excess += d;
        } else if sample_under {
            excess += under + d;
        } else if under < 0.0 {
            excess -= under;
        }
        d - 2.0 * excess
    }

    fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    fn dl_radial(
        d: [f32; 3],
        reference: [f32; 4],
        margin: [f32; 4],
        numerator: f32,
        low: f32,
        high: f32,
    ) -> ([f32; 3], f32) {
        let r = [reference[0], reference[1], reference[2]];
        let r2 = reference[3].max(1.0e-30);
        let q = 2.0 * dot3(r, d) + dot3(d, d);
        let z2 = (r2 + q).max(1.0e-30);
        let (sample_low, sample_high) = (margin[0] + q, margin[1] + q);
        let zone = |low: f32, high: f32| {
            if low < 0.0 {
                0
            } else if high >= 0.0 {
                2
            } else {
                1
            }
        };
        let (sample_zone, reference_zone) =
            (zone(sample_low, sample_high), zone(margin[0], margin[1]));
        let factor = match sample_zone {
            0 => numerator / low,
            2 => numerator / high,
            _ => numerator / z2,
        };
        let difference = match (sample_zone, reference_zone) {
            (1, 1) => -numerator * q / (z2 * r2),
            (1, 0) => -numerator * sample_low / (z2 * low),
            (0, 1) => numerator * margin[0] / (r2 * low),
            (1, 2) => -numerator * sample_high / (z2 * high),
            (2, 1) => numerator * margin[1] / (r2 * high),
            (0, 2) => numerator / low - numerator / high,
            (2, 0) => numerator / high - numerator / low,
            _ => 0.0,
        };
        (
            [0, 1, 2].map(|axis| factor * d[axis] + difference * r[axis]),
            factor,
        )
    }

    fn dl_pair(d: [f32; 2], threshold: f32, mirror: bool) -> [f32; 2] {
        let sample = if mirror {
            threshold + d[0] + d[1] < 0.0
        } else {
            threshold + (d[0] - d[1]) < 0.0
        };
        let reference = threshold < 0.0;
        match (sample, reference, mirror) {
            (true, true, false) => [d[1], d[0]],
            (true, false, false) => [d[1] - threshold, d[0] + threshold],
            (false, true, false) => [d[0] + threshold, d[1] - threshold],
            (true, true, true) => [-d[1], -d[0]],
            (true, false, true) => [-threshold - d[1], -threshold - d[0]],
            (false, true, true) => [threshold + d[0], threshold + d[1]],
            (false, false, _) => d,
        }
    }

    fn rotate_f32(mut value: [f32; 3], degrees: [f32; 3]) -> [f32; 3] {
        let wide = rotate_xyz(value.map(f64::from), degrees);
        value = wide.map(|component| component as f32);
        value
    }

    /// Distance estimate of `origin + p` as the shader computes it.
    fn relative_estimate(fractal: &DslFractalConfig, orbit: &ReferenceOrbit, p: [f32; 3]) -> f64 {
        let stride = orbit.stride as usize;
        let bailout = fractal.bailout.unwrap();
        let mut d = p;
        let mut derivative = 1.0_f32;
        let mut iteration = 0_usize;
        let header = |iteration: usize| {
            let slot = orbit.slots[iteration * stride];
            [slot[0], slot[1], slot[2]]
        };
        let mut z = [0, 1, 2].map(|axis| header(0)[axis] + p[axis]);

        while iteration < orbit.iterations.min(fractal.iterations) as usize {
            let base = iteration * stride;
            let scheduled_iteration = fractal
                .orbit_period
                .map_or(iteration as u32, |period| iteration as u32 % period);
            let mut offset = base + 1;
            for transform in &fractal.orbit {
                let count = slot_count(transform) as usize;
                let slots = &orbit.slots[offset..offset + count];
                offset += count;
                if !scheduled(transform, scheduled_iteration) {
                    continue;
                }
                let box_fold = |d: &mut [f32; 3], axes: usize| {
                    for axis in 0..axes {
                        d[axis] = dl_box(d[axis], slots[0][axis], slots[1][axis]);
                    }
                };
                match transform {
                    OrbitTransform::BoxFold { .. } => box_fold(&mut d, 3),
                    OrbitTransform::SphereFold {
                        min_radius_squared,
                        fixed_radius_squared,
                    } => {
                        let (folded, factor) = dl_radial(
                            d,
                            slots[0],
                            slots[1],
                            *fixed_radius_squared,
                            *min_radius_squared,
                            *fixed_radius_squared,
                        );
                        d = folded;
                        derivative *= factor;
                    }
                    OrbitTransform::AmazingSurfFold {
                        minimum_radius_squared,
                        scale,
                        rotation_degrees,
                        ..
                    } => {
                        box_fold(&mut d, 2);
                        let (folded, factor) = dl_radial(
                            d,
                            slots[2],
                            slots[3],
                            *scale,
                            minimum_radius_squared.max(1.0e-12),
                            1.0,
                        );
                        d = rotate_f32(folded, *rotation_degrees);
                        derivative = derivative * factor.abs() + 1.0;
                    }
                    OrbitTransform::MandelboxJuliaFold {
                        min_radius_squared,
                        fixed_radius_squared,
                        scale,
                        rotation_degrees,
                        ..
                    }
                    | OrbitTransform::MandelboxFold {
                        min_radius_squared,
                        fixed_radius_squared,
                        scale,
                        rotation_degrees,
                        ..
                    } => {
                        box_fold(&mut d, 3);
                        let (folded, factor) = dl_radial(
                            d,
                            slots[2],
                            slots[3],
                            *fixed_radius_squared,
                            *min_radius_squared,
                            *fixed_radius_squared,
                        );
                        derivative *= factor;
                        d = rotate_f32(folded, *rotation_degrees);
                        let adds_point = matches!(transform, OrbitTransform::MandelboxFold { .. });
                        d = [0, 1, 2]
                            .map(|axis| scale * d[axis] + if adds_point { p[axis] } else { 0.0 });
                        derivative = derivative * scale.abs() + 1.0;
                    }
                    OrbitTransform::KifsFold {
                        symmetry,
                        scale,
                        rotation_degrees,
                        ..
                    } => {
                        match symmetry {
                            KifsSymmetry::Menger | KifsSymmetry::Octahedral => {
                                let r = slots[0];
                                for axis in 0..3 {
                                    let sample_negative = r[axis] + d[axis] < 0.0;
                                    let sign = if sample_negative { -1.0 } else { 1.0 };
                                    d[axis] = if sample_negative == (r[axis] < 0.0) {
                                        sign * d[axis]
                                    } else {
                                        sign * (2.0 * r[axis] + d[axis])
                                    };
                                }
                                let xy = dl_pair([d[0], d[1]], slots[0][3], false);
                                d = [xy[0], xy[1], d[2]];
                                let xz = dl_pair([d[0], d[2]], slots[1][0], false);
                                d = [xz[0], d[1], xz[1]];
                                let yz = dl_pair([d[1], d[2]], slots[1][1], false);
                                d = [d[0], yz[0], yz[1]];
                            }
                            KifsSymmetry::Tetrahedral => {
                                let xy = dl_pair([d[0], d[1]], slots[0][0], true);
                                d = [xy[0], xy[1], d[2]];
                                let xz = dl_pair([d[0], d[2]], slots[0][1], true);
                                d = [xz[0], d[1], xz[1]];
                                let yz = dl_pair([d[1], d[2]], slots[0][2], true);
                                d = [d[0], yz[0], yz[1]];
                            }
                        }
                        d = rotate_f32(d, *rotation_degrees).map(|component| component * scale);
                        if *symmetry == KifsSymmetry::Menger {
                            let fold = slots[1][2];
                            let sample_folds = fold + d[2] < 0.0;
                            if sample_folds && fold < 0.0 {
                                d[2] = -d[2];
                            } else if sample_folds {
                                d[2] = -2.0 * fold - d[2];
                            } else if fold < 0.0 {
                                d[2] += 2.0 * fold;
                            }
                        }
                        derivative *= scale;
                    }
                    OrbitTransform::ScaleAddPoint { scale } => {
                        d = [0, 1, 2].map(|axis| scale * d[axis] + p[axis]);
                        derivative = derivative * scale.abs() + 1.0;
                    }
                    OrbitTransform::ScaleAddConstant { scale, .. } => {
                        d = d.map(|component| component * scale);
                        derivative = derivative * scale.abs() + 1.0;
                    }
                    OrbitTransform::Rotate { axis, degrees } => {
                        d = rotate(
                            d.map(f64::from),
                            axis.map(f64::from),
                            f64::from(*degrees).to_radians(),
                        )
                        .map(|component| component as f32);
                    }
                    OrbitTransform::Translate { .. } => {}
                }
            }
            iteration += 1;
            z = [0, 1, 2].map(|axis| header(iteration)[axis] + d[axis]);
            let radius_squared = dot3(z, z);
            if radius_squared > bailout * bailout {
                return f64::from(radius_squared.sqrt() / derivative.abs().max(1.0e-12));
            }
            if dot3(d, d) > REBASE_RATIO * REBASE_RATIO * radius_squared {
                break;
            }
        }
        let world = [0, 1, 2].map(|axis| f64::from(orbit.origin[axis] as f32 + p[axis]));
        fractal.continue_orbit(
            world,
            z.map(f64::from),
            f64::from(derivative),
            iteration as u32,
        )
    }

    fn programs() -> Vec<DslFractalConfig> {
        let scheduled = |orbit: Vec<OrbitTransform>, period: Option<u32>| DslFractalConfig {
            iterations: 60,
            orbit_period: period,
            bailout: Some(1000.0),
            orbit,
            ..DslFractalConfig::default()
        };
        let kifs =
            |symmetry, scale, offset, rotation_degrees, start, stop| OrbitTransform::KifsFold {
                start_iteration: start,
                stop_iteration: stop,
                symmetry,
                scale,
                offset,
                rotation_degrees,
            };
        let boxed = |start, stop| OrbitTransform::MandelboxFold {
            start_iteration: start,
            stop_iteration: stop,
            fold_limit: 1.0,
            min_radius_squared: 0.25,
            fixed_radius_squared: 1.0,
            scale: -1.8,
            rotation_degrees: [0.0, 4.0, 0.0],
        };
        vec![
            DslFractalConfig {
                iterations: 60,
                bailout: Some(1000.0),
                ..DslFractalConfig::default()
            },
            scheduled(
                vec![
                    OrbitTransform::Rotate {
                        axis: [0.3, 0.8, 1.0],
                        degrees: 7.5,
                    },
                    OrbitTransform::BoxFold { limit: 1.1 },
                    OrbitTransform::SphereFold {
                        min_radius_squared: 0.3,
                        fixed_radius_squared: 1.4,
                    },
                    OrbitTransform::Translate {
                        offset: [0.1, -0.05, 0.02],
                    },
                    OrbitTransform::ScaleAddConstant {
                        scale: 2.1,
                        constant: [0.4, -0.3, 0.8],
                    },
                ],
                None,
            ),
            scheduled(
                vec![kifs(
                    KifsSymmetry::Menger,
                    3.0,
                    [1.0, 1.0, 1.0],
                    [0.0; 3],
                    0,
                    60,
                )],
                None,
            ),
            scheduled(
                vec![
                    kifs(
                        KifsSymmetry::Octahedral,
                        2.0,
                        [1.0, 0.2, 0.0],
                        [5.0, 0.0, -3.0],
                        0,
                        1,
                    ),
                    kifs(
                        KifsSymmetry::Tetrahedral,
                        2.0,
                        [1.0, 1.0, 1.0],
                        [0.0, 8.0, 0.0],
                        1,
                        3,
                    ),
                ],
                Some(3),
            ),
            scheduled(
                vec![
                    kifs(KifsSymmetry::Menger, 2.6, [1.0, 0.9, 1.1], [0.0; 3], 0, 1),
                    boxed(1, 2),
                ],
                Some(2),
            ),
            scheduled(
                vec![
                    OrbitTransform::AmazingSurfFold {
                        start_iteration: 0,
                        stop_iteration: 3,
                        limits: [1.0, 0.9],
                        minimum_radius_squared: 0.1,
                        scale: 1.4,
                        rotation_degrees: [0.0, 90.0, 6.0],
                    },
                    OrbitTransform::MandelboxJuliaFold {
                        start_iteration: 3,
                        stop_iteration: crate::OPEN_STOP_ITERATION,
                        fold_limit: 1.0,
                        min_radius_squared: 0.2,
                        fixed_radius_squared: 1.0,
                        scale: 1.9,
                        constant: [0.5, -0.4, 1.1],
                        rotation_degrees: [3.0, 0.0, -2.0],
                    },
                ],
                None,
            ),
        ]
    }

    /// Walks towards the surface from `start` until the estimate is `depth`.
    fn point_at_clearance(fractal: &DslFractalConfig, start: [f64; 3], depth: f64) -> [f64; 3] {
        let length = dot(start, start).sqrt();
        let direction = start.map(|component| -component / length);
        let mut point = start;
        for _ in 0..4_000 {
            let distance = fractal.distance_estimate(point);
            if distance <= depth {
                break;
            }
            let step = ((distance - depth) * 0.5).max(depth * 0.1);
            point = [0, 1, 2].map(|axis| point[axis] + direction[axis] * step);
        }
        point
    }

    #[test]
    fn relative_orbit_matches_the_f64_estimator_far_below_f32_resolution() {
        let mut checked = 0;
        for (index, fractal) in programs().iter().enumerate() {
            fractal.validate_relative().unwrap();
            for start in [[5.0, 2.3, 1.1], [-1.7, 4.2, -3.9], [0.9, -0.4, 6.0]] {
                for depth in [1.0e-3, 1.0e-6, 1.0e-9] {
                    let origin = point_at_clearance(fractal, start, depth);
                    let clearance = fractal.distance_estimate(origin);
                    if !(clearance.is_finite() && clearance > 0.0 && clearance < depth * 4.0) {
                        continue;
                    }
                    let orbit = fractal.reference_orbit(origin);
                    // Samples from a tenth of the clearance to a few times it.
                    for (direction, reach) in [
                        ([1.0, 0.0, 0.0], 0.1),
                        ([0.0, -1.0, 0.0], 0.5),
                        ([0.577, 0.577, 0.577], 0.9),
                        ([-0.6, 0.0, 0.8], 2.5),
                        ([0.0, 0.0, 0.0], 0.0),
                    ] {
                        let p = direction.map(|component: f64| component * reach * clearance);
                        let absolute = [0, 1, 2].map(|axis| origin[axis] + p[axis]);
                        let expected = fractal.distance_estimate(absolute);
                        let actual =
                            relative_estimate(fractal, &orbit, p.map(|component| component as f32));
                        // Compared against the scale of the view: an error
                        // well under a pixel's footprint.
                        assert!(
                            (actual - expected).abs() < clearance * 2.0e-4,
                            "program {index} depth {depth:e} p {p:?}: {actual:e} vs {expected:e}"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 100, "only {checked} samples were checked");
    }

    #[test]
    fn reference_orbit_fits_the_uniform_block_and_stops_after_escape() {
        for fractal in programs() {
            let orbit = fractal.reference_orbit([30.0, 20.0, 10.0]);
            assert_eq!(orbit.slots.len(), REFERENCE_CAPACITY);
            assert!(((orbit.iterations + 1) * orbit.stride) as usize <= REFERENCE_CAPACITY);
            // A point far outside the shape escapes long before the limit.
            assert!(
                orbit.iterations < fractal.iterations,
                "{} iterations",
                orbit.iterations
            );
            assert!(orbit.slots.iter().flatten().all(|value| value.is_finite()));
        }
    }

    #[test]
    fn generated_relative_shader_reads_the_reference_and_resumes_the_orbit() {
        for fractal in programs() {
            let source = fractal.generate_relative_wgsl().unwrap();
            assert!(source.contains("var<uniform> dsl_reference: DslReference;"));
            assert!(source.contains("fn dsl_orbit_from("));
            assert!(source.contains("return dsl_orbit_from("));
            assert!(source.contains("fn dsl_world_position("));
        }
        let unbounded = DslFractalConfig::default();
        assert!(unbounded.generate_relative_wgsl().is_err());
    }
}
