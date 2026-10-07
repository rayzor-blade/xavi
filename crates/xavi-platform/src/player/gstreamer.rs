//! Clocked system playback. Autoplugging explicitly rejects gst-libav on each
//! decodebin, without changing the process-wide plugin registry or ranks.
use super::{Api, Buffer, MapInfo, Mini, Ptr, failure};
use crate::player::equalizer::{Control, Stream};
use crate::player::{PlaybackInfo, PlaybackState};
use std::sync::Mutex;
use std::{
    ffi::{CStr, CString, c_char},
    path::Path,
    sync::Arc,
};
use xavi_core::equalizer::Settings;
use xavi_core::{Error, ErrorKind, Result, VideoDescriptor, VideoFrame, VideoPixelFormat};

type Set = unsafe extern "C" fn(Ptr, *const c_char, ...);
#[repr(C)]
struct Message {
    mini: Mini,
    kind: u32,
}

pub struct NativePlayer {
    equalizer: Control,
    api: Arc<Api>,
    set: Set,
    pipeline: Ptr,
    sink: Ptr,
    bus: Ptr,
    playing: bool,
    seeking: bool,
    ended: bool,
    buffering: bool,
    volume: f64,
    failure: Option<String>,
    last_pts: Option<u64>,
}
// GStreamer objects are thread-safe; this owner is only accessed through &mut
// or the backend Mutex. Callbacks use the immutable, process-lifetime Api.
unsafe impl Send for NativePlayer {}

struct AudioTap {
    api: Arc<Api>,
    stream: Mutex<Stream>,
    control: Control,
}
unsafe extern "C" fn audio_tap_drop(data: Ptr, _: Ptr) {
    drop(unsafe { Box::from_raw(data.cast::<AudioTap>()) });
}
unsafe extern "C" fn audio_tap(identity: Ptr, buffer: *mut Buffer, data: Ptr) {
    let tap = unsafe { &*data.cast::<AudioTap>() };
    let a = &tap.api;
    let result = (|| {
        let mut stream = tap
            .stream
            .try_lock()
            .map_err(|_| failure("concurrent audio callback"))?;
        unsafe {
            let pad = (a.gst_element_get_static_pad)(identity, c"sink".as_ptr());
            if pad.is_null() {
                return Err(failure("missing audio pad"));
            }
            let caps = (a.gst_pad_get_current_caps)(pad);
            (a.gst_object_unref)(pad);
            if caps.is_null() {
                return Err(failure("missing audio caps"));
            }
            let structure = (a.gst_caps_get_structure)(caps, 0);
            let (mut rate, mut channels) = (0, 0);
            if !structure.is_null() {
                (a.gst_structure_get_int)(structure, c"rate".as_ptr(), &mut rate);
                (a.gst_structure_get_int)(structure, c"channels".as_ptr(), &mut channels);
            }
            (a.gst_mini_object_unref)(caps);
            let mut map = MapInfo::default();
            // Identity is an in-place GstBaseTransform, which supplies writable
            // buffers to transform_ip/handoff (it does not enable passthrough).
            if buffer.is_null() || (a.gst_buffer_map)(buffer, &mut map, 3) == 0 {
                return Err(failure("audio buffer is not writable"));
            }
            let result = if map.size == 0 {
                Ok(())
            } else if map.data.is_null() || map.size > crate::MAX_BYTES {
                Err(failure("invalid PCM buffer"))
            } else {
                let bytes = std::slice::from_raw_parts_mut(map.data, map.size);
                let result = stream.interleaved(
                    bytes,
                    rate as f64,
                    channels as usize,
                    true,
                    (*buffer).mini.flags & 64 != 0,
                );
                if result.is_err() {
                    bytes.fill(0);
                }
                result
            };
            (a.gst_buffer_unmap)(buffer, &mut map);
            result
        }
    })();
    if result.is_err() {
        tap.control.fail();
    }
}

