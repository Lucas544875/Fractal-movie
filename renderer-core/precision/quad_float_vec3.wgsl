struct QfVec3 {
    x: Qf32,
    y: Qf32,
    z: Qf32,
}

fn qfv_from_f32(value: vec3<f32>) -> QfVec3 {
    return QfVec3(qf_from_f32(value.x), qf_from_f32(value.y), qf_from_f32(value.z));
}

fn qfv_to_f32(value: QfVec3) -> vec3<f32> {
    return vec3<f32>(qf_to_f32(value.x), qf_to_f32(value.y), qf_to_f32(value.z));
}

fn qfv_add(a: QfVec3, b: QfVec3) -> QfVec3 {
    return QfVec3(qf_add(a.x, b.x), qf_add(a.y, b.y), qf_add(a.z, b.z));
}

fn qfv_subtract(a: QfVec3, b: QfVec3) -> QfVec3 {
    return QfVec3(
        qf_subtract(a.x, b.x),
        qf_subtract(a.y, b.y),
        qf_subtract(a.z, b.z),
    );
}

fn qfv_multiply(a: QfVec3, scalar: Qf32) -> QfVec3 {
    return QfVec3(
        qf_multiply(a.x, scalar),
        qf_multiply(a.y, scalar),
        qf_multiply(a.z, scalar),
    );
}

fn qfv_dot(a: QfVec3, b: QfVec3) -> Qf32 {
    return qf_add(
        qf_add(qf_multiply(a.x, b.x), qf_multiply(a.y, b.y)),
        qf_multiply(a.z, b.z),
    );
}

fn qfv_uniform_position() -> QfVec3 {
    return QfVec3(
        uniforms.camera_position_qf_x,
        uniforms.camera_position_qf_y,
        uniforms.camera_position_qf_z,
    );
}

fn qfv_uniform_target() -> QfVec3 {
    return QfVec3(
        uniforms.camera_target_qf_x,
        uniforms.camera_target_qf_y,
        uniforms.camera_target_qf_z,
    );
}

fn qfv_camera_basis() -> CameraBasis {
    let difference = qfv_subtract(qfv_uniform_target(), qfv_uniform_position());
    let scale = max(max(abs(difference.x.x), abs(difference.y.x)), abs(difference.z.x));
    return camera_basis_from_forward(
        qfv_to_f32(qfv_multiply(difference, qf_from_f32(1.0 / scale))),
    );
}

fn qfv_camera_ray(pixel: vec2<f32>) -> vec3<f32> {
    let resolution = uniforms.resolution_time_frame.xy;
    var screen = (2.0 * pixel - resolution) / resolution.y;
    screen.y = -screen.y;

    let basis = qfv_camera_basis();
    let focal_scale = tan(0.5 * uniforms.camera_position_fov.w);
    return safe_normalize(
        basis.forward + focal_scale * (screen.x * basis.right + screen.y * basis.up),
        basis.forward,
    );
}
