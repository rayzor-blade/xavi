//! CPU media operations shared by Ash, Rayzor and Caribou adapters.
//!
//! Like xgpu, resources live in Rust and adapters pass integer handles. This
//! layer has no VM types or guest callbacks: the generated ABI wraps it and
//! translates its errors, records and byte slices into the runtime's carriers.
//! Unlike xgpu's code that references generated types, these operations compile
//! directly as a Rust library; no source installation is necessary yet.
//!
//! Create one backend per adapter context, and never mix its handles with another
//! context's. Copies run without holding the table lock. An Arc acquired for
//! queued work survives close, while future lookups through that handle fail.
//! CPU VideoFrame copies complete synchronously; an adapter must still return
//! the IDL's future, resolved only after the destination writes finish.
//!
//! Codec engines, demuxing, device capture, browser agents and GPU surfaces are
//! not implemented by this initial data backend.
//!
//! [`stream::channel`] carries retained frames/chunks or incremental byte input
//! with bounded capacity and backpressure. Sending `backend.audio(handle)?`
//! retains that resource even after the caller releases the handle. Neither
//! the queue nor a future codec source needs a whole file or a seek operation.

use std::sync::{Arc, Mutex, MutexGuard};

use xavi_core::handles::{Kind, Slab};
pub use xavi_core::stream;
pub use xavi_core::{
    AudioCopyOptions, AudioData, AudioDescriptor, AudioSampleFormat, EncodedChunk,
    EncodedChunkType, Error, ErrorKind, PlaneLayout, Rect, Result, VideoColorSpace,
    VideoCopyOptions, VideoDescriptor, VideoFrame, VideoInfo, VideoPixelFormat,
};

struct Resources {
    audio: Slab<AudioData>,
    video: Slab<VideoFrame>,
    encoded_audio: Slab<EncodedChunk>,
    encoded_video: Slab<EncodedChunk>,
}

/// Thread-safe resource tables. No global state, runtime allocation or callbacks.
pub struct MediaBackend {
    resources: Mutex<Resources>,
}

impl Default for MediaBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MediaBackend {
    pub const fn new() -> Self {
        Self {
            resources: Mutex::new(Resources {
                audio: Slab::new(Kind::AudioData),
                video: Slab::new(Kind::VideoFrame),
                encoded_audio: Slab::new(Kind::EncodedAudioChunk),
                encoded_video: Slab::new(Kind::EncodedVideoChunk),
            }),
        }
    }

    pub fn create_audio(&self, descriptor: AudioDescriptor, data: &[u8]) -> Result<i32> {
        let audio = AudioData::new(descriptor, data)?;
        self.resources()?.audio.insert(audio)
    }

    pub fn create_video(
        &self,
        descriptor: VideoDescriptor,
        data: &[u8],
        layout: Option<&[PlaneLayout]>,
    ) -> Result<i32> {
        let video = VideoFrame::new(descriptor, data, layout)?;
        self.resources()?.video.insert(video)
    }

    pub fn create_audio_chunk(
        &self,
        kind: EncodedChunkType,
        timestamp: i64,
        duration: Option<u64>,
        data: &[u8],
    ) -> Result<i32> {
        let chunk = EncodedChunk::new(kind, timestamp, duration, data)?;
        self.resources()?.encoded_audio.insert(chunk)
    }

    pub fn create_video_chunk(
        &self,
        kind: EncodedChunkType,
        timestamp: i64,
        duration: Option<u64>,
        data: &[u8],
    ) -> Result<i32> {
        let chunk = EncodedChunk::new(kind, timestamp, duration, data)?;
        self.resources()?.encoded_video.insert(chunk)
    }

    pub fn audio(&self, handle: i32) -> Result<Arc<AudioData>> {
        self.resources()?
            .audio
            .get(handle)
            .ok_or_else(Error::closed)
    }

    pub fn video(&self, handle: i32) -> Result<Arc<VideoFrame>> {
        self.resources()?
            .video
            .get(handle)
            .ok_or_else(Error::closed)
    }

    pub fn audio_chunk(&self, handle: i32) -> Result<Arc<EncodedChunk>> {
        self.resources()?
            .encoded_audio
            .get(handle)
            .ok_or_else(Error::closed)
    }

    pub fn video_chunk(&self, handle: i32) -> Result<Arc<EncodedChunk>> {
        self.resources()?
            .encoded_video
            .get(handle)
            .ok_or_else(Error::closed)
    }

    /// A second independent handle retaining the same immutable resource.
    pub fn clone_audio(&self, handle: i32) -> Result<i32> {
        let mut resources = self.resources()?;
        let audio = resources.audio.get(handle).ok_or_else(Error::closed)?;
        resources.audio.insert_shared(audio)
    }

    pub fn clone_video(&self, handle: i32) -> Result<i32> {
        let mut resources = self.resources()?;
        let video = resources.video.get(handle).ok_or_else(Error::closed)?;
        resources.video.insert_shared(video)
    }

    pub fn audio_allocation_size(&self, handle: i32, options: AudioCopyOptions) -> Result<u32> {
        self.audio(handle)?.allocation_size(options)
    }

    pub fn copy_audio(
        &self,
        handle: i32,
        destination: &mut [u8],
        options: AudioCopyOptions,
    ) -> Result<()> {
        self.audio(handle)?.copy_to(destination, options)
    }

    pub fn video_allocation_size(&self, handle: i32, options: &VideoCopyOptions) -> Result<u32> {
        self.video(handle)?.allocation_size(options)
    }

    pub fn copy_video(
        &self,
        handle: i32,
        destination: &mut [u8],
        options: &VideoCopyOptions,
    ) -> Result<Vec<PlaneLayout>> {
        self.video(handle)?.copy_to(destination, options)
    }

    pub fn copy_audio_chunk(&self, handle: i32, destination: &mut [u8]) -> Result<()> {
        self.audio_chunk(handle)?.copy_to(destination)
    }

    pub fn copy_video_chunk(&self, handle: i32, destination: &mut [u8]) -> Result<()> {
        self.video_chunk(handle)?.copy_to(destination)
    }

    /// Drops this handle's reference. Finalizers and explicit close can both call
    /// this: zero, stale handles and repeated releases are harmless. Other
    /// clones and already acquired resources remain alive.
    pub fn release(&self, handle: i32) -> Result<()> {
        let mut resources = self.resources()?;
        match Kind::of(handle) {
            Some(Kind::AudioData) => {
                resources.audio.remove(handle);
            }
            Some(Kind::VideoFrame) => {
                resources.video.remove(handle);
            }
            Some(Kind::EncodedAudioChunk) => {
                resources.encoded_audio.remove(handle);
            }
            Some(Kind::EncodedVideoChunk) => {
                resources.encoded_video.remove(handle);
            }
            None => {}
        }
        Ok(())
    }

    pub fn live_resources(&self) -> Result<usize> {
        let r = self.resources()?;
        Ok(r.audio.len() + r.video.len() + r.encoded_audio.len() + r.encoded_video.len())
    }

    fn resources(&self) -> Result<MutexGuard<'_, Resources>> {
        self.resources
            .lock()
            .map_err(|_| Error::new(ErrorKind::InvalidState, "media resource lock is poisoned"))
    }
}