unsafe extern "C" fn select(_: Ptr, _: Ptr, _: Ptr, factory: Ptr, data: Ptr) -> i32 {
    let api = unsafe { &*data.cast::<Api>() };
    let plugin = unsafe { (api.gst_plugin_feature_get_plugin_name)(factory) };
    if !plugin.is_null() {
        let name = unsafe { CStr::from_ptr(plugin) }.to_bytes();
        if name == b"libav" || name == b"ffmpeg" {
            return 2;
        } // SKIP
    }
    0 // TRY
}
unsafe extern "C" fn element_setup(_: Ptr, element: Ptr, data: Ptr) {
    let api = unsafe { &*data.cast::<Api>() };
    unsafe {
        let factory = (api.gst_element_get_factory)(element);
        if factory.is_null() {
            return;
        }
        let name = (api.gst_object_get_name)(factory);
        if name.is_null() {
            return;
        }
        let is_decoder = matches!(
            CStr::from_ptr(name).to_bytes(),
            b"decodebin" | b"uridecodebin"
        );
        (api.g_free)(name.cast());
        if is_decoder {
            (api.g_signal_connect_data)(
                element,
                c"autoplug-sort".as_ptr(),
                sort as *const () as Ptr,
                data,
                std::ptr::null_mut(),
                0,
            );
            (api.g_signal_connect_data)(
                element,
                c"autoplug-select".as_ptr(),
                select as *const () as Ptr,
                data,
                std::ptr::null_mut(),
                0,
            );
        }
    }
}
// Filter before parser caps negotiation as well as before decoder creation.
// Otherwise an excluded avdec_h264 can make h264parse negotiate AVC while the
// remaining OpenH264 decoder needs Annex B, silently losing the video track.
unsafe extern "C" fn sort(_: Ptr, _: Ptr, _: Ptr, factories: Ptr, data: Ptr) -> Ptr {
    let api = unsafe { &*data.cast::<Api>() };
    unsafe {
        // GValueArray's first public field is guint n_values; GLib handles all
        // allocation, element access, reference copying and eventual freeing.
        let count = *factories.cast::<u32>();
        let filtered = (api.g_value_array_new)(count);
        for i in 0..count {
            let value = (api.g_value_array_get_nth)(factories, i);
            let factory = (api.g_value_get_object)(value);
            if select(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                factory,
                data,
            ) == 0
            {
                (api.g_value_array_append)(filtered, value);
            }
        }
        filtered
    }
}
impl NativePlayer {
    pub fn open(path: &Path) -> Result<Self> {
        let path = path
            .canonicalize()
            .map_err(|e| Error::new(ErrorKind::Io, e.to_string()))?;
        if !path.is_file() {
            return Err(Error::invalid("player input must be a local file"));
        }
        let filename = CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| Error::invalid("NUL in path"))?;
        let api = Api::get()?;
        let set = *api
            ._libraries
            .iter()
            .find_map(|l| unsafe { l.get::<Set>(b"g_object_set\0") }.ok())
            .ok_or_else(|| Error::unsupported("missing g_object_set"))?;
        unsafe {
            let pipeline = (api.gst_element_factory_make)(c"playbin".as_ptr(), std::ptr::null());
            if pipeline.is_null() {
                return Err(Error::unsupported("system GStreamer playbin is missing"));
            }
            (api.g_object_ref_sink)(pipeline);
            let sink = (api.gst_element_factory_make)(c"appsink".as_ptr(), std::ptr::null());
            if !sink.is_null() {
                (api.g_object_ref_sink)(sink);
            }
            let bus = (api.gst_element_get_bus)(pipeline);
            let player = Self {
                equalizer: Control::default(),
                api,
                set,
                pipeline,
                sink,
                bus,
                playing: false,
                seeking: false,
                ended: false,
                buffering: false,
                volume: 1.0,
                failure: None,
                last_pts: None,
            };
            if sink.is_null() || bus.is_null() {
                return Err(Error::unsupported("system GStreamer appsink/bus missing"));
            }
            let a = &player.api;
            let format = if cfg!(target_endian = "big") {
                "F32BE"
            } else {
                "F32LE"
            };
            let chain = CString::new(format!("audioconvert ! audio/x-raw,format={format},layout=interleaved ! identity name=xavi_equalizer signal-handoffs=true ! audioconvert")).unwrap();
            let mut error = std::ptr::null_mut();
            let filter = (a.gst_parse_bin_from_description)(chain.as_ptr(), 1, &mut error);
            if !error.is_null() || filter.is_null() {
                if !filter.is_null() {
                    (a.gst_object_unref)(filter);
                }
                return Err(Error::unsupported(a.take_error(error)));
            }
            (a.g_object_ref_sink)(filter);
            let identity = (a.gst_bin_get_by_name)(filter, c"xavi_equalizer".as_ptr());
            if identity.is_null() {
                (a.gst_object_unref)(filter);
                return Err(failure("missing equalizer identity element"));
            }
            let control = player.equalizer.clone();
            let tap = Box::new(AudioTap {
                api: a.clone(),
                stream: Mutex::new(Stream::new(control.clone())),
                control,
            });
            (a.g_signal_connect_data)(
                identity,
                c"handoff".as_ptr(),
                audio_tap as *const () as Ptr,
                Box::into_raw(tap).cast(),
                audio_tap_drop as *const () as Ptr,
                0,
            );
            set(
                pipeline,
                c"audio-filter".as_ptr(),
                filter,
                std::ptr::null::<c_char>(),
            );
            (a.gst_object_unref)(identity);
            (a.gst_object_unref)(filter);
            let mut error = std::ptr::null_mut();
            let uri = (a.gst_filename_to_uri)(filename.as_ptr(), &mut error);
            if uri.is_null() {
                return Err(failure(a.take_error(error)));
            }
            let caps = (a.gst_caps_from_string)(c"video/x-raw,format=BGRA".as_ptr());
            // sync=true schedules video against the same clock as the audio sink.
            // Dropping late video is bounded; audio remains continuous.
            set(
                sink,
                c"caps".as_ptr(),
                caps,
                c"sync".as_ptr(),
                1i32,
                c"max-buffers".as_ptr(),
                1u32,
                c"drop".as_ptr(),
                1i32,
                c"enable-last-sample".as_ptr(),
                0i32,
                c"wait-on-eos".as_ptr(),
                0i32,
                std::ptr::null::<c_char>(),
            );
            (a.gst_mini_object_unref)(caps);
            // Enable audio/video and software conversion, not subtitle rendering.
            set(
                pipeline,
                c"uri".as_ptr(),
                uri,
                c"video-sink".as_ptr(),
                sink,
                c"flags".as_ptr(),
                0x613u32,
                std::ptr::null::<c_char>(),
            );
            (a.g_free)(uri.cast());
            (a.g_signal_connect_data)(
                pipeline,
                c"element-setup".as_ptr(),
                element_setup as *const () as Ptr,
                Arc::as_ptr(a) as Ptr,
                std::ptr::null_mut(),
                0,
            );
            if (a.gst_element_set_state)(pipeline, 3) == 0 {
                return Err(failure("GStreamer could not preroll"));
            }
            Ok(player)
        }
    }
    fn messages(&mut self) -> Result<()> {
        self.equalizer.check()?;
        if let Some(e) = &self.failure {
            return Err(failure(e.clone()));
        }
        unsafe {
            loop {
                // Drain all messages so state changes cannot grow the bus queue.
                let message = (self.api.gst_bus_pop_filtered)(self.bus, u32::MAX);
                if message.is_null() {
                    break;
                }
                match (*message.cast::<Message>()).kind {
                    1 => self.ended = true,
                    2 => {
                        let mut error = std::ptr::null_mut();
                        let mut debug = std::ptr::null_mut();
                        (self.api.gst_message_parse_error)(message, &mut error, &mut debug);
                        (self.api.g_free)(debug.cast());
                        self.failure = Some(self.api.take_error(error));
                    }
                    32 => {
                        let mut percent = 0;
                        (self.api.gst_message_parse_buffering)(message, &mut percent);
                        self.buffering = percent < 100;
                        (self.api.gst_element_set_state)(
                            self.pipeline,
                            if self.playing && !self.buffering {
                                4
                            } else {
                                3
                            },
                        );
                    }
                    0x200000 => self.seeking = false, // ASYNC_DONE
                    _ => {}
                }
                (self.api.gst_mini_object_unref)(message);
            }
        }
        if let Some(e) = &self.failure {
            Err(failure(e.clone()))
        } else {
            Ok(())
        }
    }
    pub fn info(&mut self) -> Result<PlaybackInfo> {
        self.messages()?;
        let (mut position, mut duration, mut state, mut pending) = (0, 0, 0, 0);
        unsafe {
            (self.api.gst_element_query_position)(self.pipeline, 3, &mut position);
            (self.api.gst_element_query_duration)(self.pipeline, 3, &mut duration);
            (self.api.gst_element_get_state)(self.pipeline, &mut state, &mut pending, 0);
        }
        Ok(PlaybackInfo {
            position: position.max(0) as f64 / 1e9,
            duration: duration.max(0) as f64 / 1e9,
            volume: self.volume,
            state: if state < 3 {
                PlaybackState::Opening
            } else if self.ended {
                PlaybackState::Ended
            } else if self.seeking || self.buffering || pending != 0 {
                PlaybackState::Buffering
            } else if state == 4 {
                PlaybackState::Playing
            } else {
                PlaybackState::Paused
            },
        })
    }
    pub fn command(&mut self, command: i32, value: f64) -> Result<()> {
        self.messages()?;
        unsafe {
            match command {
                0 | 1 => {
                    self.playing = command == 0;
                    if (self.api.gst_element_set_state)(
                        self.pipeline,
                        if self.playing && !self.buffering {
                            4
                        } else {
                            3
                        },
                    ) == 0
                    {
                        return Err(failure("GStreamer state transition failed"));
                    }
                }
                2 => {
                    if value > i64::MAX as f64 / 1e9 {
                        return Err(Error::invalid("seek exceeds GStreamer time range"));
                    }
                    if (self.api.gst_element_seek_simple)(
                        self.pipeline,
                        3,
                        1 | 2,
                        (value * 1e9) as i64,
                    ) == 0
                    {
                        return Err(failure("media is not seekable yet"));
                    }
                    self.seeking = true;
                    self.equalizer.reset();
                    self.ended = false;
                    self.last_pts = None;
                }
                3 => {
                    (self.set)(
                        self.pipeline,
                        c"volume".as_ptr(),
                        value,
                        std::ptr::null::<c_char>(),
                    );
                    self.volume = value;
                }
                _ => return Err(Error::invalid("unknown playback command")),
            }
        }
        Ok(())
    }
    pub fn set_equalizer(&mut self, settings: Settings) -> Result<()> {
        self.messages()?;
        self.equalizer.set(settings)
    }
    pub fn frame(&mut self) -> Result<Option<Arc<VideoFrame>>> {
        self.messages()?;
        if self.seeking {
            return Ok(None);
        }
        unsafe {
            let sample = if self.playing {
                (self.api.gst_app_sink_try_pull_sample)(self.sink, 0)
            } else {
                (self.api.gst_app_sink_try_pull_preroll)(self.sink, 0)
            };
            if sample.is_null() {
                return Ok(None);
            }
            let result = (|| {
                let buffer = (self.api.gst_sample_get_buffer)(sample);
                if buffer.is_null() {
                    return Err(failure("missing decoded buffer"));
                }
                let pts = (*buffer).pts;
                if pts == u64::MAX || self.last_pts == Some(pts) {
                    return Ok(None);
                }
                let caps = (self.api.gst_sample_get_caps)(sample);
                if caps.is_null() {
                    return Err(failure("missing decoded caps"));
                }
                let s = (self.api.gst_caps_get_structure)(caps, 0);
                let (mut w, mut h) = (0, 0);
                if s.is_null()
                    || (self.api.gst_structure_get_int)(s, c"width".as_ptr(), &mut w) == 0
                    || (self.api.gst_structure_get_int)(s, c"height".as_ptr(), &mut h) == 0
                    || w <= 0
                    || h <= 0
                {
                    return Err(failure("invalid video dimensions"));
                }
                let bytes = self.api.bytes(buffer)?;
                if u64::from(w as u32) * u64::from(h as u32) * 4 != bytes.len() as u64 {
                    return Err(Error::unsupported(
                        "GStreamer output must be tightly packed BGRA",
                    ));
                }
                let frame = VideoFrame::new(
                    VideoDescriptor::new(
                        VideoPixelFormat::Bgra,
                        w as u32,
                        h as u32,
                        (pts / 1000) as i64,
                    ),
                    &bytes,
                    None,
                )?;
                self.last_pts = Some(pts);
                Ok(Some(Arc::new(frame)))
            })();
            (self.api.gst_mini_object_unref)(sample);
            result
        }
    }
}
impl Drop for NativePlayer {
    fn drop(&mut self) {
        unsafe {
            (self.api.gst_element_set_state)(self.pipeline, 1);
            for p in [self.bus, self.sink, self.pipeline] {
                if !p.is_null() {
                    (self.api.gst_object_unref)(p);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clocked_audio_filter_changes_pcm_without_a_device() {
        // Use the same identity handoff and F32 negotiation as playbin's
        // audio-filter, but capture its actual output rather than play a tone.
        let api = Api::get().unwrap();
        let control = Control::default();
        let mut settings = Settings::new(1).unwrap();
        settings.set_band(0, 1000.0, 6.0, 2.0).unwrap();
        control.set(settings).unwrap();
        let format = if cfg!(target_endian = "big") {
            "F32BE"
        } else {
            "F32LE"
        };
        let description = CString::new(format!("audiotestsrc wave=sine freq=1000 volume=0.1 samplesperbuffer=480 num-buffers=20 ! audioconvert ! audio/x-raw,format={format},rate=48000,channels=2,layout=interleaved ! identity name=eq signal-handoffs=true ! appsink name=out sync=false max-buffers=2")).unwrap();
        unsafe {
            let mut error = std::ptr::null_mut();
            let pipeline = (api.gst_parse_launch)(description.as_ptr(), &mut error);
            assert!(error.is_null() && !pipeline.is_null());
            struct Running(Arc<Api>, Ptr);
            impl Drop for Running {
                fn drop(&mut self) {
                    unsafe {
                        (self.0.gst_element_set_state)(self.1, 1);
                        (self.0.gst_object_unref)(self.1);
                    }
                }
            }
            let _running = Running(api.clone(), pipeline);
            let identity = (api.gst_bin_get_by_name)(pipeline, c"eq".as_ptr());
            let sink = (api.gst_bin_get_by_name)(pipeline, c"out".as_ptr());
            assert!(!identity.is_null() && !sink.is_null());
            let tap = Box::new(AudioTap {
                api: api.clone(),
                stream: Mutex::new(Stream::new(control.clone())),
                control: control.clone(),
            });
            (api.g_signal_connect_data)(
                identity,
                c"handoff".as_ptr(),
                audio_tap as *const () as Ptr,
                Box::into_raw(tap).cast(),
                audio_tap_drop as *const () as Ptr,
                0,
            );
            (api.gst_object_unref)(identity);
            assert_ne!((api.gst_element_set_state)(pipeline, 4), 0);
            let mut energy = 0.0;
            let mut count = 0;
            for block in 0..20 {
                let sample = (api.gst_app_sink_try_pull_sample)(sink, 2_000_000_000);
                assert!(!sample.is_null(), "missing audio block {block}");
                let bytes = api.bytes((api.gst_sample_get_buffer)(sample)).unwrap();
                (api.gst_mini_object_unref)(sample);
                control.check().unwrap();
                if block >= 10 {
                    for sample in bytes.as_chunks::<4>().0 {
                        energy += f64::from(f32::from_ne_bytes(*sample)).powi(2);
                        count += 1;
                    }
                }
            }
            (api.gst_object_unref)(sink);
            let gain_db = 10.0 * (energy / count as f64 / 0.005).log10();
            assert!((gain_db - 6.0).abs() < 0.01, "measured {gain_db} dB");
        }
    }
}
