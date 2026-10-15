//! Built-in effects mirroring the ManyCam feature set, implemented as GPU shaders.

use bytemuck::{Pod, Zeroable};

use crate::Effect;

/// Brightness / contrast / saturation / temperature color grade.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct ColorGradeParams {
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub temperature: f32,
}

#[derive(Clone)]
pub struct ColorGrade {
    pub params: ColorGradeParams,
}

impl Default for ColorGrade {
    fn default() -> Self {
        Self {
            params: ColorGradeParams { brightness: 0.0, contrast: 1.0, saturation: 1.0, temperature: 0.0 },
        }
    }
}

impl ColorGrade {
    /// Whether the current settings change the image at all.
    pub fn is_identity(&self) -> bool {
        let p = &self.params;
        p.brightness == 0.0 && p.contrast == 1.0 && p.saturation == 1.0 && p.temperature == 0.0
    }

    /// The grade as one affine color matrix (so it can be fused with color filters):
    ///   c1  = C·c + K                       (contrast/brightness, K = 255·(0.5 − 0.5C + B))
    ///   out = s·c1 + (1−s)·luma(c1) ± T     (saturation, temperature)
    /// and since the luma weights sum to 1 the offsets collapse, giving
    ///   out = C·(s·I + (1−s)·1·wᵀ)·rgb + K + (T, 0, −T).
    pub fn matrix(&self) -> crate::cpu::ColorMatrix {
        let p = &self.params;
        let mut cm = crate::cpu::ColorMatrix::saturation(p.saturation);
        for row in cm.m.iter_mut() {
            for e in row.iter_mut() {
                *e *= p.contrast;
            }
        }
        let k = 255.0 * (0.5 - 0.5 * p.contrast + p.brightness);
        let t = 255.0 * 0.1 * p.temperature;
        cm.off = [k + t, k, k - t];
        cm
    }

    /// Fast grade `src` → `dst` (RGBA8, same length), matching [`Effect::apply_cpu`] / the
    /// shader to within ±2 levels. Cheaper per frame than a GPU round trip.
    pub fn apply_fast(&self, src: &[u8], dst: &mut [u8]) {
        self.matrix().apply(src, dst);
    }
}

impl Effect for ColorGrade {
    fn id(&self) -> &'static str {
        "color_grade"
    }
    fn wgsl(&self) -> &'static str {
        include_str!("../../../shaders/color_grade.wgsl")
    }
    fn uniform_bytes(&self) -> Vec<u8> {
        bytemuck::bytes_of(&self.params).to_vec()
    }
    fn has_cpu(&self) -> bool {
        true
    }
    /// In-place grade on RGBA8, matching `shaders/color_grade.wgsl`.
    fn apply_cpu(&self, rgba: &mut [u8], _w: u32, _h: u32) {
        let p = &self.params;
        for px in rgba.chunks_exact_mut(4) {
            let mut c = [
                px[0] as f32 / 255.0,
                px[1] as f32 / 255.0,
                px[2] as f32 / 255.0,
            ];
            // brightness + contrast around 0.5
            for ch in c.iter_mut() {
                *ch = (*ch - 0.5) * p.contrast + 0.5 + p.brightness;
            }
            // saturation
            let l = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
            for ch in c.iter_mut() {
                *ch = l + (*ch - l) * p.saturation;
            }
            // temperature: warm (+) / cool (-)
            c[0] += p.temperature * 0.1;
            c[2] -= p.temperature * 0.1;

            px[0] = (c[0].clamp(0.0, 1.0) * 255.0).round() as u8;
            px[1] = (c[1].clamp(0.0, 1.0) * 255.0).round() as u8;
            px[2] = (c[2].clamp(0.0, 1.0) * 255.0).round() as u8;
            // px[3] (alpha) untouched
        }
    }
}

/// Background blur: samples the segmentation mask (`extra_inputs = ["mask"]`) to blur
/// only the background, keeping the subject sharp.
#[repr(C)]
#[derive(Clone, Copy, Default, Pod, Zeroable)]
pub struct BackgroundBlurParams {
    pub radius: f32,
    pub _pad: [f32; 3],
}

#[derive(Clone)]
pub struct BackgroundBlur {
    pub params: BackgroundBlurParams,
}

impl Default for BackgroundBlur {
    fn default() -> Self {
        Self { params: BackgroundBlurParams { radius: 4.0, _pad: [0.0; 3] } }
    }
}

impl Effect for BackgroundBlur {
    fn id(&self) -> &'static str {
        "background_blur"
    }
    fn wgsl(&self) -> &'static str {
        include_str!("../../../shaders/background_blur.wgsl")
    }
    fn uniform_bytes(&self) -> Vec<u8> {
        bytemuck::bytes_of(&self.params).to_vec()
    }
    fn extra_inputs(&self) -> &[&'static str] {
        &["mask"]
    }
}

/// Classic green-screen chroma key.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct ChromaKeyParams {
    pub key_rgb: [f32; 3],
    pub similarity: f32,
    pub smoothness: f32,
    pub _pad: [f32; 3],
}

#[derive(Clone)]
pub struct ChromaKey {
    pub params: ChromaKeyParams,
}

impl Default for ChromaKey {
    fn default() -> Self {
        Self {
            params: ChromaKeyParams {
                key_rgb: [0.0, 1.0, 0.0],
                similarity: 0.4,
                smoothness: 0.1,
                _pad: [0.0; 3],
            },
        }
    }
}

impl Effect for ChromaKey {
    fn id(&self) -> &'static str {
        "chroma_key"
    }
    fn wgsl(&self) -> &'static str {
        include_str!("../../../shaders/chroma_key.wgsl")
    }
    fn uniform_bytes(&self) -> Vec<u8> {
        bytemuck::bytes_of(&self.params).to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_grade_matches_float() {
        let src: Vec<u8> = (0..=255u8).flat_map(|v| [v, 255 - v, v / 2, 255]).collect();
        for params in [
            ColorGradeParams { brightness: 0.05, contrast: 1.0, saturation: 1.0, temperature: 0.0 },
            ColorGradeParams { brightness: -0.1, contrast: 1.4, saturation: 0.3, temperature: 0.5 },
            ColorGradeParams { brightness: 0.2, contrast: 0.7, saturation: 1.8, temperature: -1.0 },
        ] {
            let cg = ColorGrade { params };
            let mut slow = src.clone();
            cg.apply_cpu(&mut slow, 256, 1);
            let mut fast = vec![0u8; src.len()];
            cg.apply_fast(&src, &mut fast);
            for (a, b) in slow.iter().zip(&fast) {
                assert!((*a as i32 - *b as i32).abs() <= 2, "slow {a} vs fast {b}");
            }
        }
    }
}
