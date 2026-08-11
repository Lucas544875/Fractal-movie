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

fn camera_ray(pixel: vec2<f32>) -> vec3<f32> {
    let resolution = uniforms.resolution_time_frame.xy;
    var screen = (2.0 * pixel - resolution) / resolution.y;
    screen.y = -screen.y;

    let origin = uniforms.camera_position_fov.xyz;
    let forward = safe_normalize(uniforms.camera_target.xyz - origin, vec3<f32>(0.0, 0.0, -1.0));
    var world_up = safe_normalize(uniforms.camera_up.xyz, vec3<f32>(0.0, 1.0, 0.0));
    if abs(dot(forward, world_up)) > 0.999 {
        world_up = select(
            vec3<f32>(0.0, 1.0, 0.0),
            vec3<f32>(1.0, 0.0, 0.0),
            abs(forward.y) > 0.999,
        );
    }
    let right = safe_normalize(cross(forward, world_up), vec3<f32>(1.0, 0.0, 0.0));
    let up = cross(right, forward);
    let focal_scale = tan(0.5 * uniforms.camera_position_fov.w);
    return safe_normalize(forward + focal_scale * (screen.x * right + screen.y * up), forward);
}
