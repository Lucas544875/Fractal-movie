const MAX_RAY_STEPS: u32 = 1024u;
const MAX_SECONDARY_RAY_STEPS: u32 = 256u;
const MAX_SAMPLES_PER_PIXEL: u32 = 128u;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
}

struct RayHitQf {
    position: QfVec3,
    direction: vec3<f32>,
    travel: f32,
    steps: u32,
    hit: bool,
    epsilon: f32,
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

fn ray_hit_epsilon_qf(travel: f32, pixel_angle: f32) -> f32 {
    let epsilon = uniforms.render_params.x;
    return clamp(
        max(
            epsilon,
            travel * pixel_angle * uniforms.render_params.w,
        ),
        epsilon,
        0.1,
    );
}

fn trace_ray_qf(
    origin: QfVec3,
    direction: vec3<f32>,
    max_distance: f32,
    max_steps: u32,
    pixel_angle: f32,
) -> RayHitQf {
    let epsilon = uniforms.render_params.x;
    var travel = 0.0;
    var steps = 0u;
    var hit = false;
    var hit_epsilon = epsilon;

    for (var step = 0u; step < MAX_RAY_STEPS; step += 1u) {
        if step >= max_steps {
            break;
        }
        steps = step + 1u;
        let point = qfv_add(
            origin,
            qfv_multiply(qfv_from_f32(direction), qf_from_f32(travel)),
        );
        let distance = map_qf(point);
        if distance != distance {
            break;
        }
        hit_epsilon = ray_hit_epsilon_qf(travel, pixel_angle);
        if distance < hit_epsilon {
            hit = true;
            break;
        }
        travel += max(distance * uniforms.render_params.z, epsilon * 0.25);
        if travel > max_distance {
            break;
        }
    }
    let position = qfv_add(
        origin,
        qfv_multiply(qfv_from_f32(direction), qf_from_f32(travel)),
    );
    return RayHitQf(position, direction, travel, steps, hit, hit_epsilon);
}

fn trace_secondary_visibility_qf(
    position: QfVec3,
    normal: vec3<f32>,
    direction: vec3<f32>,
    surface_epsilon: f32,
    max_distance: f32,
    max_steps: u32,
) -> f32 {
    if max_steps == 0u {
        return 1.0;
    }
    let bias = max(surface_epsilon * 4.0, uniforms.render_params.x * 4.0);
    let origin = qfv_add(
        position,
        qfv_multiply(qfv_from_f32(normal), qf_from_f32(bias)),
    );
    let result = trace_ray_qf(origin, direction, max_distance, max_steps, 0.0);
    return select(1.0, 0.0, result.hit);
}

fn sample_camera_ray_qf(
    pixel: vec2<f32>,
    state: ptr<function, u32>,
) -> RayHitQf {
    var jitter = vec2<f32>(0.0);
    if uniforms.quality_limits.x > 1u {
        jitter = vec2<f32>(random_01(state), random_01(state)) - vec2<f32>(0.5);
    }
    let pinhole_direction = qfv_camera_ray(pixel + jitter);
    var origin = qfv_mandelbox_local(qfv_uniform_position());
    var direction = pinhole_direction;
    if uniforms.camera_lens.x > 0.0 {
        let basis = qfv_camera_basis();
        let disk = sample_unit_disk(state) * uniforms.camera_lens.x;
        let lens_offset = disk.x * basis.right + disk.y * basis.up;
        let focus_travel = uniforms.camera_lens.y
            / max(dot(pinhole_direction, basis.forward), 1.0e-4);
        let focus_point = qfv_add(
            origin,
            qfv_multiply(qfv_from_f32(pinhole_direction), qf_from_f32(focus_travel)),
        );
        origin = qfv_add(origin, qfv_from_f32(lens_offset));
        let focus_delta = qfv_subtract(focus_point, origin);
        direction = safe_normalize(qfv_to_f32(focus_delta), pinhole_direction);
    }
    let pixel_angle = uniforms.camera_position_fov.w
        / min(uniforms.resolution_time_frame.x, uniforms.resolution_time_frame.y);
    return trace_ray_qf(
        origin,
        direction,
        uniforms.render_params.y,
        uniforms.limits.y,
        pixel_angle,
    );
}

fn shade_sample_qf(pixel: vec2<f32>, sample_index: u32) -> vec3<f32> {
    var state = random_seed(pixel, sample_index);
    let primary = sample_camera_ray_qf(pixel, &state);
    let primary_direction = primary.direction;
    let sky = fractal_background(primary_direction);
    if !primary.hit {
        return sky;
    }

    let normal = estimate_normal_qf(primary.position, primary.epsilon);
    let base_light = safe_normalize(
        uniforms.light_direction.xyz,
        vec3<f32>(-0.4, 0.8, 0.5),
    );
    let light = sample_direction_disk(base_light, uniforms.soft_shadow.x, &state);
    var direct_visibility = 1.0;
    if dot(normal, light) > 0.0 {
        direct_visibility = trace_secondary_visibility_qf(
            primary.position,
            normal,
            light,
            primary.epsilon,
            uniforms.soft_shadow.y,
            uniforms.quality_limits.z,
        );
    }

    var ambient_visibility = 1.0;
    if uniforms.quality_limits.y > 0u && uniforms.ambient_occlusion.y > 0.0 {
        let ambient_direction = sample_cosine_hemisphere(normal, &state);
        let unoccluded = trace_secondary_visibility_qf(
            primary.position,
            normal,
            ambient_direction,
            primary.epsilon,
            uniforms.ambient_occlusion.x,
            uniforms.quality_limits.y,
        );
        ambient_visibility = mix(1.0, unoccluded, uniforms.ambient_occlusion.y);
    }

    let step_ratio = f32(primary.steps) / max(f32(uniforms.limits.y), 1.0);
    var surface = shade_fractal(
        qfv_mandelbox_local_to_f32(primary.position),
        primary_direction,
        normal,
        step_ratio,
        light,
        direct_visibility,
        ambient_visibility,
    );

    if uniforms.quality_limits.w > 0u && uniforms.reflection.x > 0.0 {
        let perfect_reflection = reflect(primary_direction, normal);
        let reflected_direction = sample_direction_disk(
            perfect_reflection,
            uniforms.reflection.y * 0.5,
            &state,
        );
        let bias = max(primary.epsilon * 4.0, uniforms.render_params.x * 4.0);
        let reflection_origin = qfv_add(
            primary.position,
            qfv_multiply(qfv_from_f32(normal), qf_from_f32(bias)),
        );
        let reflected = trace_ray_qf(
            reflection_origin,
            reflected_direction,
            uniforms.reflection.z,
            min(uniforms.quality_limits.w, MAX_SECONDARY_RAY_STEPS),
            0.0,
        );
        var reflected_color = fractal_background(reflected_direction);
        if reflected.hit {
            let reflected_normal = estimate_normal_qf(reflected.position, reflected.epsilon);
            let reflected_step_ratio = f32(reflected.steps)
                / max(f32(uniforms.quality_limits.w), 1.0);
            reflected_color = shade_fractal(
                qfv_mandelbox_local_to_f32(reflected.position),
                reflected_direction,
                reflected_normal,
                reflected_step_ratio,
                light,
                1.0,
                1.0,
            );
            reflected_color = apply_fractal_atmosphere(
                reflected_color,
                fractal_background(reflected_direction),
                reflected.travel,
            );
        }
        surface = mix(surface, reflected_color, uniforms.reflection.x);
    }

    return apply_fractal_atmosphere(surface, sky, primary.travel);
}

@fragment
fn fs_main(@builtin(position) fragment_position: vec4<f32>) -> @location(0) vec4<f32> {
    var accumulated = vec3<f32>(0.0);
    let sample_count = min(uniforms.quality_limits.x, MAX_SAMPLES_PER_PIXEL);
    for (var sample_index = 0u; sample_index < MAX_SAMPLES_PER_PIXEL; sample_index += 1u) {
        if sample_index >= sample_count {
            break;
        }
        accumulated += shade_sample_qf(fragment_position.xy, sample_index);
    }
    let hdr_color = accumulated / max(f32(sample_count), 1.0);
    return vec4<f32>(apply_post_process(hdr_color, fragment_position.xy), 1.0);
}
