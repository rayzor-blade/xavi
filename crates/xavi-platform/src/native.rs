#[cfg(target_vendor = "apple")]
use std::{ffi::c_void, ptr::NonNull};
#[cfg(target_vendor = "apple")]
use xavi_core::ErrorKind;
#[cfg(not(any(target_os = "android", target_os = "windows", target_os = "linux")))]
use xavi_core::{Error, Result, codec::Receive};

#[repr(C)]
#[derive(Default)]
pub(crate) struct NativeConfig {
    pub mode: i32,
    pub sample_rate: u32,
    pub channels: u32,
    pub width: u32,
    pub height: u32,
    pub bitrate: u64,
    pub framerate: f64,
    pub description: *const u8,
    pub description_len: usize,
}
#[cfg(target_vendor = "apple")]
#[repr(C)]
struct NativeOutput {
    data: *mut u8,
    len: usize,
    description: *mut u8,
    description_len: usize,
    timestamp: i64,
    duration: u64,
    format: u32,
    frames: u32,
    sample_rate: u32,
    channels: u32,
    width: u32,
    height: u32,
    key: i32,
}
#[derive(Default)]
pub(crate) struct Output {
    pub bytes: Vec<u8>,
    pub description: Vec<u8>,
    pub timestamp: i64,
    pub duration: u64,
    pub format: u32,
    pub frames: u32,
    pub sample_rate: u32,
    pub channels: u32,
    pub width: u32,
    pub height: u32,
    pub key: bool,
}

#[cfg(target_vendor = "apple")]
mod sys {
    use super::*;
    unsafe extern "C" {
        pub(super) fn xavi_codec_create(
            config: *const NativeConfig,
            status: *mut i32,
        ) -> *mut c_void;
        pub(super) fn xavi_codec_destroy(codec: *mut c_void);
        pub(super) fn xavi_codec_send(
            codec: *mut c_void,
            data: *const u8,
            len: usize,
            timestamp: i64,
            duration: u64,
            frames: u32,
            format: u32,
            key: i32,
        ) -> i32;
        pub(super) fn xavi_codec_receive(codec: *mut c_void, output: *mut *mut NativeOutput)
        -> i32;
        pub(super) fn xavi_codec_drain(codec: *mut c_void) -> i32;
        pub(super) fn xavi_output_free(output: *mut NativeOutput);
    }
}
#[cfg(target_vendor = "apple")]
fn error(status: i32) -> Error {
    let kind = match status {
        -1 => ErrorKind::NotSupported,
        -2 => ErrorKind::InvalidArgument,
        -3 => ErrorKind::ResourceExhausted,
        _ => ErrorKind::InvalidState,
    };
    Error::new(kind, format!("platform codec returned status {status}"))
}

/// Intentionally !Send: some platform codecs are bound to their creating thread.
#[cfg(target_vendor = "apple")]
pub(crate) struct Native(NonNull<c_void>);
#[cfg(target_vendor = "apple")]
impl Native {
    pub fn open(mut config: NativeConfig, description: &[u8]) -> Result<Self> {
        config.description = description.as_ptr();
        config.description_len = description.len();
        let mut status = 0;
        // SAFETY: the framework copies configuration before this call returns.
        let handle = unsafe { sys::xavi_codec_create(&config, &mut status) };
        NonNull::new(handle).map(Self).ok_or_else(|| error(status))
    }
    #[allow(clippy::too_many_arguments)]
    pub fn send(
        &mut self,
        bytes: &[u8],
        timestamp: i64,
        duration: u64,
        frames: u32,
        format: u32,
        key: bool,
    ) -> Result<bool> {
        // SAFETY: bytes are borrowed only for this synchronous call; retained
        // data belongs to the native context. The context is exclusively held.
        match unsafe {
            sys::xavi_codec_send(
                self.0.as_ptr(),
                bytes.as_ptr(),
                bytes.len(),
                timestamp,
                duration,
                frames,
                format,
                i32::from(key),
            )
        } {
            0 => Ok(true),
            1 => Ok(false),
            s => Err(error(s)),
        }
    }
    pub fn drain(&mut self) -> Result<bool> {
        match unsafe { sys::xavi_codec_drain(self.0.as_ptr()) } {
            0 => Ok(true),
            1 => Ok(false),
            s => Err(error(s)),
        }
    }
    pub fn receive(&mut self) -> Result<Receive<Output>> {
        let mut ptr = std::ptr::null_mut();
        match unsafe { sys::xavi_codec_receive(self.0.as_ptr(), &mut ptr) } {
            1 => return Ok(Receive::Pending),
            2 => return Ok(Receive::End),
            0 => {}
            s => return Err(error(s)),
        }
        let ptr = NonNull::new(ptr).ok_or_else(|| error(-4))?;
        struct Owned(NonNull<NativeOutput>);
        impl Drop for Owned {
            fn drop(&mut self) {
                unsafe { sys::xavi_output_free(self.0.as_ptr()) }
            }
        }
        let owned = Owned(ptr);
        // SAFETY: the successful receive transfers an initialized output which
        // stays alive until `owned` drops. Lengths are checked before borrowing.
        let o = unsafe { owned.0.as_ref() };
        unsafe fn copy(ptr: *const u8, len: usize) -> Result<Vec<u8>> {
            if len == 0 {
                return Ok(Vec::new());
            }
            if ptr.is_null() || len > super::MAX_BYTES {
                return Err(error(-4));
            }
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(len)
                .map_err(|_| Error::exhausted())?;
            bytes.extend_from_slice(unsafe { std::slice::from_raw_parts(ptr, len) });
            Ok(bytes)
        }
        Ok(Receive::Output(Output {
            bytes: unsafe { copy(o.data, o.len)? },
            description: unsafe { copy(o.description, o.description_len)? },
            timestamp: o.timestamp,
            duration: o.duration,
            format: o.format,
            frames: o.frames,
            sample_rate: o.sample_rate,
            channels: o.channels,
            width: o.width,
            height: o.height,
            key: o.key != 0,
        }))
    }
}
#[cfg(target_vendor = "apple")]
impl Drop for Native {
    fn drop(&mut self) {
        unsafe { sys::xavi_codec_destroy(self.0.as_ptr()) }
    }
}

#[cfg(target_os = "android")]
#[path = "android.rs"]
mod port;
#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod port;
#[cfg(target_os = "linux")]
#[path = "gstreamer.rs"]
mod port;
#[cfg(any(target_os = "android", target_os = "windows", target_os = "linux"))]
pub(crate) use port::Backend as Native;

#[cfg(not(any(
    target_vendor = "apple",
    target_os = "android",
    target_os = "windows",
    target_os = "linux"
)))]
pub(crate) struct Native;
#[cfg(not(any(
    target_vendor = "apple",
    target_os = "android",
    target_os = "windows",
    target_os = "linux"
)))]
impl Native {
    pub fn open(_: NativeConfig, _: &[u8]) -> Result<Self> {
        Err(Error::unsupported("no OS codec framework on this target"))
    }
    #[allow(clippy::too_many_arguments)]
    pub fn send(&mut self, _: &[u8], _: i64, _: u64, _: u32, _: u32, _: bool) -> Result<bool> {
        unreachable!()
    }
    pub fn receive(&mut self) -> Result<Receive<Output>> {
        unreachable!()
    }
    pub fn drain(&mut self) -> Result<bool> {
        unreachable!()
    }
}
