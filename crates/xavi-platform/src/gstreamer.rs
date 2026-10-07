//! System GStreamer 1.20+ loaded at runtime. Explicit factories avoid decodebin
//! autoplugging gst-libav: this backend never selects an FFmpeg element.
use super::{NativeConfig, Output};
use libloading::Library;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::{Arc, OnceLock};
use xavi_core::codec::Receive;
use xavi_core::{Error, ErrorKind, Result};
#[path = "mux/gstreamer.rs"]
pub(crate) mod mux;
#[path = "player/gstreamer.rs"]
pub(crate) mod player;
type Ptr = *mut c_void;
const PCM_FORMAT: &str = if cfg!(target_endian = "big") {
    "S16BE"
} else {
    "S16LE"
};
#[repr(C)]
struct GError {
    domain: u32,
    code: i32,
    message: *mut c_char,
}
// Public GStreamer 1.x C ABI prefixes. Only the published flags/timestamps are
// accessed; allocation, refcounting and memory access use framework functions.
#[repr(C)]
struct Mini {
    ty: usize,
    refs: i32,
    locks: i32,
    flags: u32,
    copy: Ptr,
    dispose: Ptr,
    free: Ptr,
    private_int: u32,
    private_ptr: Ptr,
}
#[repr(C)]
struct Buffer {
    mini: Mini,
    pool: Ptr,
    pts: u64,
    dts: u64,
    duration: u64,
    offset: u64,
    offset_end: u64,
}

