//! Native MP4 passthrough: AVAssetWriter, Media Foundation Sink Writer,
//! Android AMediaMuxer, or system GStreamer mp4mux. No FFmpeg or re-encoding.
//! Configuration is fixed for the file: start a new writer when encoder output
//! announces a different format. AAC priming/padding is retained; gapless trim
//! metadata is not yet available from the codec profile.
//! Native writers may add decoder preroll and edit lists around the submitted
//! AAC access units. The movie timeline is distinct from the raw sample table.

use std::path::Path;
use xavi_core::mux::{Engine, Mp4Config, Track};
use xavi_core::{EncodedChunk, Error, Result, VideoColorSpace};

#[cfg(target_vendor = "apple")]
#[path = "mux/apple.rs"]
mod port;
#[cfg(target_os = "android")]
#[path = "mux/android.rs"]
mod port;
#[cfg(any(target_os = "windows", target_os = "linux"))]
use crate::native::port::mux::Backend;
#[cfg(any(target_vendor = "apple", target_os = "android"))]
use port::Backend;

// COM and platform writer objects must stay on their creating worker thread.
pub struct NativeMuxer(Backend, std::marker::PhantomData<std::rc::Rc<()>>);
impl Engine for NativeMuxer {
    fn open(path: &Path, config: &Mp4Config) -> Result<Self> {
        if let Some(c) = &config.audio
            && crate::audio_config(&c.codec, c.sample_rate, c.channels)?.as_slice()
                != c.description.as_ref()
        {
            return Err(Error::invalid(
                "AAC track description does not match its format",
            ));
        }
        if let Some(c) = &config.video {
            let (w, h) = c
                .coded_width
                .zip(c.coded_height)
                .ok_or_else(|| Error::invalid("MP4 video requires coded dimensions"))?;
            crate::video_config(crate::H264, w, h)?;
            crate::bitstream::parameter_sets(&c.description)?;
            if c.description[1] != 66
                || c.codec.to_ascii_uppercase()
                    != format!(
                        "AVC1.{:02X}{:02X}{:02X}",
                        c.description[1], c.description[2], c.description[3]
                    )
            {
                return Err(Error::unsupported(
                    "MP4 profile requires matching Baseline AVC configuration",
                ));
            }
            if c.color_space != VideoColorSpace::default() {
                return Err(Error::unsupported(
                    "explicit MP4 color metadata is not yet supported",
                ));
            }
        }
        Ok(Self(Backend::open(path, config)?, std::marker::PhantomData))
    }
    fn write(
        &mut self,
        track: Track,
        chunk: &EncodedChunk,
        timestamp: i64,
        duration: u64,
    ) -> Result<bool> {
        if chunk.bytes().is_empty()
            || chunk.bytes().len() > crate::MAX_BYTES
            || timestamp < 0
            || duration == 0
            || (timestamp as u64)
                .checked_add(duration)
                .is_none_or(|v| v > i64::MAX as u64 / 1000)
        {
            return Err(Error::invalid("invalid native mux packet or timing"));
        }
        if track == Track::Video {
            crate::bitstream::validate_access_unit(chunk.bytes())?;
        }
        self.0.write(track, chunk, timestamp, duration)
    }
    fn end_track(&mut self, track: Track) -> Result<()> {
        self.0.end_track(track)
    }
    fn finish(&mut self) -> Result<bool> {
        self.0.finish()
    }
}

#[cfg(not(any(
    target_vendor = "apple",
    target_os = "android",
    target_os = "windows",
    target_os = "linux"
)))]
struct Backend;
#[cfg(not(any(
    target_vendor = "apple",
    target_os = "android",
    target_os = "windows",
    target_os = "linux"
)))]
impl Backend {
    fn open(_: &Path, _: &Mp4Config) -> Result<Self> {
        Err(Error::unsupported("no platform MP4 writer on this target"))
    }
    fn write(&mut self, _: Track, _: &EncodedChunk, _: i64, _: u64) -> Result<bool> {
        unreachable!()
    }
    fn end_track(&mut self, _: Track) -> Result<()> {
        unreachable!()
    }
    fn finish(&mut self) -> Result<bool> {
        unreachable!()
    }
}
