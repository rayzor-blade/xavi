use super::*;
use std::ffi::{CStr, CString, c_char, c_void};
use std::ptr::NonNull;
use xavi_core::{ErrorKind, PlaneLayout, VideoDescriptor, VideoPixelFormat};

#[repr(C)]
#[derive(Default)]
struct Info {
    state: i32,
    position: f64,
    duration: f64,
    volume: f64,
}
#[repr(C)]
#[derive(Default)]
struct Frame {
    buffer: *mut c_void,
    data: *const u8,
    len: usize,
    width: u32,
    height: u32,
    stride: u32,
    timestamp: i64,
}
unsafe extern "C" {
    fn xavi_player_main_thread() -> i32;
    fn xavi_player_open(
        path: *const c_char,
        control: *const c_void,
        error: *mut c_char,
    ) -> *mut c_void;
    fn xavi_player_command(ctx: *mut c_void, command: i32, value: f64, error: *mut c_char) -> i32;
    fn xavi_player_info(ctx: *mut c_void, out: *mut Info, error: *mut c_char) -> i32;
    fn xavi_player_frame(ctx: *mut c_void, out: *mut Frame, error: *mut c_char) -> i32;
    fn xavi_player_frame_release(frame: *mut Frame);
    fn xavi_player_drop(ctx: *mut c_void);
}
pub struct NativePlayer(NonNull<c_void>, equalizer::Control);
// Every entry point checks the main thread before touching AVPlayer. Drop
// dispatches there when necessary. Moving this owner does not move native work.
unsafe impl Send for NativePlayer {}
impl Drop for NativePlayer {
    fn drop(&mut self) {
        unsafe { xavi_player_drop(self.0.as_ptr()) };
    }
}
impl Drop for Frame {
    fn drop(&mut self) {
        unsafe { xavi_player_frame_release(self) };
    }
}
fn error(status: i32, message: &[c_char; 512]) -> Error {
    let message = unsafe { CStr::from_ptr(message.as_ptr()) }.to_string_lossy();
    Error::new(
        if status == -2 {
            ErrorKind::InvalidState
        } else {
            ErrorKind::Io
        },
        message.as_ref(),
    )
}
impl NativePlayer {
    pub fn open(path: &Path) -> Result<Self> {
        if unsafe { xavi_player_main_thread() } == 0 {
            return Err(Error::new(
                ErrorKind::InvalidState,
                "media playback must be driven on the main thread",
            ));
        }
        let path = path
            .canonicalize()
            .map_err(|e| Error::new(ErrorKind::Io, e.to_string()))?;
        if !path.is_file() {
            return Err(Error::invalid("player input must be a local file"));
        }
        let path = CString::new(
            path.to_str()
                .ok_or_else(|| Error::invalid("path must be UTF-8"))?,
        )
        .map_err(|_| Error::invalid("path contains NUL"))?;
        let mut message = [0; 512];
        let control = equalizer::Control::default();
        let ptr = unsafe {
            xavi_player_open(
                path.as_ptr(),
                (&control as *const equalizer::Control).cast(),
                message.as_mut_ptr(),
            )
        };
        NonNull::new(ptr)
            .map(|ptr| Self(ptr, control))
            .ok_or_else(|| error(-1, &message))
    }
    pub fn command(&mut self, command: i32, value: f64) -> Result<()> {
        self.1.check()?;
        let mut message = [0; 512];
        let status =
            unsafe { xavi_player_command(self.0.as_ptr(), command, value, message.as_mut_ptr()) };
        if status < 0 {
            Err(error(status, &message))
        } else {
            if command == 2 {
                self.1.reset();
            }
            Ok(())
        }
    }
    pub fn info(&mut self) -> Result<PlaybackInfo> {
        self.1.check()?;
        let mut info = Info::default();
        let mut message = [0; 512];
        let status = unsafe { xavi_player_info(self.0.as_ptr(), &mut info, message.as_mut_ptr()) };
        if status < 0 {
            return Err(error(status, &message));
        }
        let state = match info.state {
            0 => PlaybackState::Opening,
            1 => PlaybackState::Paused,
            2 => PlaybackState::Playing,
            3 => PlaybackState::Buffering,
            4 => PlaybackState::Ended,
            _ => return Err(Error::invalid("invalid native playback state")),
        };
        Ok(PlaybackInfo {
            state,
            position: info.position,
            duration: info.duration,
            volume: info.volume,
        })
    }
    pub fn frame(&mut self) -> Result<Option<Arc<VideoFrame>>> {
        self.1.check()?;
        let mut frame = Frame::default();
        let mut message = [0; 512];
        let status =
            unsafe { xavi_player_frame(self.0.as_ptr(), &mut frame, message.as_mut_ptr()) };
        if status < 0 {
            return Err(error(status, &message));
        }
        if status == 0 {
            return Ok(None);
        }
        if frame.data.is_null() || frame.len > 64 * 1024 * 1024 {
            return Err(Error::invalid("invalid decoded frame buffer"));
        }
        // The native buffer remains retained and locked until Frame drops.
        let data = unsafe { std::slice::from_raw_parts(frame.data, frame.len) };
        let descriptor = VideoDescriptor::new(
            VideoPixelFormat::Bgra,
            frame.width,
            frame.height,
            frame.timestamp,
        );
        let layout = [PlaneLayout {
            offset: 0,
            stride: frame.stride,
        }];
        Ok(Some(Arc::new(VideoFrame::new(
            descriptor,
            data,
            Some(&layout),
        )?)))
    }
    pub fn set_equalizer(&mut self, settings: EqualizerSettings) -> Result<()> {
        self.info()?; // Enforce the player's main-thread affinity.
        self.1.set(settings)
    }
}
