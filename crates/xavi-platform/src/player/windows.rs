//! Media Foundation Media Engine renders audio and clocks video. A private COM
//! worker owns all interfaces; callers receive immutable CPU BGRA snapshots.
use super::*;
use std::sync::{
    atomic::{AtomicU32, Ordering},
    mpsc,
};
use std::thread::JoinHandle;
use windows::Win32::{
    Foundation::{RECT, S_FALSE},
    Graphics::Imaging::*,
    Media::MediaFoundation::*,
    System::Com::*,
};
use windows::core::{BSTR, Interface, implement};
use xavi_core::{ErrorKind, PlaneLayout, VideoDescriptor, VideoPixelFormat};

fn failure(e: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Io, format!("Media Engine: {e}"))
}
type Job = Box<dyn FnOnce(&mut Backend) + Send>;
pub struct NativePlayer {
    jobs: Option<mpsc::SyncSender<Job>>,
    thread: Option<JoinHandle<()>>,
}
impl NativePlayer {
    pub fn open(path: &Path) -> Result<Self> {
        let path = path.canonicalize().map_err(failure)?;
        if !path.is_file() {
            return Err(Error::invalid("player input must be a local file"));
        }
        let (jobs, receive) = mpsc::sync_channel::<Job>(1);
        let (ready, result) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("xavi-media-engine".into())
            .spawn(move || match Backend::open(&path) {
                Ok(mut backend) => {
                    if ready.send(Ok(())).is_ok() {
                        while let Ok(job) = receive.recv() {
                            job(&mut backend);
                        }
                    }
                }
                Err(e) => {
                    let _ = ready.send(Err(e));
                }
            })
            .map_err(failure)?;
        let player = Self {
            jobs: Some(jobs),
            thread: Some(thread),
        };
        result.recv().map_err(failure)??;
        Ok(player)
    }
    fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Backend) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (send, recv) = mpsc::sync_channel(1);
        self.jobs
            .as_ref()
            .ok_or_else(Error::closed)?
            .send(Box::new(move |b| {
                let _ = send.send(operation(b));
            }))
            .map_err(failure)?;
        recv.recv().map_err(failure)?
    }
    pub fn info(&mut self) -> Result<PlaybackInfo> {
        self.call(Backend::info)
    }
    pub fn command(&mut self, command: i32, value: f64) -> Result<()> {
        self.call(move |b| b.command(command, value))
    }
    pub fn set_equalizer(&mut self, settings: EqualizerSettings) -> Result<()> {
        self.call(move |b| {
            b.check()?;
            b.equalizer.set(settings)
        })
    }
    pub fn frame(&mut self) -> Result<Option<Arc<VideoFrame>>> {
        self.call(Backend::frame)
    }
}
impl Drop for NativePlayer {
    fn drop(&mut self) {
        self.jobs.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Runtime;
impl Runtime {
    fn new() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(failure)?;
            if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                CoUninitialize();
                return Err(failure(e));
            }
        }
        Ok(Self)
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}
#[implement(IMFMediaEngineNotify)]
struct Notify(Arc<AtomicU32>);
impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, event: u32, param1: usize, _: u32) -> windows::core::Result<()> {
        if event == MF_MEDIA_ENGINE_EVENT_ERROR.0 as u32 {
            self.0.store((param1 as u32).max(1), Ordering::Release);
        }
        Ok(())
    }
}
struct Backend {
    equalizer: equalizer::Control,
    engine: IMFMediaEngine,
    imaging: IWICImagingFactory,
    bitmap: Option<IWICBitmap>,
    size: (u32, u32),
    failure: Arc<AtomicU32>,
    // COM objects drop before the worker's COM/MF initialization.
    _runtime: Runtime,
}
impl Backend {
    fn open(path: &Path) -> Result<Self> {
        unsafe {
            let runtime = Runtime::new()?;
            let factory: IMFMediaEngineClassFactory =
                CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)
                    .map_err(failure)?;
            let failure_state = Arc::new(AtomicU32::new(0));
            let notify: IMFMediaEngineNotify = Notify(failure_state.clone()).into();
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 2).map_err(failure)?;
            let attributes = attributes.ok_or_else(|| failure("missing attributes"))?;
            attributes
                .SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify)
                .map_err(failure)?;
            // DXGI_FORMAT_B8G8R8A8_UNORM. No HWND: frame-server mode into WIC.
            attributes
                .SetUINT32(&MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, 87)
                .map_err(failure)?;
            let imaging = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .map_err(failure)?;
            let engine = factory.CreateInstance(0, &attributes).map_err(failure)?;
            let equalizer = equalizer::Control::default();
            let effect = windows_equalizer::create(equalizer.clone());
            engine
                .cast::<IMFMediaEngineEx>()
                .map_err(failure)?
                .InsertAudioEffect(&effect, false)
                .map_err(failure)?;
            let b = Self {
                equalizer,
                engine,
                imaging,
                bitmap: None,
                size: (0, 0),
                failure: failure_state,
                _runtime: runtime,
            };
            b.engine.SetAutoPlay(false).map_err(failure)?;
            b.engine
                .SetPreload(MF_MEDIA_ENGINE_PRELOAD_AUTOMATIC)
                .map_err(failure)?;
            // Media Engine accepts a local absolute filename; preserve UTF-16.
            use std::os::windows::ffi::OsStrExt;
            let source: Vec<u16> = path.as_os_str().encode_wide().collect();
            b.engine
                .SetSource(&BSTR::from_wide(&source))
                .map_err(failure)?;
            b.engine.Load().map_err(failure)?;
            Ok(b)
        }
    }
    fn check(&self) -> Result<()> {
        self.equalizer.check()?;
        let code = self.failure.load(Ordering::Acquire);
        if code != 0 {
            Err(failure(format!("playback error {code}")))
        } else {
            Ok(())
        }
    }
    fn info(&mut self) -> Result<PlaybackInfo> {
        self.check()?;
        unsafe {
            let valid = |v: f64| if v.is_finite() { v.max(0.0) } else { 0.0 };
            Ok(PlaybackInfo {
                position: valid(self.engine.GetCurrentTime()),
                duration: valid(self.engine.GetDuration()),
                volume: self.engine.GetVolume(),
                state: if self.engine.GetReadyState() == 0 {
                    PlaybackState::Opening
                } else if self.engine.IsSeeking().as_bool() {
                    PlaybackState::Buffering
                } else if self.engine.IsEnded().as_bool() {
                    PlaybackState::Ended
                } else if self.engine.IsPaused().as_bool() {
                    PlaybackState::Paused
                } else if self.engine.GetReadyState() < 3 {
                    PlaybackState::Buffering
                } else {
                    PlaybackState::Playing
                },
            })
        }
    }
    fn command(&mut self, command: i32, value: f64) -> Result<()> {
        self.check()?;
        unsafe {
            match command {
                0 => self.engine.Play(),
                1 => self.engine.Pause(),
                2 => self.engine.SetCurrentTime(value),
                3 => self.engine.SetVolume(value),
                _ => return Err(Error::invalid("unknown playback command")),
            }
            .map_err(failure)?;
            if command == 2 {
                self.equalizer.reset();
            }
            Ok(())
        }
    }
    fn frame(&mut self) -> Result<Option<Arc<VideoFrame>>> {
        self.check()?;
        unsafe {
            if !self.engine.HasVideo().as_bool()
                || self.engine.IsSeeking().as_bool()
                || self.engine.GetReadyState() < 2
            {
                return Ok(None);
            }
            let mut pts = 0;
            // The generated Result wrapper erases S_FALSE (no new frame).
            let status = (self.engine.vtable().OnVideoStreamTick)(self.engine.as_raw(), &mut pts);
            if status == S_FALSE {
                return Ok(None);
            }
            status.ok().map_err(failure)?;
            let (mut w, mut h) = (0, 0);
            self.engine
                .GetNativeVideoSize(Some(&mut w), Some(&mut h))
                .map_err(failure)?;
            if w == 0 || h == 0 || u64::from(w) * u64::from(h) * 4 > crate::MAX_BYTES as u64 {
                return Err(Error::unsupported("video exceeds frame limit"));
            }
            if self.size != (w, h) {
                self.bitmap = Some(
                    self.imaging
                        .CreateBitmap(w, h, &GUID_WICPixelFormat32bppBGRA, WICBitmapCacheOnLoad)
                        .map_err(failure)?,
                );
                self.size = (w, h);
            }
            let bitmap = self.bitmap.as_ref().unwrap();
            let rect = RECT {
                left: 0,
                top: 0,
                right: w as i32,
                bottom: h as i32,
            };
            self.engine
                .TransferVideoFrame(bitmap, None, &rect, None)
                .map_err(failure)?;
            let lock = bitmap
                .Lock(
                    &WICRect {
                        X: 0,
                        Y: 0,
                        Width: w as i32,
                        Height: h as i32,
                    },
                    WICBitmapLockRead.0 as u32,
                )
                .map_err(failure)?;
            let (mut length, mut data) = (0, std::ptr::null_mut());
            lock.GetDataPointer(&mut length, &mut data)
                .map_err(failure)?;
            if data.is_null() || length as usize > crate::MAX_BYTES {
                return Err(failure("invalid WIC pixel buffer"));
            }
            let layout = [PlaneLayout {
                offset: 0,
                stride: lock.GetStride().map_err(failure)?,
            }];
            let frame = VideoFrame::new(
                VideoDescriptor::new(VideoPixelFormat::Bgra, w, h, pts / 10),
                std::slice::from_raw_parts(data, length as usize),
                Some(&layout),
            )?;
            Ok(Some(Arc::new(frame)))
        }
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        unsafe {
            let _ = self.engine.Shutdown();
        }
    }
}
