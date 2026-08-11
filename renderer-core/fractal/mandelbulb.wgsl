const MAX_FRACTAL_ITERATIONS: u32 = 64u;

// Distance-estimator contract. Other fractals can replace this file while the
// ray marcher, camera, and shading code stay fixed.
fn map(p: vec3<f32>) -> f32 {
    let power = uniforms.fractal.x;
    let bailout = uniforms.fractal.y;
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

