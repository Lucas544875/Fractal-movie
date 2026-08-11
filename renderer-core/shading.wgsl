fn estimate_normal(p: vec3<f32>) -> vec3<f32> {
    // A wider normal footprint than the hit epsilon suppresses numerical
    // sparkle on high-frequency DE surfaces while retaining bulb detail.
    let epsilon = max(uniforms.fractal.z * 10.0, 5.0e-4);
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

fn background(ray_direction: vec3<f32>) -> vec3<f32> {
    let horizon = clamp(0.5 + 0.5 * ray_direction.y, 0.0, 1.0);
    return mix(vec3<f32>(0.012, 0.018, 0.035), vec3<f32>(0.10, 0.16, 0.25), horizon);
}

fn shade_surface(p: vec3<f32>, ray_direction: vec3<f32>, step_ratio: f32) -> vec3<f32> {
    let normal = estimate_normal(p);
    let light = safe_normalize(uniforms.light_direction.xyz, vec3<f32>(-0.4, 0.8, 0.5));
    let diffuse = max(dot(normal, light), 0.0);
    let half_vector = safe_normalize(light - ray_direction, light);
    let specular = pow(max(dot(normal, half_vector), 0.0), 48.0);
    let rim = pow(1.0 - max(dot(normal, -ray_direction), 0.0), 2.5);

    let height_color = 0.5 + 0.5 * sin(vec3<f32>(0.2, 1.7, 3.5) + p.z * 2.3);
    let base = mix(vec3<f32>(0.12, 0.22, 0.42), vec3<f32>(0.95, 0.38, 0.12), height_color);
    let march_occlusion = mix(1.0, 0.72, clamp(step_ratio, 0.0, 1.0));
    return base * (0.16 + 0.84 * diffuse) * march_occlusion
        + vec3<f32>(1.0, 0.82, 0.58) * specular * 0.55
        + vec3<f32>(0.12, 0.22, 0.40) * rim * 0.32;
}
