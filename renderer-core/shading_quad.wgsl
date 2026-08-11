fn estimate_normal_qf(p: QfVec3, hit_epsilon: f32) -> vec3<f32> {
    let epsilon = fractal_normal_epsilon(hit_epsilon, uniforms.render_params.x);
    let k1 = vec3<f32>(1.0, -1.0, -1.0);
    let k2 = vec3<f32>(-1.0, -1.0, 1.0);
    let k3 = vec3<f32>(-1.0, 1.0, -1.0);
    let k4 = vec3<f32>(1.0, 1.0, 1.0);
    let gradient =
        k1 * map_qf(qfv_add(p, qfv_multiply(qfv_from_f32(k1), qf_from_f32(epsilon)))) +
        k2 * map_qf(qfv_add(p, qfv_multiply(qfv_from_f32(k2), qf_from_f32(epsilon)))) +
        k3 * map_qf(qfv_add(p, qfv_multiply(qfv_from_f32(k3), qf_from_f32(epsilon)))) +
        k4 * map_qf(qfv_add(p, qfv_multiply(qfv_from_f32(k4), qf_from_f32(epsilon))));
    return safe_normalize(gradient, vec3<f32>(0.0, 1.0, 0.0));
}

fn shade_surface_qf(
    p: QfVec3,
    ray_direction: vec3<f32>,
    step_ratio: f32,
    hit_epsilon: f32,
) -> vec3<f32> {
    let normal = estimate_normal_qf(p, hit_epsilon);
    let light = safe_normalize(uniforms.light_direction.xyz, vec3<f32>(-0.4, 0.8, 0.5));
    return shade_fractal(
        qfv_mandelbox_local_to_f32(p),
        ray_direction,
        normal,
        step_ratio,
        light,
        1.0,
        1.0,
    );
}
