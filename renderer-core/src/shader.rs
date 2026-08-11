const CAMERA: &str = include_str!("../camera.wgsl");
const MANDELBULB: &str = include_str!("../fractal/mandelbulb.wgsl");
const MANDELBOX: &str = include_str!("../fractal/mandelbox.wgsl");
const SHADING: &str = include_str!("../shading.wgsl");
const RAYMARCH: &str = include_str!("../raymarch.wgsl");

use crate::FractalKind;

/// Builds one WGSL module from deliberately replaceable source fragments.
///
/// WGSL has no standard include directive. Keeping composition on the Rust
/// side gives later DSL-generated `map` implementations a narrow insertion
/// point while the camera and renderer remain fixed.
pub(crate) fn fractal_source(kind: FractalKind) -> String {
    let fractal = match kind {
        FractalKind::Mandelbulb => MANDELBULB,
        FractalKind::Mandelbox => MANDELBOX,
    };
    [CAMERA, fractal, SHADING, RAYMARCH].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composed_shader_contains_contract_and_entry_points() {
        for kind in [FractalKind::Mandelbulb, FractalKind::Mandelbox] {
            let source = fractal_source(kind);
            assert!(source.contains("fn map("));
            assert!(source.contains("fn shade_fractal("));
            assert!(source.contains("fn fractal_normal_epsilon("));
            assert!(source.contains("fn fractal_background("));
            assert!(source.contains("fn apply_fractal_atmosphere("));
            assert!(source.contains("fn vs_main("));
            assert!(source.contains("fn fs_main("));
        }
    }
}
