//! Effects: a WGSL shader + a typed, `bytemuck`-able uniform + optional extra input
//! textures (e.g. a segmentation mask from `ovc-ml`). Effects register by name so the
//! UI and saved projects can reference them declaratively.

use std::collections::HashMap;

pub mod anim;
pub mod builtin;
pub mod cpu;

/// An effect's parameter block must be plain-old-data so we can upload it as a uniform.
pub trait EffectParams: bytemuck::Pod + bytemuck::Zeroable + Clone + Send {}
impl<T: bytemuck::Pod + bytemuck::Zeroable + Clone + Send> EffectParams for T {}

/// A GPU effect. One `Effect` instance = one configured node in the graph.
pub trait Effect: Send {
    /// Stable id, e.g. `"color_grade"`, `"background_blur"`.
    fn id(&self) -> &'static str;
    /// WGSL source for this effect's fragment stage.
    fn wgsl(&self) -> &'static str;
    /// Raw bytes of the current uniform block (uploaded each frame if dirty).
    fn uniform_bytes(&self) -> Vec<u8>;
    /// Names of extra sampled inputs besides the main color texture (e.g. `["mask"]`).
    fn extra_inputs(&self) -> &[&'static str] {
        &[]
    }

    /// CPU fallback applied in-place to a packed RGBA8 buffer (`w*h*4` bytes).
    ///
    /// This is the baseline path used by the headless run loop until the GPU render
    /// graph is wired. Default is a no-op; effects that have a CPU implementation
    /// override it. Effects that require GPU-only inputs (e.g. a segmentation mask)
    /// leave this as a no-op.
    fn apply_cpu(&self, _rgba: &mut [u8], _w: u32, _h: u32) {}

    /// Whether `apply_cpu` does anything, so the caller can warn on no-op effects.
    fn has_cpu(&self) -> bool {
        false
    }
}

/// Maps effect ids to constructors so projects/UI can instantiate by name.
#[derive(Default)]
pub struct Registry {
    ctors: HashMap<&'static str, fn() -> Box<dyn Effect>>,
}

impl Registry {
    pub fn with_builtins() -> Self {
        let mut r = Registry::default();
        r.register("color_grade", || Box::new(builtin::ColorGrade::default()));
        r.register("background_blur", || Box::new(builtin::BackgroundBlur::default()));
        r.register("chroma_key", || Box::new(builtin::ChromaKey::default()));
        r
    }
    pub fn register(&mut self, id: &'static str, ctor: fn() -> Box<dyn Effect>) {
        self.ctors.insert(id, ctor);
    }
    pub fn create(&self, id: &str) -> Option<Box<dyn Effect>> {
        self.ctors.get(id).map(|c| c())
    }
    pub fn ids(&self) -> impl Iterator<Item = &&'static str> {
        self.ctors.keys()
    }
}
