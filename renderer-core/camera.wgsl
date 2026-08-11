struct RenderUniforms {
    // xy: resolution, z: time in seconds, w: frame index
    resolution_time_frame: vec4<f32>,
    // xyz: position, w: vertical FOV in radians
    camera_position_fov: vec4<f32>,
    camera_target: vec4<f32>,
    // Fractal-specific parameters; interpreted by the selected map() module.
    fractal_primary: vec4<f32>,
    // x: epsilon, y: max distance, z: step safety, w: pixel epsilon multiplier
    render_params: vec4<f32>,
    // x: fractal iterations, y: ray steps, z: seed
    limits: vec4<u32>,
    light_direction: vec4<f32>,
    camera_up: vec4<f32>,
    camera_position_qf_x: vec4<f32>,
    camera_position_qf_y: vec4<f32>,
    camera_position_qf_z: vec4<f32>,
    camera_target_qf_x: vec4<f32>,
    camera_target_qf_y: vec4<f32>,
    camera_target_qf_z: vec4<f32>,
    // x: thin-lens aperture radius, y: focus distance
    camera_lens: vec4<f32>,
    // x: AO radius, y: AO strength
    ambient_occlusion: vec4<f32>,
    // x: light angular radius in radians, y: maximum trace distance
    soft_shadow: vec4<f32>,
    // x: strength, y: roughness, z: maximum trace distance
    reflection: vec4<f32>,
    // x: exposure stops, y: white point, z: enabled
    tone_mapping: vec4<f32>,
    // x: samples/pixel, y: AO steps, z: shadow steps, w: reflection steps
    quality_limits: vec4<u32>,
}

@group(0) @binding(0)
var<uniform> uniforms: RenderUniforms;

fn safe_normalize(value: vec3<f32>, fallback: vec3<f32>) -> vec3<f32> {
    let magnitude_squared = dot(value, value);
    if magnitude_squared < 1.0e-12 || !(magnitude_squared >= 0.0) {
        return fallback;
    }
    return value * inverseSqrt(magnitude_squared);
}

struct CameraBasis {
    forward: vec3<f32>,
    right: vec3<f32>,
    up: vec3<f32>,
}

fn camera_basis_from_forward(forward_value: vec3<f32>) -> CameraBasis {
    let forward = safe_normalize(forward_value, vec3<f32>(0.0, 0.0, -1.0));
    var world_up = safe_normalize(uniforms.camera_up.xyz, vec3<f32>(0.0, 1.0, 0.0));
    if abs(dot(forward, world_up)) > 0.999 {
        world_up = select(
            vec3<f32>(0.0, 1.0, 0.0),
            vec3<f32>(1.0, 0.0, 0.0),
            abs(forward.y) > 0.999,
        );
    }
    let right = safe_normalize(cross(forward, world_up), vec3<f32>(1.0, 0.0, 0.0));
    return CameraBasis(forward, right, cross(right, forward));
}

fn camera_basis() -> CameraBasis {
    return camera_basis_from_forward(
        uniforms.camera_target.xyz - uniforms.camera_position_fov.xyz,
    );
}

fn camera_ray(pixel: vec2<f32>) -> vec3<f32> {
    let resolution = uniforms.resolution_time_frame.xy;
    var screen = (2.0 * pixel - resolution) / resolution.y;
    screen.y = -screen.y;

    let basis = camera_basis();
    let focal_scale = tan(0.5 * uniforms.camera_position_fov.w);
    return safe_normalize(
        basis.forward + focal_scale * (screen.x * basis.right + screen.y * basis.up),
        basis.forward,
    );
}
