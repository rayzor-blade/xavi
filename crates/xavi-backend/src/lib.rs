//! CPU media operations shared by Ash, Rayzor and Caribou adapters.
//!
//! Like xgpu, resources live in Rust and adapters pass integer handles. This
//! layer has no VM types or guest callbacks: the generated ABI wraps it and
//! translates its errors, records and byte slices into the runtime's carriers.
//! The operations compile directly as a Rust library. [`install`] supplies the
//! small adapter module that translates x-idl's generated records and carriers.
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
    layouts: Slab<Vec<PlaneLayout>>,
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
                layouts: Slab::new(Kind::PlaneLayouts),
            }),
        }
    }

    pub fn create_audio(&self, descriptor: AudioDescriptor, data: &[u8]) -> Result<i32> {
        let audio = AudioData::new(descriptor, data)?;
        self.retain_audio(Arc::new(audio))
    }

    pub fn create_video(
        &self,
        descriptor: VideoDescriptor,
        data: &[u8],
        layout: Option<&[PlaneLayout]>,
    ) -> Result<i32> {
        let video = VideoFrame::new(descriptor, data, layout)?;
        self.retain_video(Arc::new(video))
    }

    pub fn create_audio_chunk(
        &self,
        kind: EncodedChunkType,
        timestamp: i64,
        duration: Option<u64>,
        data: &[u8],
    ) -> Result<i32> {
        let chunk = EncodedChunk::new(kind, timestamp, duration, data)?;
        self.retain_audio_chunk(Arc::new(chunk))
    }

    pub fn create_video_chunk(
        &self,
        kind: EncodedChunkType,
        timestamp: i64,
        duration: Option<u64>,
        data: &[u8],
    ) -> Result<i32> {
        let chunk = EncodedChunk::new(kind, timestamp, duration, data)?;
        self.retain_video_chunk(Arc::new(chunk))
    }

    /// Publish a retained or streamed resource without copying its media bytes.
    pub fn retain_audio(&self, value: Arc<AudioData>) -> Result<i32> {
        self.resources()?.audio.insert_shared(value)
    }

    pub fn retain_video(&self, value: Arc<VideoFrame>) -> Result<i32> {
        self.resources()?.video.insert_shared(value)
    }

    pub fn retain_audio_chunk(&self, value: Arc<EncodedChunk>) -> Result<i32> {
        self.resources()?.encoded_audio.insert_shared(value)
    }

    pub fn retain_video_chunk(&self, value: Arc<EncodedChunk>) -> Result<i32> {
        self.resources()?.encoded_video.insert_shared(value)
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

    /// Retain a native copy result until its recipient closes the collection.
    pub fn create_plane_layouts(&self, layouts: Vec<PlaneLayout>) -> Result<i32> {
        self.resources()?.layouts.insert(layouts)
    }

    pub fn plane_layouts(&self, handle: i32) -> Result<Arc<Vec<PlaneLayout>>> {
        self.resources()?
            .layouts
            .get(handle)
            .ok_or_else(Error::closed)
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
            Some(Kind::PlaneLayouts) => {
                resources.layouts.remove(handle);
            }
            None => {}
        }
        Ok(())
    }

    pub fn live_resources(&self) -> Result<usize> {
        let r = self.resources()?;
        Ok(r.audio.len()
            + r.video.len()
            + r.encoded_audio.len()
            + r.encoded_video.len()
            + r.layouts.len())
    }

    fn resources(&self) -> Result<MutexGuard<'_, Resources>> {
        self.resources
            .lock()
            .map_err(|_| Error::new(ErrorKind::InvalidState, "media resource lock is poisoned"))
    }
}

/// Installs the shared native adapter beneath `out`, for inclusion as
/// `mod backend` beside xavi-bindgen's generated model. The host provides
/// `with_media(|media: &MediaBackend| ...)`, selecting one stable backend per
/// runtime context, and the x-idl carriers at the model's crate root.
///
/// Calls, future completion and context teardown must run on the owning runtime
/// thread. Buffers must remain pinned and exclusively writable during copies.
/// No guest callback may run while a buffer is borrowed. CPU copies complete
/// before returning, including VideoFrame.copyTo's resolved future. Explicit
/// close is required; dropping generated wrappers does not release handles.
/// Workers should retain core Arcs and use bounded `stream` channels; guest
/// objects and future settlement stay on the runtime thread.
pub fn install(out: impl AsRef<std::path::Path>) -> std::io::Result<std::path::PathBuf> {
    let root = out.as_ref().join("xavi_backend");
    std::fs::create_dir_all(&root)?;
    let path = root.join("native.rs");
    std::fs::write(&path, include_str!("template/native.rs"))?;
    Ok(path)
}
