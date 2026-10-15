//! Vision models behind small traits, with ONNX Runtime (`ort`) backends.
//!
//! Inference runs on a worker thread; results (boxes, landmarks, mask) are published for
//! effects to sample on the next frame, so ML never blocks the render cadence.

pub mod face;
pub mod seg;
pub mod warp;

/// Normalized face bounding box (0..1 in image space) plus detector confidence.
#[derive(Debug, Clone, Copy)]
pub struct FaceBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub score: f32,
}

/// One facial landmark in normalized image space.
#[derive(Debug, Clone, Copy)]
pub struct Landmark {
    pub x: f32,
    pub y: f32,
}

/// A single-channel mask (0..1) at some resolution; uploaded as a GPU texture by callers.
#[derive(Debug, Clone)]
pub struct Mask {
    pub width: u32,
    pub height: u32,
    pub data: Vec<f32>,
}

/// An RGB image view for inference input (callers downscale to the model's size).
pub struct ImageView<'a> {
    pub width: u32,
    pub height: u32,
    pub rgb: &'a [u8],
}

pub trait FaceDetector: Send {
    fn detect(&mut self, img: ImageView<'_>) -> anyhow::Result<Vec<FaceBox>>;
}

pub trait LandmarkEstimator: Send {
    fn landmarks(&mut self, img: ImageView<'_>, face: FaceBox) -> anyhow::Result<Vec<Landmark>>;
}

pub trait Segmenter: Send {
    fn segment(&mut self, img: ImageView<'_>) -> anyhow::Result<Mask>;
}

pub mod onnx {
    //! ONNX Runtime backends loaded from `models/manifest.toml`.
    use super::*;

    pub struct OnnxFaceDetector {
        // session: ort::Session,  // loaded from the manifest entry
    }

    impl OnnxFaceDetector {
        pub fn load(_model_path: &str) -> anyhow::Result<Self> {
            todo!("ort::Session::builder().commit_from_file(model_path)")
        }
    }

    impl FaceDetector for OnnxFaceDetector {
        fn detect(&mut self, _img: ImageView<'_>) -> anyhow::Result<Vec<FaceBox>> {
            todo!("preprocess -> run -> decode anchors/NMS")
        }
    }
}
