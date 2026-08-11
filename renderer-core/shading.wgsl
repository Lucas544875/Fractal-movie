fn estimate_normal(p: vec3<f32>, hit_epsilon: f32) -> vec3<f32> {
    // A wider normal footprint than the hit epsilon suppresses numerical
    // sparkle on high-frequency DE surfaces while retaining bulb detail.
    let epsilon = fractal_normal_epsilon(hit_epsilon, uniforms.render_params.x);
    let k1 = vec3<f32>(1.0, -1.0, -1.0);
    let k2 = vec3<f32>(-1.0, -1.0, 1.0);
    let k3 = vec3<f32>(-1.0, 1.0, -1.0);
    let k4 = vec3<f32>(1.0, 1.0, 1.0);
    let gradient =
        k1 * map(p + epsilon * k1) +
        k2 * map(p + epsilon * k2) +
        k3 * map(p + epsilon * k3) +
        k4 * map(p + epsilon * k4);
    return safe_normalize(gradient, vec3<f32>(0.0, 1.0, 0.0));
}

fn shade_surface(
    p: vec3<f32>,
    ray_direction: vec3<f32>,
    step_ratio: f32,
    hit_epsilon: f32,
) -> vec3<f32> {
    let normal = estimate_normal(p, hit_epsilon);
    let light = safe_normalize(uniforms.light_direction.xyz, vec3<f32>(-0.4, 0.8, 0.5));
    return shade_fractal(p, ray_direction, normal, step_ratio, light, 1.0, 1.0);
}
