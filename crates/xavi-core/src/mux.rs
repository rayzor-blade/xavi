//! Incremental container writing on a host worker. Inputs are already encoded;
//! a muxer never silently re-encodes them. Submit both tracks in time order and
//! retry a rejected write after feeding the other track or scheduling a poll.
//! `end_track` lets a shorter track stop holding up the other track.
//!
//! No whole-file media buffer is kept. Native calls may perform disk I/O, so
//! construct, drive and drop on a worker, not a runtime's event-loop thread.
//! `finish` is a polling barrier; only successful completion publishes the file.
//! Close/drop/failure remove the private partial file. Existing destinations
//! are never overwritten, including a file created while recording is active.
//! Publication requires a filesystem supporting hard links in the output folder.
//! This file sink requires seekable storage; it is not a network byte stream.

use crate::codec::{AudioDecoderConfig, VideoDecoderConfig};
use crate::{EncodedChunk, EncodedChunkType, Error, ErrorKind, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mp4Config {
    pub audio: Option<AudioDecoderConfig>,
    pub video: Option<VideoDecoderConfig>,
    /// A shared origin for both tracks; it maps to zero in the file.
    pub timestamp_origin: i64,
    /// Used only when a video chunk has no duration. Microseconds, positive.
    pub default_video_duration: Option<u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Track {
    Audio,
    Video,
}
impl Track {
    pub fn index(self) -> usize {
        match self {
            Self::Audio => 0,
            Self::Video => 1,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MuxState {
    Writing,
    Finishing,
    Finished,
    Failed,
    Closed,
}

/// The normalized timing is nonnegative, with PTS = DTS for the initial
/// AAC-LC/H.264 Baseline profile. `write(false)` retains no input. Engines own
/// any asynchronous copies and must stop using the path before being dropped.
pub trait Engine: Sized {
    fn open(path: &Path, config: &Mp4Config) -> Result<Self>;
    fn write(
        &mut self,
        track: Track,
        chunk: &EncodedChunk,
        timestamp: i64,
        duration: u64,
    ) -> Result<bool>;
    fn end_track(&mut self, track: Track) -> Result<()>;
    fn finish(&mut self) -> Result<bool>;
}

pub struct Muxer<E: Engine> {
    engine: Option<E>,
    staging: Option<Staging>,
    config: Mp4Config,
    state: MuxState,
    last_timestamp: [Option<i64>; 2],
    ended: [bool; 2],
    max_packet_bytes: usize,
}
impl<E: Engine> Muxer<E> {
    pub fn create(
        path: impl AsRef<Path>,
        config: Mp4Config,
        max_packet_bytes: usize,
    ) -> Result<Self> {
        if config.audio.is_none() && config.video.is_none() {
            return Err(Error::invalid("at least one MP4 track is required"));
        }
        if max_packet_bytes == 0 || max_packet_bytes > 64 * 1024 * 1024 {
            return Err(Error::invalid("packet limit must be in 1..=64 MiB"));
        }
        if config
            .default_video_duration
            .is_some_and(|v| v == 0 || v > i64::MAX as u64 / 1000)
        {
            return Err(Error::invalid("invalid default video duration"));
        }
        if let Some(audio) = &config.audio
            && (audio.sample_rate == 0 || audio.channels == 0)
        {
            return Err(Error::invalid(
                "audio rate and channel count must be positive",
            ));
        }
        let staging = Staging::new(path.as_ref())?;
        let engine = E::open(&staging.partial, &config)?;
        Ok(Self {
            engine: Some(engine),
            staging: Some(staging),
            ended: [config.audio.is_none(), config.video.is_none()],
            config,
            state: MuxState::Writing,
            last_timestamp: [None; 2],
            max_packet_bytes,
        })
    }
    pub fn state(&self) -> MuxState {
        self.state
    }
    pub fn write(&mut self, track: Track, chunk: &EncodedChunk) -> Result<bool> {
        self.writing()?;
        let i = track.index();
        if self.ended[i] {
            return Err(Error::invalid("track is absent or already ended"));
        }
        if chunk.bytes().is_empty() || chunk.bytes().len() > self.max_packet_bytes {
            return Err(Error::invalid(
                "encoded packet is empty or exceeds the configured limit",
            ));
        }
        if self.last_timestamp[i].is_some_and(|last| chunk.timestamp() <= last) {
            return Err(Error::invalid("track timestamps must strictly increase"));
        }
        if (track == Track::Audio || self.last_timestamp[i].is_none())
            && chunk.kind() != EncodedChunkType::Key
        {
            return Err(Error::invalid(
                "audio packets and the first video packet must be key chunks",
            ));
        }
        let timestamp = chunk
            .timestamp()
            .checked_sub(self.config.timestamp_origin)
            .filter(|v| *v >= 0)
            .ok_or_else(|| Error::invalid("packet precedes the timeline origin or overflows"))?;
        let duration = chunk
            .duration()
            .or_else(|| match track {
                Track::Audio => self
                    .config
                    .audio
                    .as_ref()
                    .map(|c| 1024 * 1_000_000 / u64::from(c.sample_rate)),
                Track::Video => self.config.default_video_duration,
            })
            .filter(|v| *v > 0)
            .ok_or_else(|| Error::invalid("a positive packet duration is required"))?;
        if track == Track::Audio {
            let rate = u64::from(self.config.audio.as_ref().unwrap().sample_rate);
            let frames = 1024 * 1_000_000;
            if duration < frames / rate || duration > frames.div_ceil(rate) {
                return Err(Error::invalid(
                    "AAC duration must describe one 1024-frame access unit",
                ));
            }
        }
        if (timestamp as u64)
            .checked_add(duration)
            .is_none_or(|v| v > i64::MAX as u64 / 1000)
        {
            return Err(Error::invalid(
                "packet timing exceeds the portable container range",
            ));
        }
        let result = self
            .engine
            .as_mut()
            .unwrap()
            .write(track, chunk, timestamp, duration);
        match result {
            Ok(true) => {
                self.last_timestamp[i] = Some(chunk.timestamp());
                Ok(true)
            }
            Ok(false) => Ok(false),
            Err(e) => {
                self.fail();
                Err(e)
            }
        }
    }
    pub fn end_track(&mut self, track: Track) -> Result<()> {
        self.writing()?;
        let i = track.index();
        if self.ended[i] {
            return Ok(());
        }
        if self.last_timestamp[i].is_none() {
            return Err(Error::invalid(
                "each configured track must contain at least one packet",
            ));
        }
        if let Err(error) = self.engine.as_mut().unwrap().end_track(track) {
            self.fail();
            return Err(error);
        }
        self.ended[i] = true;
        Ok(())
    }
    /// Stops input on the first call. Retry until true; repeated success is
    /// harmless. Fatal I/O/framework failures discard the unfinished file.
    pub fn finish(&mut self) -> Result<bool> {
        if self.state == MuxState::Finished {
            return Ok(true);
        }
        if self.state == MuxState::Writing {
            // Validate both before ending either, so empty-track errors can be fixed.
            if (0..2).any(|i| !self.ended[i] && self.last_timestamp[i].is_none()) {
                return Err(Error::invalid(
                    "each configured track must contain at least one packet",
                ));
            }
            self.end_track(Track::Audio)?;
            self.end_track(Track::Video)?;
            self.state = MuxState::Finishing;
        }
        if self.state != MuxState::Finishing {
            return Err(Error::closed());
        }
        match self.engine.as_mut().unwrap().finish() {
            Ok(false) => Ok(false),
            Ok(true) => {
                self.engine = None;
                if let Err(error) = self.staging.as_ref().unwrap().publish() {
                    self.fail();
                    return Err(error);
                }
                self.staging = None;
                self.state = MuxState::Finished;
                Ok(true)
            }
            Err(error) => {
                self.fail();
                Err(error)
            }
        }
    }
    pub fn close(&mut self) {
        self.engine = None;
        self.staging = None;
        if self.state != MuxState::Finished {
            self.state = MuxState::Closed;
        }
    }
    fn fail(&mut self) {
        self.close();
        self.state = MuxState::Failed;
    }
    fn writing(&self) -> Result<()> {
        if self.state == MuxState::Writing {
            Ok(())
        } else {
            Err(Error::new(ErrorKind::InvalidState, "muxer is not writing"))
        }
    }
}
impl<E: Engine> Drop for Muxer<E> {
    fn drop(&mut self) {
        self.close();
    }
}

fn io(error: std::io::Error) -> Error {
    Error::new(ErrorKind::Io, format!("media file: {error}"))
}
struct Staging {
    target: PathBuf,
    directory: PathBuf,
    partial: PathBuf,
}
impl Staging {
    fn new(path: &Path) -> Result<Self> {
        if path.file_name().is_none() || path.to_str().is_none_or(|s| s.contains('\0')) {
            return Err(Error::invalid(
                "output must be a UTF-8 file path without NUL",
            ));
        }
        let target = std::env::current_dir().map_err(io)?.join(path);
        match fs::symlink_metadata(&target) {
            Ok(_) => {
                return Err(io(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "destination already exists",
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io(e)),
        }
        let parent = target.parent().unwrap();
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..100 {
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| Error::invalid("system time before epoch"))?
                .as_nanos();
            let directory =
                parent.join(format!(".xavi-{}-{stamp:x}-{serial:x}", std::process::id()));
            let builder = fs::DirBuilder::new();
            #[cfg(unix)]
            let builder = {
                use std::os::unix::fs::DirBuilderExt;
                let mut b = builder;
                b.mode(0o700);
                b
            };
            match builder.create(&directory) {
                Ok(()) => {
                    return Ok(Self {
                        target,
                        partial: directory.join("partial.mp4"),
                        directory,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io(e)),
            }
        }
        Err(Error::exhausted())
    }
    fn publish(&self) -> Result<()> {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.partial)
            .map_err(io)?
            .sync_all()
            .map_err(io)?;
        // Atomic no-clobber publication; both names are on the same filesystem.
        fs::hard_link(&self.partial, &self.target).map_err(io)
    }
}
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.partial);
        let _ = fs::remove_dir(&self.directory);
    }
}
