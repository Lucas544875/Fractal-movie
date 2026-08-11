const CAMERA: &str = include_str!("../camera.wgsl");
const MANDELBULB: &str = include_str!("../fractal/mandelbulb.wgsl");
const SHADING: &str = include_str!("../shading.wgsl");
const RAYMARCH: &str = include_str!("../raymarch.wgsl");

/// Builds one WGSL module from deliberately replaceable source fragments.
///
/// WGSL has no standard include directive. Keeping composition on the Rust
/// side gives later DSL-generated `map` implementations a narrow insertion
/// point while the camera and renderer remain fixed.
pub(crate) fn mandelbulb_source() -> String {
    [CAMERA, MANDELBULB, SHADING, RAYMARCH].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composed_shader_contains_contract_and_entry_points() {
        let source = mandelbulb_source();
        assert!(source.contains("fn map("));
        assert!(source.contains("fn vs_main("));
        assert!(source.contains("fn fs_main("));
    }
}
