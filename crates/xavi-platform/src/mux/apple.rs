use crate::native::NativeConfig;
use std::{
    ffi::{CStr, CString, c_char, c_void},
    path::Path,
    ptr::NonNull,
};
use xavi_core::mux::{Mp4Config, Track};
use xavi_core::{EncodedChunk, EncodedChunkType, Error, ErrorKind, Result};
unsafe extern "C" {
    fn xavi_mux_open(
        path: *const c_char,
        audio: *const NativeConfig,
        video: *const NativeConfig,
        error: *mut c_char,
    ) -> *mut c_void;
    fn xavi_mux_write(
        ctx: *mut c_void,
        track: i32,
        data: *const u8,
        len: usize,
        timestamp: i64,
        duration: u64,
        key: bool,
        error: *mut c_char,
    ) -> i32;
    fn xavi_mux_end(ctx: *mut c_void, track: i32, error: *mut c_char) -> i32;
    fn xavi_mux_finish(ctx: *mut c_void, error: *mut c_char) -> i32;
    fn xavi_mux_drop(ctx: *mut c_void);
}
fn failure(bytes: &[c_char; 512]) -> Error {
    // The shim always initializes and terminates this fixed-size buffer.
    let message = unsafe { CStr::from_ptr(bytes.as_ptr()) }.to_string_lossy();
    Error::new(ErrorKind::Io, format!("AVAssetWriter: {message}"))
}
pub(crate) struct Backend(NonNull<c_void>);
impl Backend {
    pub fn open(path: &Path, config: &Mp4Config) -> Result<Self> {
        let path = CString::new(
            path.to_str()
                .ok_or_else(|| Error::invalid("path must be UTF-8"))?,
        )
        .map_err(|_| Error::invalid("NUL in output path"))?;
        let audio = config.audio.as_ref().map(|c| NativeConfig {
            sample_rate: c.sample_rate,
            channels: c.channels,
            description: c.description.as_ptr(),
            description_len: c.description.len(),
            ..Default::default()
        });
        let video = config.video.as_ref().map(|c| NativeConfig {
            width: c.coded_width.unwrap(),
            height: c.coded_height.unwrap(),
            description: c.description.as_ptr(),
            description_len: c.description.len(),
            ..Default::default()
        });
        let mut error = [0; 512];
        let ptr = unsafe {
            xavi_mux_open(
                path.as_ptr(),
                audio.as_ref().map_or(std::ptr::null(), |v| v),
                video.as_ref().map_or(std::ptr::null(), |v| v),
                error.as_mut_ptr(),
            )
        };
        NonNull::new(ptr).map(Self).ok_or_else(|| failure(&error))
    }
    pub fn write(
        &mut self,
        track: Track,
        chunk: &EncodedChunk,
        timestamp: i64,
        duration: u64,
    ) -> Result<bool> {
        let mut error = [0; 512];
        match unsafe {
            xavi_mux_write(
                self.0.as_ptr(),
                track.index() as i32,
                chunk.bytes().as_ptr(),
                chunk.bytes().len(),
                timestamp,
                duration,
                chunk.kind() == EncodedChunkType::Key,
                error.as_mut_ptr(),
            )
        } {
            0 => Ok(true),
            1 => Ok(false),
            _ => Err(failure(&error)),
        }
    }
    pub fn end_track(&mut self, track: Track) -> Result<()> {
        let mut error = [0; 512];
        if unsafe { xavi_mux_end(self.0.as_ptr(), track.index() as i32, error.as_mut_ptr()) } == 0 {
            Ok(())
        } else {
            Err(failure(&error))
        }
    }
    pub fn finish(&mut self) -> Result<bool> {
        let mut error = [0; 512];
        match unsafe { xavi_mux_finish(self.0.as_ptr(), error.as_mut_ptr()) } {
            0 => Ok(true),
            1 => Ok(false),
            _ => Err(failure(&error)),
        }
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        unsafe { xavi_mux_drop(self.0.as_ptr()) }
    }
}
