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
    let ray_origin = uniforms.camera_position_fov.xyz;
    let ray_direction = camera_ray(fragment_position.xy);
    let max_distance = uniforms.fractal.w;
    let epsilon = uniforms.fractal.z;
    var travel = 0.0;
    var steps = 0u;
    var hit = false;

    for (var step = 0u; step < MAX_RAY_STEPS; step += 1u) {
        if step >= uniforms.limits.y {
            break;
        }
        steps = step + 1u;
        let distance = map(ray_origin + ray_direction * travel);
        // NaN is the only IEEE-754 value unequal to itself. Negative DE values
        // can legitimately occur just inside the fractal and count as a hit.
        if distance != distance {
            break;
        }
        let hit_epsilon = epsilon * max(1.0, travel * 0.1);
        if distance < hit_epsilon {
            hit = true;
            break;
        }
        travel += max(distance, epsilon * 0.25);
        if travel > max_distance {
            break;
        }
    }

    let sky = background(ray_direction);
    if !hit {
        return vec4<f32>(sky, 1.0);
    }

    let hit_position = ray_origin + ray_direction * travel;
    let step_ratio = f32(steps) / max(f32(uniforms.limits.y), 1.0);
    let surface = shade_surface(hit_position, ray_direction, step_ratio);
    let fog = 1.0 - exp(-0.012 * travel * travel);
    return vec4<f32>(clamp(mix(surface, sky, fog), vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
