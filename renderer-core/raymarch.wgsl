const MAX_RAY_STEPS: u32 = 1024u;
const MAX_SECONDARY_RAY_STEPS: u32 = 256u;
const MAX_SAMPLES_PER_PIXEL: u32 = 128u;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
}

struct RayHit {
    position: vec3<f32>,
    direction: vec3<f32>,
    travel: f32,
    steps: u32,
    hit: bool,
    epsilon: f32,
    // Light gathered from the surfaces the ray passed close to and missed.
    glow: f32,
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

fn ray_hit_epsilon(travel: f32, pixel_angle: f32, min_epsilon: f32) -> f32 {
    let epsilon = max(uniforms.render_params.x, min_epsilon);
    return clamp(
        max(
            epsilon,
            travel * pixel_angle * uniforms.render_params.w,
        ),
        epsilon,
        max(0.1, epsilon),
    );
}

fn trace_ray(
    origin: vec3<f32>,
    direction: vec3<f32>,
    max_distance: f32,
    max_steps: u32,
    pixel_angle: f32,
    // Secondary rays inherit the precision their surface was found with;
    // a finer test would find that same surface again.
    min_epsilon: f32,
) -> RayHit {
    let epsilon = uniforms.render_params.x;
    var travel = 0.0;
    var steps = 0u;
    var hit = false;
    var hit_epsilon = epsilon;
    var glow = 0.0;
    var previous_distance = 1.0e30;
    var previous_travel = 0.0;
    var approaching = false;

    for (var step = 0u; step < MAX_RAY_STEPS; step += 1u) {
        if step >= max_steps {
            break;
        }
        steps = step + 1u;
        let distance = map(origin + direction * travel);
        if distance != distance {
            break;
        }
        hit_epsilon = ray_hit_epsilon(travel, pixel_angle, min_epsilon);
        if distance < hit_epsilon {
            hit = true;
            break;
        }
        // The distance turning from falling to rising marks a near miss.
        if approaching && distance > previous_distance {
            glow += fractal_glow_weight(previous_distance, previous_travel);
        }
        approaching = distance < previous_distance;
        previous_distance = distance;
        previous_travel = travel;
        travel += max(distance * uniforms.render_params.z, epsilon * 0.25);
        if travel > max_distance {
            break;
        }
    }
    return RayHit(origin + direction * travel, direction, travel, steps, hit, hit_epsilon, glow);
}

fn trace_secondary_visibility(
    position: vec3<f32>,
    normal: vec3<f32>,
    direction: vec3<f32>,
    surface_epsilon: f32,
    max_distance: f32,
    max_steps: u32,
    // Half-angle of the cone the ray stands for. Occluders thinner than the
    // cone at their distance are passed by, which is what a sampled area
    // light would average them to, without the noise of sampling them.
    cone_angle: f32,
) -> f32 {
    if max_steps == 0u {
        return 1.0;
    }
    let origin = position + normal * max(surface_epsilon * 4.0, uniforms.render_params.x * 4.0);
    let result = trace_ray(
        origin,
        direction,
        max_distance,
        max_steps,
        cone_angle,
        surface_epsilon,
    );
    return select(1.0, 0.0, result.hit);
}

fn sample_camera_ray(
    pixel: vec2<f32>,
    state: ptr<function, u32>,
) -> RayHit {
    var jitter = vec2<f32>(0.0);
    if uniforms.quality_limits.x > 1u {
        jitter = vec2<f32>(random_01(state), random_01(state)) - vec2<f32>(0.5);
    }
    let pinhole_direction = camera_ray(pixel + jitter);
    var origin = uniforms.camera_position_fov.xyz;
    var direction = pinhole_direction;
    if uniforms.camera_lens.x > 0.0 {
        let basis = camera_basis();
        let disk = sample_unit_disk(state) * uniforms.camera_lens.x;
        let lens_offset = disk.x * basis.right + disk.y * basis.up;
        let focus_travel = uniforms.camera_lens.y
            / max(dot(pinhole_direction, basis.forward), 1.0e-4);
        let focus_point = origin + pinhole_direction * focus_travel;
        origin += lens_offset;
        direction = safe_normalize(focus_point - origin, pinhole_direction);
    }
    let pixel_angle = uniforms.camera_position_fov.w
        / min(uniforms.resolution_time_frame.x, uniforms.resolution_time_frame.y);
    return trace_ray(
        origin,
        direction,
        uniforms.render_params.y,
        uniforms.limits.y,
        pixel_angle,
        0.0,
    );
}

fn shade_sample(pixel: vec2<f32>, sample_index: u32) -> vec3<f32> {
    var state = random_seed(pixel, sample_index);
    let primary = sample_camera_ray(pixel, &state);
    let primary_direction = primary.direction;
    let sky = fractal_background(primary_direction);
    if !primary.hit {
        return fractal_haze(sky, primary.travel, false, primary.glow);
    }

    let normal = estimate_normal(primary.position, primary.epsilon);
    let base_light = safe_normalize(
        uniforms.light_direction.xyz,
        vec3<f32>(-0.4, 0.8, 0.5),
    );
    let light = sample_direction_disk(base_light, uniforms.soft_shadow.x, &state);
    var direct_visibility = 1.0;
    if dot(normal, light) > 0.0 {
        direct_visibility = trace_secondary_visibility(
            primary.position,
            normal,
            light,
            primary.epsilon,
            uniforms.soft_shadow.y,
            uniforms.quality_limits.z,
            uniforms.soft_shadow.x,
        );
    }

    var ambient_visibility = 1.0;
    if uniforms.quality_limits.y > 0u && uniforms.ambient_occlusion.y > 0.0 {
        let ambient_direction = sample_cosine_hemisphere(normal, &state);
        let unoccluded = trace_secondary_visibility(
            primary.position,
            normal,
            ambient_direction,
            primary.epsilon,
            uniforms.ambient_occlusion.x,
            uniforms.quality_limits.y,
            0.0,
        );
        ambient_visibility = mix(1.0, unoccluded, uniforms.ambient_occlusion.y);
    }

    let step_ratio = f32(primary.steps) / max(f32(uniforms.limits.y), 1.0);
    var surface = shade_fractal(
        primary.position,
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
        let reflection_origin = primary.position
            + normal * max(primary.epsilon * 4.0, uniforms.render_params.x * 4.0);
        let reflected = trace_ray(
            reflection_origin,
            reflected_direction,
            uniforms.reflection.z,
            min(uniforms.quality_limits.w, MAX_SECONDARY_RAY_STEPS),
            0.0,
            primary.epsilon * 0.5,
        );
        var reflected_color = fractal_background(reflected_direction);
        if reflected.hit {
            let reflected_normal = estimate_normal(reflected.position, reflected.epsilon);
            let reflected_step_ratio = f32(reflected.steps)
                / max(f32(uniforms.quality_limits.w), 1.0);
            reflected_color = shade_fractal(
                reflected.position,
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

    return fractal_haze(
        apply_fractal_atmosphere(surface, sky, primary.travel),
        primary.travel,
        true,
        primary.glow,
    );
}

@fragment
fn fs_main(@builtin(position) fragment_position: vec4<f32>) -> @location(0) vec4<f32> {
    var accumulated = vec3<f32>(0.0);
    let sample_count = min(uniforms.quality_limits.x, MAX_SAMPLES_PER_PIXEL);
    for (var sample_index = 0u; sample_index < MAX_SAMPLES_PER_PIXEL; sample_index += 1u) {
        if sample_index >= sample_count {
            break;
        }
        accumulated += shade_sample(fragment_position.xy, sample_index);
    }
    let hdr_color = accumulated / max(f32(sample_count), 1.0);
    return vec4<f32>(apply_post_process(hdr_color, fragment_position.xy), 1.0);
}
