//! Native file playback, including demuxing, decoding, audio output and clock.
//! AVPlayer, Media Foundation, Android media APIs, and system GStreamer own
//! decoding and audio output. Frame polling retains only the current BGRA frame.
use std::path::Path;
use std::sync::Arc;
use xavi_core::{Error, Result, VideoFrame};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    Opening,
    Paused,
    Playing,
    Buffering,
    Ended,
}
#[derive(Clone, Copy, Debug)]
pub struct PlaybackInfo {
    pub state: PlaybackState,
    pub position: f64,
    pub duration: f64,
    pub volume: f64,
}

#[cfg(target_vendor = "apple")]
#[path = "player/apple.rs"]
mod native;
#[cfg(target_os = "linux")]
use crate::native::port::player as native;
#[cfg(target_os = "windows")]
#[path = "player/windows.rs"]
mod native;
#[cfg(target_os = "android")]
#[path = "player/android.rs"]
mod native;
#[cfg(not(any(
    target_vendor = "apple",
    target_os = "linux",
    target_os = "windows",
    target_os = "android"
)))]
mod native {
    use super::*;
    pub struct NativePlayer;
    impl NativePlayer {
        pub fn open(_: &Path) -> Result<Self> {
            Err(Error::unsupported(
                "no native playback backend for this target",
            ))
        }
        pub fn info(&mut self) -> Result<PlaybackInfo> {
            unreachable!()
        }
        pub fn command(&mut self, _: i32, _: f64) -> Result<()> {
            unreachable!()
        }
        pub fn frame(&mut self) -> Result<Option<Arc<VideoFrame>>> {
            unreachable!()
        }
    }
}

/// One native player and at most one polled frame awaiting `take_frame`.
/// Apple operations reject calls off the main thread. Drop on another thread
/// schedules native destruction on the main queue; the host must keep pumping.
pub struct Player {
    native: native::NativePlayer,
    pending: Option<Arc<VideoFrame>>,
}
impl Player {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            native: native::NativePlayer::open(path.as_ref())?,
            pending: None,
        })
    }
    pub fn info(&mut self) -> Result<PlaybackInfo> {
        self.native.info()
    }
    pub fn play(&mut self) -> Result<()> {
        self.native.command(0, 0.0)
    }
    pub fn pause(&mut self) -> Result<()> {
        self.native.command(1, 0.0)
    }
    pub fn seek(&mut self, seconds: f64) -> Result<()> {
        if !seconds.is_finite() || !(0.0..=i64::MAX as f64 / 1_000_000.0).contains(&seconds) {
            return Err(Error::invalid(
                "seek time must be finite, nonnegative and representable",
            ));
        }
        let duration = self.info()?.duration;
        self.native.command(
            2,
            if duration > 0.0 {
                seconds.min(duration)
            } else {
                seconds
            },
        )?;
        self.pending = None;
        Ok(())
    }
    pub fn set_volume(&mut self, volume: f64) -> Result<()> {
        if !volume.is_finite() || !(0.0..=1.0).contains(&volume) {
            return Err(Error::invalid("volume must be in 0..=1"));
        }
        self.native.command(3, volume)
    }
    pub fn poll_frame(&mut self) -> Result<bool> {
        // Check affinity/failure even if a prior call already polled a frame.
        self.info()?;
        if self.pending.is_none() {
            self.pending = self.native.frame()?;
        }
        Ok(self.pending.is_some())
    }
    pub fn take_frame(&mut self) -> Result<Arc<VideoFrame>> {
        self.info()?;
        self.pending
            .take()
            .ok_or_else(|| Error::invalid("pollFrame must return true before takeFrame"))
    }
}
