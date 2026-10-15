//! Core types shared across the OpenVCam engine: pixel formats, frames, the
//! processing graph (DAG) abstraction, and the presentation clock.
//!
//! This crate has no GPU, ML, or OS dependencies — it defines the vocabulary the
//! other crates speak.

pub mod convert;
pub mod format;
pub mod frame;
pub mod graph;
pub mod time;

pub use format::{FourCc, PixelFormat, VideoFormat};
pub use frame::{CpuFrame, Frame, FrameRef, TextureHandle};
pub use graph::{Node, NodeId, NodeKind, Pipeline, ProcessCtx};
pub use time::{Clock, Timestamp};

/// Crate-wide error type.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("pipeline has a cycle or dangling edge")]
    InvalidGraph,
    #[error("format {0:?} is not supported by this node")]
    UnsupportedFormat(PixelFormat),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
