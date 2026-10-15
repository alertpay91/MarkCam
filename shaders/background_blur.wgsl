// Gaussian blur (single pass, 5x5 taps). Texel size comes from textureDimensions, so no
// resolution uniform is needed. `radius` scales the tap spread.
//
// NOTE: this blurs the whole frame. True *background* blur gates the blur by a segmentation
// mask (person stays sharp); that mask is produced by ovc-ml (ONNX) and bound as a second
// input texture — a later step. For now the effect demonstrates multi-tap GPU sampling.

struct BB {
    radius: f32,
    _p0: f32,
    _p1: f32,
    _p2: f32,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> p: BB;

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
    let texel = 1.0 / vec2<f32>(textureDimensions(src));
    let step = texel * max(p.radius, 0.0);
    // 5x5 separable Gaussian weights (outer product of [1,4,6,4,1]/16).
    var w = array<f32, 5>(0.0625, 0.25, 0.375, 0.25, 0.0625);
    var acc = vec3<f32>(0.0);
    for (var j = 0; j < 5; j = j + 1) {
        for (var i = 0; i < 5; i = i + 1) {
            let off = vec2<f32>(f32(i - 2), f32(j - 2)) * step;
            acc = acc + textureSample(src, samp, in.uv + off).rgb * (w[i] * w[j]);
        }
    }
    return vec4<f32>(acc, 1.0);
}
