// Fullscreen color-grade pass.
// Vertex stage emits a single oversized triangle; fragment applies the grade.

struct Params {
    brightness: f32,
    contrast: f32,
    saturation: f32,
    temperature: f32,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> p: Params;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // (0,0) (2,0) (0,2) → covers the screen with one triangle.
    let p2 = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var o: VsOut;
    o.uv = p2;
    o.pos = vec4<f32>(p2 * 2.0 - 1.0, 0.0, 1.0);
    o.uv.y = 1.0 - o.uv.y;
    return o;
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    var c = textureSample(src, samp, in.uv).rgb;

    // brightness + contrast around 0.5
    c = (c - 0.5) * p.contrast + 0.5 + p.brightness;

    // saturation
    let l = luma(c);
    c = mix(vec3<f32>(l), c, p.saturation);

    // temperature: push toward warm (+) or cool (-)
    c += vec3<f32>(p.temperature, 0.0, -p.temperature) * 0.1;

    return vec4<f32>(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
