const MAX_FRACTAL_ITERATIONS: u32 = 64u;

// Mandelbox distance estimator ported from the portfolio WebGL page.
// fractal_primary: x=scale, y=min radius², z=fixed radius², w=fold limit.
fn map(p: vec3<f32>) -> f32 {
    let scale = uniforms.fractal_primary.x;
    let min_radius_squared = uniforms.fractal_primary.y;
    let fixed_radius_squared = uniforms.fractal_primary.z;
    let fold_limit = uniforms.fractal_primary.w;
    var z = p;
    var derivative = 1.0;

    for (var iteration = 0u; iteration < MAX_FRACTAL_ITERATIONS; iteration += 1u) {
        if iteration >= uniforms.limits.x {
            break;
        }

        z = clamp(z, vec3<f32>(-fold_limit), vec3<f32>(fold_limit)) * 2.0 - z;
        let radius_squared = dot(z, z);
        if radius_squared < min_radius_squared {
            let factor = fixed_radius_squared / min_radius_squared;
            z *= factor;
            derivative *= factor;
        } else if radius_squared < fixed_radius_squared {
            let factor = fixed_radius_squared / max(radius_squared, 1.0e-12);
            z *= factor;
            derivative *= factor;
        }
        z = scale * z + p;
        derivative = derivative * abs(scale) + 1.0;
    }

    return length(z) / max(abs(derivative), 1.0e-12);
}

fn fractal_normal_epsilon(hit_epsilon: f32, base_epsilon: f32) -> f32 {
    return max(hit_epsilon * 0.5, base_epsilon);
}

fn incandescent_source(p: vec3<f32>, center: vec3<f32>) -> vec3<f32> {
    let strength = pow(max(1.0 - distance(center, p) / 2.0, 0.0), 2.0) * 1.5;
    return strength * vec3<f32>(1.0, 0.501, 0.2);
}

// Brown material, six orange incandescent centers, and late-step glow match
// the effects enabled by mandelbox-full.frag.
fn shade_fractal(
    p: vec3<f32>,
    ray_direction: vec3<f32>,
    normal: vec3<f32>,
    step_ratio: f32,
    light: vec3<f32>,
    direct_visibility: f32,
    ambient_visibility: f32,
) -> vec3<f32> {
    let base = vec3<f32>(0.454, 0.301, 0.211);
    let diffuse = max(dot(light, normal), 0.0);
    var color = base * (
        0.7 * ambient_visibility + 1.1 * diffuse * direct_visibility
    );
    color += incandescent_source(p, vec3<f32>( 2.0, 0.0, 0.0));
    color += incandescent_source(p, vec3<f32>(-2.0, 0.0, 0.0));
    color += incandescent_source(p, vec3<f32>(0.0,  2.0, 0.0));
    color += incandescent_source(p, vec3<f32>(0.0, -2.0, 0.0));
    color += incandescent_source(p, vec3<f32>(0.0, 0.0,  2.0));
    color += incandescent_source(p, vec3<f32>(0.0, 0.0, -2.0));
    color += smoothstep(0.0, 0.95, step_ratio) * vec3<f32>(1.0, 0.501, 0.2);
    return max(color, vec3<f32>(0.0));
}

fn fractal_background(ray_direction: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(0.0);
}

fn apply_fractal_atmosphere(surface: vec3<f32>, sky: vec3<f32>, travel: f32) -> vec3<f32> {
    // Fog is disabled in the referenced portfolio effect configuration.
    return surface;
}
