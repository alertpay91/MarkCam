// Compute shader: pack an RGBA8 texture into an NV12 byte buffer (BT.601 limited range).
//
// Output layout in `out_buf` (array<u32>, little-endian bytes):
//   Y plane:  width*height bytes, Y[y*width + x]
//   UV plane: interleaved U,V per 2x2 block at offset width*height
//
// Each invocation writes exactly one u32 word (4 bytes) so no two threads touch the same
// word — avoids read-modify-write races on the byte-packed buffer. Requires width % 4 == 0
// and height % 2 == 0 (enforced by the caller).

struct Dims { width: u32, height: u32, y_words: u32, uv_words: u32 };

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> out_buf: array<u32>;
@group(0) @binding(2) var<uniform> d: Dims;

fn load_rgb(x: u32, y: u32) -> vec3<f32> {
    return textureLoad(src, vec2<i32>(i32(x), i32(y)), 0).rgb * 255.0;
}
fn to_y(c: vec3<f32>) -> f32 {
    return clamp(0.257 * c.r + 0.504 * c.g + 0.098 * c.b + 16.0, 0.0, 255.0);
}
fn to_u(c: vec3<f32>) -> f32 {
    return clamp(-0.148 * c.r - 0.291 * c.g + 0.439 * c.b + 128.0, 0.0, 255.0);
}
fn to_v(c: vec3<f32>) -> f32 {
    return clamp(0.439 * c.r - 0.368 * c.g - 0.071 * c.b + 128.0, 0.0, 255.0);
}
fn pack4(a: f32, b: f32, c: f32, e: f32) -> u32 {
    return u32(a + 0.5) | (u32(b + 0.5) << 8u) | (u32(c + 0.5) << 16u) | (u32(e + 0.5) << 24u);
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let wi = gid.x;
    if (wi >= d.y_words + d.uv_words) {
        return;
    }
    if (wi < d.y_words) {
        // One Y word = 4 horizontally-adjacent luma samples (width % 4 == 0 => same row).
        let p = wi * 4u;
        let y = p / d.width;
        let x0 = p % d.width;
        out_buf[wi] = pack4(
            to_y(load_rgb(x0, y)),
            to_y(load_rgb(x0 + 1u, y)),
            to_y(load_rgb(x0 + 2u, y)),
            to_y(load_rgb(x0 + 3u, y)),
        );
    } else {
        // One UV word = two chroma blocks: [U0, V0, U1, V1].
        let j = wi - d.y_words;
        let cw = d.width / 2u;          // chroma blocks per row
        let words_per_row = cw / 2u;    // 2 blocks per u32
        let cy = j / words_per_row;
        let cx0 = (j % words_per_row) * 2u;
        let by = cy * 2u;

        let bx0 = cx0 * 2u;
        let a0 = load_rgb(bx0, by);
        let a1 = load_rgb(bx0 + 1u, by);
        let a2 = load_rgb(bx0, by + 1u);
        let a3 = load_rgb(bx0 + 1u, by + 1u);
        let u0 = (to_u(a0) + to_u(a1) + to_u(a2) + to_u(a3)) * 0.25;
        let v0 = (to_v(a0) + to_v(a1) + to_v(a2) + to_v(a3)) * 0.25;

        let bx1 = (cx0 + 1u) * 2u;
        let b0 = load_rgb(bx1, by);
        let b1 = load_rgb(bx1 + 1u, by);
        let b2 = load_rgb(bx1, by + 1u);
        let b3 = load_rgb(bx1 + 1u, by + 1u);
        let u1 = (to_u(b0) + to_u(b1) + to_u(b2) + to_u(b3)) * 0.25;
        let v1 = (to_v(b0) + to_v(b1) + to_v(b2) + to_v(b3)) * 0.25;

        out_buf[wi] = pack4(u0, v0, u1, v1);
    }
}
