//! Native MP4 writing for Rust worker hosts. Codec output metadata supplies the
//! track configuration; submit encoded chunks incrementally and poll finish.
pub use xavi_core::mux::{Mp4Config, MuxState, Track};
pub type Mp4Writer = xavi_core::mux::Muxer<xavi_platform::mux::NativeMuxer>;
