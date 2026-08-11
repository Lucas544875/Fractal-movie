const CAMERA: &str = include_str!("../camera.wgsl");
const QUALITY: &str = include_str!("../quality.wgsl");
const MANDELBULB: &str = include_str!("../fractal/mandelbulb.wgsl");
const MANDELBOX: &str = include_str!("../fractal/mandelbox.wgsl");
const MANDELBOX_QUAD: &str = include_str!("../fractal/mandelbox_quad.wgsl");
const QUAD_FLOAT: &str = include_str!("../precision/quad_float.wgsl");
const QUAD_FLOAT_VEC3: &str = include_str!("../precision/quad_float_vec3.wgsl");
const SHADING: &str = include_str!("../shading.wgsl");
const SHADING_QUAD: &str = include_str!("../shading_quad.wgsl");
const RAYMARCH: &str = include_str!("../raymarch.wgsl");
const RAYMARCH_QUAD: &str = include_str!("../raymarch_quad.wgsl");

use anyhow::Result;

use crate::{FractalConfig, FractalKind, Precision};

/// Builds one WGSL module from deliberately replaceable source fragments.
///
/// WGSL has no standard include directive. Keeping composition on the Rust
/// side gives later DSL-generated `map` implementations a narrow insertion
/// point while the camera and renderer remain fixed.
pub(crate) fn fractal_source(fractal: &FractalConfig, precision: Precision) -> Result<String> {
    let kind = fractal.kind();
    if precision == Precision::QuadFloat {
        debug_assert_eq!(kind, FractalKind::Mandelbox);
        return Ok([
            CAMERA,
            QUALITY,
            QUAD_FLOAT,
            QUAD_FLOAT_VEC3,
            MANDELBOX_QUAD,
            SHADING_QUAD,
            RAYMARCH_QUAD,
        ]
        .join("\n"));
    }
    let generated;
    let fractal_source = match fractal {
        FractalConfig::Mandelbulb(_) => MANDELBULB,
        FractalConfig::Mandelbox(_) => MANDELBOX,
        FractalConfig::Dsl(config) => {
            generated = config.generate_wgsl()?;
            &generated
        }
    };
    Ok([CAMERA, QUALITY, fractal_source, SHADING, RAYMARCH].join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composed_shader_contains_contract_and_entry_points() {
        for fractal in [
            FractalConfig::Mandelbulb(crate::MandelbulbConfig::default()),
            FractalConfig::Mandelbox(crate::MandelboxConfig::default()),
            FractalConfig::Dsl(crate::DslFractalConfig::default()),
        ] {
            let source = fractal_source(&fractal, Precision::F32).unwrap();
            assert!(source.contains("fn map("));
            assert!(source.contains("fn shade_fractal("));
            assert!(source.contains("fn fractal_normal_epsilon("));
            assert!(source.contains("fn fractal_background("));
            assert!(source.contains("fn apply_fractal_atmosphere("));
            assert!(source.contains("fn apply_post_process("));
            assert!(source.contains("fn vs_main("));
            assert!(source.contains("fn fs_main("));
        }

        let source = fractal_source(
            &FractalConfig::Mandelbox(crate::MandelboxConfig::default()),
            Precision::QuadFloat,
        )
        .unwrap();
        assert!(source.contains("fn qf_multiply("));
        assert!(source.contains("fn map_qf("));
        assert!(source.contains("fn shade_surface_qf("));
    }
}
