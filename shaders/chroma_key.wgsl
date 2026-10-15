// Chroma key: remove pixels close to a key color (default green), fading to black with a
// soft edge. Real background compositing (replace with an image/video) needs a second input
// texture and is a later feature; this keys to black.

struct CK {
    key: vec3<f32>,     // key color, 0..1
    similarity: f32,    // distance below which a pixel is fully keyed
    smoothness: f32,    // soft-edge width above `similarity`
    _p0: f32,
    _p1: f32,
    _p2: f32,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> p: CK;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let p2 = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var o: VsOut;
    o.uv = vec2<f32>(p2.x, 1.0 - p2.y);
    o.pos = vec4<f32>(p2 * 2.0 - 1.0, 0.0, 1.0);
    return o;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, samp, in.uv).rgb;
    let d = distance(c, p.key);
    // mask: 0 near the key color (removed), 1 far from it (kept).
    let mask = smoothstep(p.similarity, p.similarity + max(p.smoothness, 1e-4), d);
    return vec4<f32>(c * mask, 1.0);
}