macro_rules! api {
    ($($name:ident($($arg:ty),*) -> $ret:ty;)+) => {
        struct Api { _libraries: Vec<Library>, $( $name: unsafe extern "C" fn($($arg),*) -> $ret, )+ }
        impl Api {
            unsafe fn load() -> std::result::Result<Self, String> {
                let mut libraries = Vec::new();
                for name in ["libgstreamer-1.0.so.0", "libgstapp-1.0.so.0", "libglib-2.0.so.0", "libgobject-2.0.so.0"] { libraries.push(unsafe { Library::new(name) }.map_err(|e| e.to_string())?); }
                Ok(Self { $( $name: *libraries.iter().find_map(|lib| unsafe { lib.get::<unsafe extern "C" fn($($arg),*) -> $ret>(concat!(stringify!($name), "\0").as_bytes()) }.ok()).ok_or(concat!("missing GStreamer symbol ", stringify!($name)))?, )+ _libraries: libraries })
            }
        }
    }
}
api! {
    gst_init_check(*mut i32, *mut *mut *mut c_char, *mut *mut GError) -> i32;
    gst_parse_launch(*const c_char, *mut *mut GError) -> Ptr;
    gst_element_factory_find(*const c_char) -> Ptr;
    gst_bin_get_by_name(Ptr, *const c_char) -> Ptr;
    gst_element_set_state(Ptr, i32) -> i32;
    gst_element_get_state(Ptr, *mut i32, *mut i32, u64) -> i32;
    gst_element_get_bus(Ptr) -> Ptr;
    gst_bus_pop_filtered(Ptr, u32) -> Ptr;
    gst_message_parse_error(Ptr, *mut *mut GError, *mut *mut c_char) -> ();
    gst_object_unref(Ptr) -> ();
    gst_mini_object_unref(Ptr) -> ();
    gst_caps_from_string(*const c_char) -> Ptr;
    gst_caps_get_structure(Ptr, u32) -> Ptr;
    gst_structure_get_int(Ptr, *const c_char, *mut i32) -> i32;
    gst_structure_get_value(Ptr, *const c_char) -> Ptr;
    g_value_get_boxed(Ptr) -> Ptr;
    g_value_get_object(Ptr) -> Ptr;
    g_value_array_new(u32) -> Ptr;
    g_value_array_get_nth(Ptr, u32) -> Ptr;
    g_value_array_append(Ptr, Ptr) -> Ptr;
    gst_buffer_new_allocate(Ptr, usize, Ptr) -> *mut Buffer;
    gst_buffer_fill(*mut Buffer, usize, *const c_void, usize) -> usize;
    gst_buffer_extract(*mut Buffer, usize, *mut c_void, usize) -> usize;
    gst_buffer_get_size(*mut Buffer) -> usize;
    gst_app_src_set_caps(Ptr, Ptr) -> ();
    gst_app_src_get_current_level_buffers(Ptr) -> u64;
    gst_app_src_push_buffer(Ptr, *mut Buffer) -> i32;
    gst_app_src_end_of_stream(Ptr) -> i32;
    gst_app_sink_try_pull_sample(Ptr, u64) -> Ptr;
    gst_app_sink_try_pull_preroll(Ptr, u64) -> Ptr;
    gst_app_sink_is_eos(Ptr) -> i32;
    gst_sample_get_buffer(Ptr) -> *mut Buffer;
    gst_sample_get_caps(Ptr) -> Ptr;
    gst_element_factory_make(*const c_char, *const c_char) -> Ptr;
    gst_element_get_factory(Ptr) -> Ptr;
    gst_plugin_feature_get_plugin_name(Ptr) -> *const c_char;
    gst_object_get_name(Ptr) -> *mut c_char;
    gst_element_query_position(Ptr, i32, *mut i64) -> i32;
    gst_element_query_duration(Ptr, i32, *mut i64) -> i32;
    gst_element_seek_simple(Ptr, i32, u32, i64) -> i32;
    gst_filename_to_uri(*const c_char, *mut *mut GError) -> *mut c_char;
    gst_message_parse_buffering(Ptr, *mut i32) -> ();
    g_signal_connect_data(Ptr, *const c_char, Ptr, Ptr, Ptr, u32) -> usize;
    g_object_ref_sink(Ptr) -> Ptr;
    g_error_free(*mut GError) -> ();
    g_free(Ptr) -> ();
}
fn failure(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidState, message)
}
impl Api {
    fn get() -> Result<Arc<Self>> {
        static API: OnceLock<std::result::Result<Arc<Api>, String>> = OnceLock::new();
        API.get_or_init(|| unsafe {
            let api = Self::load()?;
            let mut error = std::ptr::null_mut();
            if (api.gst_init_check)(std::ptr::null_mut(), std::ptr::null_mut(), &mut error) == 0 {
                return Err(api.take_error(error));
            }
            Ok(Arc::new(api))
        })
        .clone()
        .map_err(|e| Error::unsupported(format!("system GStreamer 1.20+ unavailable: {e}")))
    }
    unsafe fn take_error(&self, error: *mut GError) -> String {
        if error.is_null() {
            return "GStreamer operation failed".into();
        }
        let message = unsafe { CStr::from_ptr((*error).message) }
            .to_string_lossy()
            .into_owned();
        unsafe { (self.g_error_free)(error) };
        message
    }
    fn factory(&self, names: &[&str]) -> Result<String> {
        for name in names {
            let text = CString::new(*name).unwrap();
            let ptr = unsafe { (self.gst_element_factory_find)(text.as_ptr()) };
            if !ptr.is_null() {
                unsafe { (self.gst_object_unref)(ptr) };
                return Ok((*name).into());
            }
        }
        Err(Error::unsupported(format!(
            "no system GStreamer factory from {names:?}; gst-libav is not a fallback"
        )))
    }
    unsafe fn bytes(&self, buffer: *mut Buffer) -> Result<Vec<u8>> {
        if buffer.is_null() {
            return Err(failure("GStreamer sample has no buffer"));
        }
        let len = unsafe { (self.gst_buffer_get_size)(buffer) };
        if len > crate::MAX_BYTES {
            return Err(Error::exhausted());
        }
        let mut bytes = vec![0; len];
        if unsafe { (self.gst_buffer_extract)(buffer, 0, bytes.as_mut_ptr().cast(), len) } != len {
            return Err(failure("GStreamer buffer extraction failed"));
        }
        Ok(bytes)
    }
}
pub(crate) struct Backend {
    api: Arc<Api>,
    pipeline: Ptr,
    source: Ptr,
    sink: Ptr,
    bus: Ptr,
    config: NativeConfig,
    description: Vec<u8>,
    origin: Option<i64>,
    eos: bool,
    input_format: Option<u32>,
}
impl Backend {
    pub fn open(config: NativeConfig, description: &[u8]) -> Result<Self> {
        let api = Api::get()?;
        let chain = match config.mode {
            1 => format!(
                "audioconvert ! {} bitrate={} ! aacparse ! audio/mpeg,mpegversion=4,stream-format=raw",
                api.factory(&["voaacenc"])?,
                config.bitrate
            ),
            2 => format!(
                "aacparse ! {} ! audioconvert ! audio/x-raw,format={PCM_FORMAT},layout=interleaved,rate={},channels={}",
                api.factory(&["faad", "fdkaacdec"])?,
                config.sample_rate,
                config.channels
            ),
            3 => format!(
                "videoconvert ! video/x-raw,format=I420 ! {} bitrate={} gop-size=1 ! h264parse ! video/x-h264,stream-format=avc,alignment=au,profile=baseline,level=(string)3",
                api.factory(&["openh264enc"])?,
                config.bitrate
            ),
            4 => format!(
                "h264parse ! {} ! videoconvert ! video/x-raw,format=BGRA",
                api.factory(&["vah264dec", "vaapih264dec", "openh264dec"])?
            ),
            _ => return Err(Error::invalid("invalid codec mode")),
        };
        let text=CString::new(format!("appsrc name=input format=time is-live=false block=false max-bytes={} max-buffers=2 ! {chain} ! appsink name=output sync=false max-buffers=2 drop=false", crate::MAX_BYTES)).unwrap();
        let mut error = std::ptr::null_mut();
        let pipeline = unsafe { (api.gst_parse_launch)(text.as_ptr(), &mut error) };
        if !error.is_null() || pipeline.is_null() {
            if !pipeline.is_null() {
                unsafe { (api.gst_object_unref)(pipeline) };
            }
            return Err(Error::unsupported(unsafe { api.take_error(error) }));
        }
        let source = unsafe { (api.gst_bin_get_by_name)(pipeline, c"input".as_ptr()) };
        let sink = unsafe { (api.gst_bin_get_by_name)(pipeline, c"output".as_ptr()) };
        let bus = unsafe { (api.gst_element_get_bus)(pipeline) };
        let mut backend = Self {
            api,
            pipeline,
            source,
            sink,
            bus,
            config,
            description: description.to_vec(),
            origin: None,
            eos: false,
            input_format: None,
        };
        if source.is_null() || sink.is_null() || bus.is_null() {
            return Err(failure("GStreamer pipeline is missing appsrc/appsink/bus"));
        }
        if backend.config.mode != 3 {
            backend.set_caps(0)?;
        }
        if unsafe { (backend.api.gst_element_set_state)(pipeline, 4) } == 0 {
            return Err(Error::unsupported("GStreamer pipeline could not start"));
        }
        Ok(backend)
    }
    fn set_caps(&mut self, format: u32) -> Result<()> {
        if self.config.mode == 3 && format == 3 && !self.config.width.is_multiple_of(4) {
            return Err(Error::unsupported(
                "GStreamer NV12 input requires four-byte row alignment",
            ));
        }
        let hex = self
            .description
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let c = &self.config;
        let caps = match c.mode {
            1 => format!(
                "audio/x-raw,format={PCM_FORMAT},layout=interleaved,rate={},channels={}",
                c.sample_rate, c.channels
            ),
            2 => format!(
                "audio/mpeg,mpegversion=4,stream-format=raw,framed=true,rate={},channels={},codec_data=(buffer){hex}",
                c.sample_rate, c.channels
            ),
            3 => format!(
                "video/x-raw,format={},width={},height={},framerate={}/1000",
                if format == 2 { "BGRA" } else { "NV12" },
                c.width,
                c.height,
                (c.framerate * 1000.0).round() as u32
            ),
            4 => format!("video/x-h264,stream-format=avc,alignment=au,codec_data=(buffer){hex}"),
            _ => unreachable!(),
        };
        let text = CString::new(caps).unwrap();
        let caps = unsafe { (self.api.gst_caps_from_string)(text.as_ptr()) };
        if caps.is_null() {
            return Err(Error::invalid("invalid GStreamer caps"));
        }
        unsafe {
            (self.api.gst_app_src_set_caps)(self.source, caps);
            (self.api.gst_mini_object_unref)(caps);
        }
        self.input_format = Some(format);
        Ok(())
    }
    fn check_error(&self) -> Result<()> {
        let message = unsafe { (self.api.gst_bus_pop_filtered)(self.bus, 2) };
        if message.is_null() {
            return Ok(());
        }
        let mut error = std::ptr::null_mut();
        let mut debug = std::ptr::null_mut();
        unsafe {
            (self.api.gst_message_parse_error)(message, &mut error, &mut debug);
            (self.api.g_free)(debug.cast());
            (self.api.gst_mini_object_unref)(message);
        }
        Err(failure(unsafe { self.api.take_error(error) }))
    }
    #[allow(clippy::too_many_arguments)]
    pub fn send(
        &mut self,
        bytes: &[u8],
        timestamp: i64,
        duration: u64,
        _: u32,
        format: u32,
        _: bool,
    ) -> Result<bool> {
        self.check_error()?;
        if self.eos {
            return Err(failure("input after EOS"));
        }
        if unsafe { (self.api.gst_app_src_get_current_level_buffers)(self.source) } >= 2 {
            return Ok(false);
        }
        if self.config.mode == 3 && self.input_format != Some(format) {
            self.set_caps(format)?;
        }
        let origin = *self.origin.get_or_insert(timestamp);
        let pts = u64::try_from((i128::from(timestamp) - i128::from(origin)) * 1000)
            .map_err(|_| Error::invalid("GStreamer timestamp is outside the segment range"))?;
        let duration = duration
            .checked_mul(1000)
            .ok_or_else(|| Error::invalid("duration overflow"))?;
        let b = unsafe {
            (self.api.gst_buffer_new_allocate)(
                std::ptr::null_mut(),
                bytes.len(),
                std::ptr::null_mut(),
            )
        };
        if b.is_null() {
            return Err(Error::exhausted());
        }
        unsafe {
            // New buffer is exclusively owned until push transfers it.
            (*b).pts = pts;
            (*b).dts = pts;
            (*b).duration = if duration == 0 { u64::MAX } else { duration };
            if (self.api.gst_buffer_fill)(b, 0, bytes.as_ptr().cast(), bytes.len()) != bytes.len() {
                (self.api.gst_mini_object_unref)(b.cast());
                return Err(failure("GStreamer buffer fill failed"));
            }
            if (self.api.gst_app_src_push_buffer)(self.source, b) != 0 {
                return Err(failure("GStreamer rejected input"));
            }
        }
        Ok(true)
    }
    pub fn receive(&mut self) -> Result<Receive<Output>> {
        self.check_error()?;
        let sample = unsafe { (self.api.gst_app_sink_try_pull_sample)(self.sink, 0) };
        if sample.is_null() {
            // is_eos also returns true before appsink starts. Do not mistake
            // asynchronous state transition/preroll for a drained pipeline.
            let mut state = 0;
            unsafe {
                (self.api.gst_element_get_state)(self.sink, &mut state, std::ptr::null_mut(), 0);
            }
            return Ok(
                if self.eos
                    && state >= 3
                    && unsafe { (self.api.gst_app_sink_is_eos)(self.sink) } != 0
                {
                    Receive::End
                } else {
                    Receive::Pending
                },
            );
        }
        let result = (|| unsafe {
            let buffer = (self.api.gst_sample_get_buffer)(sample);
            let bytes = self.api.bytes(buffer)?;
            let pts = (*buffer).pts;
            if pts == u64::MAX {
                return Err(failure("GStreamer output has no timestamp"));
            }
            let timestamp =
                i64::try_from(i128::from(self.origin.unwrap_or(0)) + i128::from(pts / 1000))
                    .map_err(|_| failure("timestamp overflow"))?;
            let duration = if (*buffer).duration == u64::MAX {
                0
            } else {
                (*buffer).duration / 1000
            };
            let caps = (self.api.gst_sample_get_caps)(sample);
            if caps.is_null() {
                return Err(failure("missing output caps"));
            }
            let structure = (self.api.gst_caps_get_structure)(caps, 0);
            if structure.is_null() {
                return Err(failure("missing output caps structure"));
            }
            let mut o = Output {
                bytes,
                timestamp,
                duration,
                key: (*buffer).mini.flags & (1 << 13) == 0,
                ..Default::default()
            };
            if self.config.mode == 2 {
                let mut rate = 0;
                let mut channels = 0;
                if (self.api.gst_structure_get_int)(structure, c"rate".as_ptr(), &mut rate) == 0
                    || (self.api.gst_structure_get_int)(
                        structure,
                        c"channels".as_ptr(),
                        &mut channels,
                    ) == 0
                    || rate <= 0
                    || channels <= 0
                {
                    return Err(failure("invalid PCM output caps"));
                }
                o.sample_rate = rate as u32;
                o.channels = channels as u32;
                o.frames = (o.bytes.len() / (o.channels as usize * 2)) as u32;
                o.format = 1;
            }
            if self.config.mode == 3 {
                let value = (self.api.gst_structure_get_value)(structure, c"codec_data".as_ptr());
                if !value.is_null() {
                    let desc = self.api.bytes((self.api.g_value_get_boxed)(value).cast())?;
                    if desc != self.description {
                        self.description = desc.clone();
                        o.description = desc;
                    }
                }
            }
            if self.config.mode == 4 {
                let mut w = 0;
                let mut h = 0;
                if (self.api.gst_structure_get_int)(structure, c"width".as_ptr(), &mut w) == 0
                    || (self.api.gst_structure_get_int)(structure, c"height".as_ptr(), &mut h) == 0
                    || w <= 0
                    || h <= 0
                    || u64::from(w as u32) * u64::from(h as u32) * 4 != o.bytes.len() as u64
                {
                    return Err(Error::unsupported(
                        "GStreamer output is not tightly packed BGRA",
                    ));
                }
                o.width = w as u32;
                o.height = h as u32;
                o.format = 2;
            }
            Ok(Receive::Output(o))
        })();
        unsafe { (self.api.gst_mini_object_unref)(sample) };
        result
    }
    pub fn drain(&mut self) -> Result<bool> {
        self.check_error()?;
        if !self.eos {
            if unsafe { (self.api.gst_app_src_end_of_stream)(self.source) } != 0 {
                return Err(failure("GStreamer EOS failed"));
            }
            self.eos = true;
        }
        Ok(true)
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        unsafe {
            (self.api.gst_element_set_state)(self.pipeline, 1);
            for ptr in [self.source, self.sink, self.bus, self.pipeline] {
                if !ptr.is_null() {
                    (self.api.gst_object_unref)(ptr);
                }
            }
        }
    }
}
