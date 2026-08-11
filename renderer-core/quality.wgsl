const PI: f32 = 3.14159265358979323846;

fn hash_u32(value: u32) -> u32 {
    var hash = value;
    hash = hash ^ (hash >> 16u);
    hash *= 0x7feb352du;
    hash = hash ^ (hash >> 15u);
    hash *= 0x846ca68bu;
    return hash ^ (hash >> 16u);
}

fn random_seed(pixel: vec2<f32>, sample_index: u32) -> u32 {
    let x = u32(pixel.x);
    let y = u32(pixel.y);
    let frame = u32(uniforms.resolution_time_frame.w);
    return hash_u32(
        uniforms.limits.z ^ hash_u32(x + 0x9e3779b9u * y)
            ^ hash_u32(sample_index + 0x85ebca6bu * frame),
    );
}

fn random_01(state: ptr<function, u32>) -> f32 {
    *state = hash_u32(*state + 0x9e3779b9u);
    return f32(*state >> 8u) * (1.0 / 16777216.0);
}

fn sample_unit_disk(state: ptr<function, u32>) -> vec2<f32> {
    let radius = sqrt(random_01(state));
    let angle = 2.0 * PI * random_01(state);
    return radius * vec2<f32>(cos(angle), sin(angle));
}

fn direction_basis(axis_value: vec3<f32>) -> CameraBasis {
    let axis = safe_normalize(axis_value, vec3<f32>(0.0, 1.0, 0.0));
    let helper = select(
        vec3<f32>(0.0, 0.0, 1.0),
        vec3<f32>(0.0, 1.0, 0.0),
        abs(axis.z) > 0.999,
    );
    let right = safe_normalize(cross(axis, helper), vec3<f32>(1.0, 0.0, 0.0));
    return CameraBasis(axis, right, cross(right, axis));
}

fn sample_cosine_hemisphere(normal: vec3<f32>, state: ptr<function, u32>) -> vec3<f32> {
    let disk = sample_unit_disk(state);
    let axial = sqrt(max(1.0 - dot(disk, disk), 0.0));
    let basis = direction_basis(normal);
    return safe_normalize(
        disk.x * basis.right + disk.y * basis.up + axial * basis.forward,
        basis.forward,
    );
}

fn sample_direction_disk(
    direction: vec3<f32>,
    angular_radius: f32,
    state: ptr<function, u32>,
) -> vec3<f32> {
    if angular_radius <= 0.0 {
        return direction;
    }
    let disk = sample_unit_disk(state) * tan(angular_radius);
    let basis = direction_basis(direction);
    return safe_normalize(
        basis.forward + disk.x * basis.right + disk.y * basis.up,
        basis.forward,
    );
}

fn tone_map(color_value: vec3<f32>) -> vec3<f32> {
    var color = max(color_value, vec3<f32>(0.0));
    if uniforms.tone_mapping.z < 0.5 {
        return clamp(color, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    color *= exp2(uniforms.tone_mapping.x);

    // Mandelbulber 2.26's image pipeline operates in display-coded RGB:
    // brightness -> contrast -> HDR tanh -> saturation -> gamma. The render
    // target performs an sRGB transfer, so convert back to linear before return.
    if uniforms.tone_mapping.w >= 0.5 {
        color *= uniforms.image_adjustments.x;
        color = max(
            (color - vec3<f32>(0.5)) * uniforms.image_adjustments.y
                + vec3<f32>(0.5),
            vec3<f32>(0.0),
        );
        color = tanh(color);
        let saturation_value = sqrt(dot(
            color * color,
            vec3<f32>(0.299, 0.587, 0.114),
        ));
        color = vec3<f32>(saturation_value)
            + (color - vec3<f32>(saturation_value)) * uniforms.image_adjustments.w;
        let display_color = pow(
            clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)),
            vec3<f32>(1.0 / uniforms.image_adjustments.z),
        );
        let low = display_color / vec3<f32>(12.92);
        let high = pow(
            (display_color + vec3<f32>(0.055)) / vec3<f32>(1.055),
            vec3<f32>(2.4),
        );
        return select(high, low, display_color <= vec3<f32>(0.04045));
    }

    let luminance = dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
    if luminance <= 1.0e-8 {
        return color;
    }
    let white_squared = max(
        uniforms.tone_mapping.y * uniforms.tone_mapping.y,
        1.0e-8,
    );
    let mapped_luminance = luminance * (1.0 + luminance / white_squared)
        / (1.0 + luminance);
    return max(color * (mapped_luminance / luminance), vec3<f32>(0.0));
}
