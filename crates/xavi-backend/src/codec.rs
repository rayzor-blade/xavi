//! Native framework codecs. Construct on a codec worker, submit retained core
//! media, pump bounded work and deliver output through the host's own executor.
//! FFmpeg is not a dependency or an implicit fallback.

pub use xavi_core::codec::*;
pub use xavi_platform::{AAC, AudioDecoder, AudioEncoder, H264, VideoDecoder, VideoEncoder};

pub type AudioEncodeSession = Codec<AudioEncoder>;
pub type AudioDecodeSession = Codec<AudioDecoder>;
pub type VideoEncodeSession = Codec<VideoEncoder>;
pub type VideoDecodeSession = Codec<VideoDecoder>;
