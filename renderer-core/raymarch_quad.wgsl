const MAX_RAY_STEPS: u32 = 1024u;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var output: VertexOutput;
    output.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    return output;
}

@fragment
fn fs_main(@builtin(position) fragment_position: vec4<f32>) -> @location(0) vec4<f32> {
    let ray_origin = qfv_mandelbox_local(qfv_uniform_position());
    let ray_direction = qfv_camera_ray(fragment_position.xy);
    let max_distance = uniforms.render_params.y;
    let epsilon = uniforms.render_params.x;
    let step_safety = uniforms.render_params.z;
    let pixel_epsilon_multiplier = uniforms.render_params.w;
    var travel = 0.0;
    var steps = 0u;
    var hit = false;

    for (var step = 0u; step < MAX_RAY_STEPS; step += 1u) {
        if step >= uniforms.limits.y {
            break;
        }
        steps = step + 1u;
        let point = qfv_add(
            ray_origin,
            qfv_multiply(qfv_from_f32(ray_direction), qf_from_f32(travel)),
        );
        let distance = map_qf(point);
        if distance != distance {
            break;
        }
        let pixel_angle = uniforms.camera_position_fov.w
            / min(uniforms.resolution_time_frame.x, uniforms.resolution_time_frame.y);
        let hit_epsilon = clamp(
            max(epsilon, travel * pixel_angle * pixel_epsilon_multiplier),
            epsilon,
            0.1,
        );
        if distance < hit_epsilon {
            hit = true;
            break;
        }
        travel += max(distance * step_safety, epsilon * 0.25);
        if travel > max_distance {
            break;
        }
    }

    let sky = fractal_background(ray_direction);
    if !hit {
        return vec4<f32>(sky, 1.0);
    }

    let hit_position = qfv_add(
        ray_origin,
        qfv_multiply(qfv_from_f32(ray_direction), qf_from_f32(travel)),
    );
    let step_ratio = f32(steps) / max(f32(uniforms.limits.y), 1.0);
    let pixel_angle = uniforms.camera_position_fov.w
        / min(uniforms.resolution_time_frame.x, uniforms.resolution_time_frame.y);
    let surface_epsilon = clamp(
        max(epsilon, travel * pixel_angle * pixel_epsilon_multiplier),
        epsilon,
        0.1,
    );
    let surface = shade_surface_qf(hit_position, ray_direction, step_ratio, surface_epsilon);
    let final_color = apply_fractal_atmosphere(surface, sky, travel);
    return vec4<f32>(clamp(final_color, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
