use super::{Api, Buffer, Mini, Ptr, failure};
use std::{
    ffi::{CString, c_char},
    path::Path,
    sync::Arc,
};
use xavi_core::mux::{Mp4Config, Track};
use xavi_core::{EncodedChunk, EncodedChunkType, Error, Result};

#[repr(C)]
struct MessagePrefix {
    mini: Mini,
    kind: u32,
}
pub(crate) struct Backend {
    api: Arc<Api>,
    pipeline: Ptr,
    source: [Ptr; 2],
    bus: Ptr,
    ended: [bool; 2],
    complete: bool,
}
impl Backend {
    pub fn open(path: &Path, config: &Mp4Config) -> Result<Self> {
        let api = Api::get()?;
        api.factory(&["mp4mux"])?;
        // Only parsers and mp4mux are used; no codec autoplugging or FFmpeg.
        let mut pipeline = String::from("mp4mux name=mux ! filesink name=file sync=false ");
        if config.audio.is_some() {
            pipeline.push_str("appsrc name=audio format=time block=false max-buffers=2 max-bytes=67108864 ! aacparse ! mux. ");
        }
        if config.video.is_some() {
            pipeline.push_str("appsrc name=video format=time block=false max-buffers=2 max-bytes=67108864 ! h264parse ! mux. ");
        }
        let text = CString::new(pipeline).unwrap();
        let mut error = std::ptr::null_mut();
        let pipeline = unsafe { (api.gst_parse_launch)(text.as_ptr(), &mut error) };
        if pipeline.is_null() || !error.is_null() {
            if !pipeline.is_null() {
                unsafe {
                    (api.gst_object_unref)(pipeline);
                }
            }
            return Err(Error::unsupported(unsafe { api.take_error(error) }));
        }
        let bus = unsafe { (api.gst_element_get_bus)(pipeline) };
        let mut result = Self {
            api,
            pipeline,
            bus,
            source: [std::ptr::null_mut(); 2],
            ended: [config.audio.is_none(), config.video.is_none()],
            complete: false,
        };
        if bus.is_null() {
            return Err(failure("missing MP4 pipeline bus"));
        }
        let filename = CString::new(
            path.to_str()
                .ok_or_else(|| Error::invalid("output path must be UTF-8"))?,
        )
        .map_err(|_| Error::invalid("NUL in output path"))?;
        // Set the path as a GObject string value, never as pipeline source text.
        type Set = unsafe extern "C" fn(Ptr, *const c_char, ...);
        let set = result
            .api
            ._libraries
            .iter()
            .find_map(|lib| unsafe { lib.get::<Set>(b"g_object_set\0") }.ok())
            .ok_or_else(|| Error::unsupported("missing g_object_set"))?;
        let file = unsafe { (result.api.gst_bin_get_by_name)(pipeline, c"file".as_ptr()) };
        if file.is_null() {
            return Err(failure("missing MP4 file sink"));
        }
        unsafe {
            set(
                file,
                c"location".as_ptr(),
                filename.as_ptr(),
                std::ptr::null::<c_char>(),
            );
            (result.api.gst_object_unref)(file);
        }
        if let Some(c) = &config.audio {
            result.source[0] =
                unsafe { (result.api.gst_bin_get_by_name)(pipeline, c"audio".as_ptr()) };
            result.caps(Track::Audio, &format!("audio/mpeg,mpegversion=4,stream-format=raw,framed=true,rate={},channels={},codec_data=(buffer){}", c.sample_rate, c.channels, hex(&c.description)))?;
        }
        if let Some(c) = &config.video {
            result.source[1] =
                unsafe { (result.api.gst_bin_get_by_name)(pipeline, c"video".as_ptr()) };
            result.caps(Track::Video, &format!("video/x-h264,stream-format=avc,alignment=au,width={},height={},codec_data=(buffer){}", c.coded_width.unwrap(), c.coded_height.unwrap(), hex(&c.description)))?;
        }
        if unsafe { (result.api.gst_element_set_state)(pipeline, 4) } == 0 {
            return Err(Error::unsupported("MP4 pipeline could not start"));
        }
        Ok(result)
    }
    fn caps(&self, track: Track, text: &str) -> Result<()> {
        let source = self.source[track.index()];
        if source.is_null() {
            return Err(failure("missing MP4 appsrc"));
        }
        let text = CString::new(text).unwrap();
        let caps = unsafe { (self.api.gst_caps_from_string)(text.as_ptr()) };
        if caps.is_null() {
            return Err(Error::invalid("invalid MP4 track caps"));
        }
        unsafe {
            (self.api.gst_app_src_set_caps)(source, caps);
            (self.api.gst_mini_object_unref)(caps);
        }
        Ok(())
    }
    fn poll_bus(&mut self) -> Result<()> {
        // Unlike the codec's error-only poll, retain EOS until it is observed.
        let message = unsafe { (self.api.gst_bus_pop_filtered)(self.bus, 3) };
        if message.is_null() {
            return Ok(());
        }
        let kind = unsafe { (*message.cast::<MessagePrefix>()).kind };
        let result = if kind == 2 {
            let mut error = std::ptr::null_mut();
            let mut debug = std::ptr::null_mut();
            unsafe {
                (self.api.gst_message_parse_error)(message, &mut error, &mut debug);
                (self.api.g_free)(debug.cast());
            }
            Err(failure(unsafe { self.api.take_error(error) }))
        } else {
            self.complete = true;
            Ok(())
        };
        unsafe {
            (self.api.gst_mini_object_unref)(message);
        }
        result
    }
    pub fn write(
        &mut self,
        track: Track,
        chunk: &EncodedChunk,
        timestamp: i64,
        duration: u64,
    ) -> Result<bool> {
        self.poll_bus()?;
        let source = self.source[track.index()];
        if unsafe { (self.api.gst_app_src_get_current_level_buffers)(source) } >= 2 {
            return Ok(false);
        }
        let data = chunk.bytes();
        let b: *mut Buffer = unsafe {
            (self.api.gst_buffer_new_allocate)(
                std::ptr::null_mut(),
                data.len(),
                std::ptr::null_mut(),
            )
        };
        if b.is_null() {
            return Err(Error::exhausted());
        }
        unsafe {
            (*b).pts = timestamp as u64 * 1000;
            (*b).dts = (*b).pts;
            (*b).duration = duration * 1000;
            if chunk.kind() == EncodedChunkType::Delta {
                (*b).mini.flags |= 1 << 13;
            }
            if (self.api.gst_buffer_fill)(b, 0, data.as_ptr().cast(), data.len()) != data.len() {
                (self.api.gst_mini_object_unref)(b.cast());
                return Err(failure("MP4 buffer fill failed"));
            }
            // push_buffer takes ownership, even on failure.
            if (self.api.gst_app_src_push_buffer)(source, b) != 0 {
                return Err(failure("MP4 pipeline rejected packet"));
            }
        }
        Ok(true)
    }
    pub fn end_track(&mut self, track: Track) -> Result<()> {
        self.poll_bus()?;
        let i = track.index();
        if !self.ended[i] {
            if unsafe { (self.api.gst_app_src_end_of_stream)(self.source[i]) } != 0 {
                return Err(failure("MP4 track EOS failed"));
            }
            self.ended[i] = true;
        }
        Ok(())
    }
    pub fn finish(&mut self) -> Result<bool> {
        self.poll_bus()?;
        if self.complete {
            unsafe {
                (self.api.gst_element_set_state)(self.pipeline, 1);
            }
        }
        Ok(self.complete)
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        unsafe {
            (self.api.gst_element_set_state)(self.pipeline, 1);
            for ptr in [self.source[0], self.source[1], self.bus, self.pipeline] {
                if !ptr.is_null() {
                    (self.api.gst_object_unref)(ptr);
                }
            }
        }
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|v| format!("{v:02x}")).collect()
}
