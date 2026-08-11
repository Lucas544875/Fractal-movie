const MAX_FRACTAL_ITERATIONS: u32 = 64u;

// Distance-estimator contract. Other fractals can replace this file while the
// ray marcher, camera, and shading code stay fixed.
fn map(p: vec3<f32>) -> f32 {
    let power = uniforms.fractal_primary.x;
    let bailout = uniforms.fractal_primary.y;
    var z = p;
    var derivative = 1.0;
    var radius = length(z);

    for (var iteration = 0u; iteration < MAX_FRACTAL_ITERATIONS; iteration += 1u) {
        if iteration >= uniforms.limits.x {
            break;
        }
        radius = length(z);
        if radius > bailout {
            break;
        }

        let safe_radius = max(radius, 1.0e-7);
        let polar = acos(clamp(z.z / safe_radius, -1.0, 1.0));
        let azimuth = atan2(z.y, z.x);
        derivative = pow(safe_radius, power - 1.0) * power * derivative + 1.0;
        let powered_radius = pow(safe_radius, power);
        let powered_polar = polar * power;
        let powered_azimuth = azimuth * power;
        z = powered_radius * vec3<f32>(
            sin(powered_polar) * cos(powered_azimuth),
            sin(powered_polar) * sin(powered_azimuth),
            cos(powered_polar),
        ) + p;
    }

    let safe_radius = max(radius, 1.0e-7);
    return 0.5 * log(safe_radius) * safe_radius / max(derivative, 1.0e-7);
}

fn fractal_normal_epsilon(hit_epsilon: f32, base_epsilon: f32) -> f32 {
    return max(max(hit_epsilon * 0.5, base_epsilon * 10.0), 5.0e-4);
}

fn shade_fractal(
    p: vec3<f32>,
    ray_direction: vec3<f32>,
    normal: vec3<f32>,
    step_ratio: f32,
    light: vec3<f32>,
    direct_visibility: f32,
    ambient_visibility: f32,
) -> vec3<f32> {
    let diffuse = max(dot(normal, light), 0.0);
    let half_vector = safe_normalize(light - ray_direction, light);
    let specular = pow(max(dot(normal, half_vector), 0.0), 48.0);
    let rim = pow(1.0 - max(dot(normal, -ray_direction), 0.0), 2.5);

    let height_color = 0.5 + 0.5 * sin(vec3<f32>(0.2, 1.7, 3.5) + p.z * 2.3);
    let base = mix(vec3<f32>(0.12, 0.22, 0.42), vec3<f32>(0.95, 0.38, 0.12), height_color);
    let march_occlusion = mix(1.0, 0.72, clamp(step_ratio, 0.0, 1.0));
    return base * (
            0.16 * ambient_visibility + 0.84 * diffuse * direct_visibility
        ) * march_occlusion
        + vec3<f32>(1.0, 0.82, 0.58) * specular * 0.55 * direct_visibility
        + vec3<f32>(0.12, 0.22, 0.40) * rim * 0.32 * ambient_visibility;
}

fn fractal_background(ray_direction: vec3<f32>) -> vec3<f32> {
    let horizon = clamp(0.5 + 0.5 * ray_direction.y, 0.0, 1.0);
    return mix(vec3<f32>(0.012, 0.018, 0.035), vec3<f32>(0.10, 0.16, 0.25), horizon);
}

fn apply_fractal_atmosphere(surface: vec3<f32>, sky: vec3<f32>, travel: f32) -> vec3<f32> {
    let fog = 1.0 - exp(-0.012 * travel * travel);
    return mix(surface, sky, fog);
}
