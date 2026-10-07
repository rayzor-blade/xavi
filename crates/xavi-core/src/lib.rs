//! Owned media data shared by xavi's runtime adapters and codec backends.
//!
//! These are internal Rust types, not a language ABI. Media bytes are immutable
//! snapshots; clones share storage. Adapters map IDL records onto these types,
//! enforce signed ABI argument ranges, and own guest roots and future delivery.
//! The CPU implementation supports PCM conversion and copying video in its
//! existing pixel format. Color conversion, codecs and GPU surfaces come later.

mod audio;
mod chunk;
mod error;
mod format;
pub mod handles;
mod layout;
pub mod stream;
mod video;

pub use audio::{AudioCopyOptions, AudioData, AudioDescriptor};
pub use chunk::{EncodedChunk, EncodedChunkType};
pub use error::{Error, ErrorKind, Result};
pub use format::{AudioSampleFormat, VideoPixelFormat};
pub use layout::{FrameLayout, Plane, PlaneLayout, Rect};
pub use video::{
    VideoColorPrimaries, VideoColorSpace, VideoCopyOptions, VideoDescriptor, VideoFrame, VideoInfo,
    VideoMatrixCoefficients, VideoTransferCharacteristics,
};

/// The IDL exposes allocation sizes and buffer lengths as unsigned 32-bit values.
pub(crate) fn byte_len(value: u64) -> Result<u32> {
    u32::try_from(value).map_err(|_| Error::invalid("buffer exceeds the 32-bit media size limit"))
}

pub(crate) fn snapshot(bytes: &[u8]) -> Result<std::sync::Arc<[u8]>> {
    byte_len(bytes.len() as u64)?;
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(bytes.len())
        .map_err(|_| Error::exhausted())?;
    owned.extend_from_slice(bytes);
    Ok(owned.into())
}
