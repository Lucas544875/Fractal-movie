const MAX_FRACTAL_ITERATIONS: u32 = 96u;

fn qf_box_fold(value: Qf32, limit: f32) -> Qf32 {
    let upper = qf_from_f32(limit);
    let lower = qf_from_f32(-limit);
    if qf_less(upper, value) {
        return qf_subtract(qf_from_f32(2.0 * limit), value);
    }
    if qf_less(value, lower) {
        return qf_subtract(qf_from_f32(-2.0 * limit), value);
    }
    return value;
}

fn qf_mandelbox_boundary() -> Qf32 {
    return qf_from_f32(2.0 * uniforms.fractal_primary.w);
}

// Deep rays use a rebased coordinate: x is stored relative to the positive
// axial boundary, while y/z remain absolute (and are already near zero).
// This avoids repeatedly adding a 1e-N offset to an O(1) coordinate.
fn qfv_mandelbox_local(absolute: QfVec3) -> QfVec3 {
    return QfVec3(
        qf_subtract(absolute.x, qf_mandelbox_boundary()),
        absolute.y,
        absolute.z,
    );
}

fn qfv_mandelbox_local_to_f32(local: QfVec3) -> vec3<f32> {
    return vec3<f32>(
        qf_to_f32(qf_mandelbox_boundary()) + qf_to_f32(local.x),
        qf_to_f32(local.y),
        qf_to_f32(local.z),
    );
}

// Mandelbox DE with all cancellation-sensitive local coordinates retained as
// Qf32. `p.x` and the x component of `z` are boundary-relative.
fn map_qf(p: QfVec3) -> f32 {
    let scale = uniforms.fractal_primary.x;
    let min_radius_squared = qf_from_f32(uniforms.fractal_primary.y);
    let fixed_radius_squared = qf_from_f32(uniforms.fractal_primary.z);
    let fold_limit = uniforms.fractal_primary.w;
    var z = p;
    var log_derivative = 0.0;

    for (var iteration = 0u; iteration < MAX_FRACTAL_ITERATIONS; iteration += 1u) {
        if iteration >= uniforms.limits.x {
            break;
        }
        let centered_threshold = qf_from_f32(-fold_limit);
        var folded_x: Qf32;
        if qf_less(centered_threshold, z.x) {
            folded_x = qf_negate(z.x);
        } else {
            folded_x = qf_box_fold(qf_add(qf_mandelbox_boundary(), z.x), fold_limit);
        }
        z = QfVec3(
            folded_x,
            qf_box_fold(z.y, fold_limit),
            qf_box_fold(z.z, fold_limit),
        );
        let radius_squared = qfv_dot(z, z);
        if qf_less(radius_squared, min_radius_squared) {
            let factor = qf_divide(fixed_radius_squared, min_radius_squared);
            z = qfv_multiply(z, factor);
            log_derivative += log(qf_to_f32(factor));
        } else if qf_less(radius_squared, fixed_radius_squared) {
            let factor = qf_divide(fixed_radius_squared, radius_squared);
            z = qfv_multiply(z, factor);
            log_derivative += log(qf_to_f32(factor));
        }
        // The x component remains centered after scale-and-translate:
        // scale*z.x + (boundary + p.x) = boundary + (scale*z.x + p.x).
        z = qfv_add(qfv_multiply(z, qf_from_f32(scale)), p);
        let scaled_log_derivative = log_derivative + log(abs(scale));
        log_derivative = scaled_log_derivative + log(1.0 + exp(-scaled_log_derivative));
    }

    let absolute_z = qfv_mandelbox_local_to_f32(z);
    let radius_squared = max(dot(absolute_z, absolute_z), 0.0);
    if radius_squared == 0.0 {
        return 0.0;
    }
    return exp(0.5 * log(radius_squared) - log_derivative);
}

fn fractal_normal_epsilon(hit_epsilon: f32, base_epsilon: f32) -> f32 {
    return max(hit_epsilon * 0.5, base_epsilon);
}

fn incandescent_source(p: vec3<f32>, center: vec3<f32>) -> vec3<f32> {
    let strength = pow(max(1.0 - distance(center, p) / 2.0, 0.0), 2.0) * 1.5;
    return strength * vec3<f32>(1.0, 0.501, 0.2);
}

fn shade_fractal(
    p: vec3<f32>,
    ray_direction: vec3<f32>,
    normal: vec3<f32>,
    step_ratio: f32,
    light: vec3<f32>,
) -> vec3<f32> {
    let base = vec3<f32>(0.454, 0.301, 0.211);
    let diffuse = max(dot(light, normal), 0.0);
    var color = base * (0.7 + 1.1 * diffuse);
    color += incandescent_source(p, vec3<f32>( 2.0, 0.0, 0.0));
    color += incandescent_source(p, vec3<f32>(-2.0, 0.0, 0.0));
    color += incandescent_source(p, vec3<f32>(0.0,  2.0, 0.0));
    color += incandescent_source(p, vec3<f32>(0.0, -2.0, 0.0));
    color += incandescent_source(p, vec3<f32>(0.0, 0.0,  2.0));
    color += incandescent_source(p, vec3<f32>(0.0, 0.0, -2.0));
    color += smoothstep(0.0, 0.95, step_ratio) * vec3<f32>(1.0, 0.501, 0.2);
    return pow(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(2.2));
}

fn fractal_background(ray_direction: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(0.0);
}

fn apply_fractal_atmosphere(surface: vec3<f32>, sky: vec3<f32>, travel: f32) -> vec3<f32> {
    return surface;
}
