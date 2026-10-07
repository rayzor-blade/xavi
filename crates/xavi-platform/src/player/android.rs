//! NDK-only playback: MediaExtractor/MediaCodec, ImageReader, and AAudio.
//! A library-owned worker pumps bounded decoder queues even when the caller is
//! busy rendering. No Java glue or application-authored native code is needed.
use super::*;
use ndk_sys as sys;
use std::{
    ffi::{CStr, c_char},
    fs::File,
    os::fd::AsRawFd,
    ptr,
    sync::mpsc,
    thread::JoinHandle,
    time::{Duration, Instant},
};
use xavi_core::{ErrorKind, VideoDescriptor, VideoPixelFormat};

fn failure(e: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Io, format!("Android playback: {e}"))
}
fn media(s: sys::media_status_t) -> Result<()> {
    if s.0 == 0 {
        Ok(())
    } else {
        Err(failure(format!("media status {}", s.0)))
    }
}
fn audio(s: i32) -> Result<()> {
    if s >= 0 {
        Ok(())
    } else {
        Err(failure(format!("audio status {s}")))
    }
}
unsafe fn integer(f: *mut sys::AMediaFormat, key: &CStr, default: i32) -> i32 {
    let mut v = default;
    unsafe {
        sys::AMediaFormat_getInt32(f, key.as_ptr(), &mut v);
    }
    v
}
type Job = Box<dyn FnOnce(&mut Backend) + Send>;
pub struct NativePlayer {
    jobs: Option<mpsc::SyncSender<Job>>,
    thread: Option<JoinHandle<()>>,
}
impl NativePlayer {
    pub fn open(path: &Path) -> Result<Self> {
        let path = path.canonicalize().map_err(failure)?;
        let (jobs, receive) = mpsc::sync_channel::<Job>(1);
        let (ready, result) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("xavi-android-player".into())
            .spawn(move || match Backend::open(&path) {
                Ok(mut b) => {
                    if ready.send(Ok(())).is_err() {
                        return;
                    }
                    loop {
                        match receive.recv_timeout(Duration::from_millis(3)) {
                            Ok(job) => job(&mut b),
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                        }
                        if b.failure.is_none()
                            && let Err(e) = b.pump()
                        {
                            b.failure = Some(e);
                        }
                    }
                }
                Err(e) => {
                    let _ = ready.send(Err(e));
                }
            })
            .map_err(failure)?;
        let p = Self {
            jobs: Some(jobs),
            thread: Some(thread),
        };
        result.recv().map_err(failure)??;
        Ok(p)
    }
    fn call<T: Send + 'static>(
        &self,
        op: impl FnOnce(&mut Backend) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (send, recv) = mpsc::sync_channel(1);
        self.jobs
            .as_ref()
            .ok_or_else(Error::closed)?
            .send(Box::new(move |b| {
                let _ = send.send(op(b));
            }))
            .map_err(failure)?;
        recv.recv().map_err(failure)?
    }
    pub fn info(&mut self) -> Result<PlaybackInfo> {
        self.call(Backend::info)
    }
    pub fn command(&mut self, c: i32, v: f64) -> Result<()> {
        self.call(move |b| b.command(c, v))
    }
    pub fn set_equalizer(&mut self, settings: EqualizerSettings) -> Result<()> {
        self.call(move |b| b.equalizer.control.set(settings))
    }
    pub fn frame(&mut self) -> Result<Option<Arc<VideoFrame>>> {
        self.call(|b| {
            b.check()?;
            Ok(b.frame.take())
        })
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

struct Reader(*mut sys::AImageReader);
impl Drop for Reader {
    fn drop(&mut self) {
        unsafe {
            sys::AImageReader_delete(self.0);
        }
    }
}
struct Image(*mut sys::AImage);
impl Drop for Image {
    fn drop(&mut self) {
        unsafe {
            sys::AImage_delete(self.0);
        }
    }
}
struct Track {
    extractor: *mut sys::AMediaExtractor,
    codec: *mut sys::AMediaCodec,
    format: *mut sys::AMediaFormat,
    reader: Option<Reader>,
    input_end: bool,
    ended: bool,
    held: Option<(usize, sys::AMediaCodecBufferInfo)>,
    duration: f64,
}
impl Drop for Track {
    fn drop(&mut self) {
        unsafe {
            if !self.codec.is_null() {
                sys::AMediaCodec_stop(self.codec);
                sys::AMediaCodec_delete(self.codec);
            }
            if !self.format.is_null() {
                sys::AMediaFormat_delete(self.format);
            }
            if !self.extractor.is_null() {
                sys::AMediaExtractor_delete(self.extractor);
            }
        }
    }
}
impl Track {
    fn open(path: &Path, video: bool) -> Result<Option<Self>> {
        let file = File::open(path).map_err(failure)?;
        let len = i64::try_from(file.metadata().map_err(failure)?.len()).map_err(failure)?;
        unsafe {
            let extractor = sys::AMediaExtractor_new();
            if extractor.is_null() {
                return Err(Error::exhausted());
            }
            let mut t = Self {
                extractor,
                codec: ptr::null_mut(),
                format: ptr::null_mut(),
                reader: None,
                input_end: false,
                ended: false,
                held: None,
                duration: 0.0,
            };
            media(sys::AMediaExtractor_setDataSourceFd(
                extractor,
                file.as_raw_fd(),
                0,
                len,
            ))?;
            for i in 0..sys::AMediaExtractor_getTrackCount(extractor) {
                let f = sys::AMediaExtractor_getTrackFormat(extractor, i);
                if f.is_null() {
                    continue;
                }
                let mut mime: *const c_char = ptr::null();
                let matches = sys::AMediaFormat_getString(f, c"mime".as_ptr(), &mut mime)
                    && !mime.is_null()
                    && CStr::from_ptr(mime).to_bytes().starts_with(if video {
                        b"video/"
                    } else {
                        b"audio/"
                    });
                if !matches {
                    sys::AMediaFormat_delete(f);
                    continue;
                }
                t.format = f;
                let mut duration = 0;
                sys::AMediaFormat_getInt64(f, c"durationUs".as_ptr(), &mut duration);
                t.duration = duration.max(0) as f64 / 1e6;
                media(sys::AMediaExtractor_selectTrack(extractor, i))?;
                t.codec = sys::AMediaCodec_createDecoderByType(mime);
                if t.codec.is_null() {
                    return Err(Error::unsupported("no Android decoder for track"));
                }
                let mut surface = ptr::null_mut();
                if video {
                    let w = integer(f, c"width", 0);
                    let h = integer(f, c"height", 0);
                    if w <= 0 || h <= 0 || w as u64 * h as u64 * 4 > crate::MAX_BYTES as u64 {
                        return Err(Error::unsupported("video exceeds frame limit"));
                    }
                    let transfer = integer(f, c"color-transfer", 0);
                    if matches!(transfer, 6 | 7) {
                        return Err(Error::unsupported("HDR playback needs tone mapping"));
                    }
                    let mut reader = ptr::null_mut();
                    media(sys::AImageReader_new(w, h, 35, 3, &mut reader))?;
                    if reader.is_null() {
                        return Err(Error::exhausted());
                    }
                    t.reader = Some(Reader(reader));
                    media(sys::AImageReader_getWindow(reader, &mut surface))?;
                } else {
                    sys::AMediaFormat_setInt32(f, c"pcm-encoding".as_ptr(), 2);
                }
                media(sys::AMediaCodec_configure(
                    t.codec,
                    f,
                    surface,
                    ptr::null_mut(),
                    0,
                ))?;
                media(sys::AMediaCodec_start(t.codec))?;
                return Ok(Some(t));
            }
            Ok(None)
        }
    }
    fn feed(&mut self) -> Result<()> {
        unsafe {
            for _ in 0..4 {
                if self.input_end {
                    break;
                }
                let index = sys::AMediaCodec_dequeueInputBuffer(self.codec, 0);
                if index == -1 {
                    break;
                }
                if index < 0 {
                    return Err(failure("decoder input failed"));
                }
                let mut capacity = 0;
                let data =
                    sys::AMediaCodec_getInputBuffer(self.codec, index as usize, &mut capacity);
                if data.is_null() {
                    return Err(failure("missing decoder input buffer"));
                }
                let pts = sys::AMediaExtractor_getSampleTime(self.extractor);
                let size = sys::AMediaExtractor_readSampleData(self.extractor, data, capacity);
                if size < 0 {
                    media(sys::AMediaCodec_queueInputBuffer(
                        self.codec,
                        index as usize,
                        0,
                        0,
                        0,
                        4,
                    ))?;
                    self.input_end = true;
                } else {
                    if size as usize > capacity {
                        return Err(failure("container packet exceeds decoder input capacity"));
                    }
                    if sys::AMediaExtractor_getSampleFlags(self.extractor) & 2 != 0 {
                        return Err(Error::unsupported("encrypted tracks are unsupported"));
                    }
                    media(sys::AMediaCodec_queueInputBuffer(
                        self.codec,
                        index as usize,
                        0,
                        size as usize,
                        // The NDK's unsigned parameter transports MediaCodec's
                        // signed int64 timestamp. Negative AAC preroll must be
                        // decoded, then trimmed at the presentation boundary.
                        pts as u64,
                        0,
                    ))?;
                    sys::AMediaExtractor_advance(self.extractor);
                }
            }
        }
        Ok(())
    }
    fn output(&mut self) -> Result<()> {
        if self.held.is_some() || self.ended {
            return Ok(());
        }
        unsafe {
            for _ in 0..4 {
                let mut info = std::mem::zeroed();
                let index = sys::AMediaCodec_dequeueOutputBuffer(self.codec, &mut info, 0);
                match index {
                    -1 => break,
                    -2 => {
                        let f = sys::AMediaCodec_getOutputFormat(self.codec);
                        if f.is_null() {
                            return Err(failure("missing decoder output format"));
                        }
                        sys::AMediaFormat_delete(self.format);
                        self.format = f;
                    }
                    -3 => {}
                    i if i >= 0 => {
                        self.held = Some((i as usize, info));
                        break;
                    }
                    _ => return Err(failure(format!("decoder output {index}"))),
                }
            }
        }
        Ok(())
    }
    fn release(&mut self, render: bool) -> Result<()> {
        if let Some((index, info)) = self.held.take() {
            unsafe {
                media(sys::AMediaCodec_releaseOutputBuffer(
                    self.codec, index, render,
                ))?;
            }
            self.ended = info.flags & 4 != 0;
        }
        Ok(())
    }
    fn seek(&mut self, seconds: f64) -> Result<()> {
        unsafe {
            media(sys::AMediaCodec_flush(self.codec))?;
            self.held = None;
            self.ended = false;
            self.input_end = false;
            media(sys::AMediaExtractor_seekTo(
                self.extractor,
                (seconds * 1e6) as i64,
                sys::SeekMode::AMEDIAEXTRACTOR_SEEK_PREVIOUS_SYNC,
            ))?;
            if let Some(r) = &self.reader {
                loop {
                    let mut image = ptr::null_mut();
                    if sys::AImageReader_acquireNextImage(r.0, &mut image).0 != 0 {
                        break;
                    }
                    drop(Image(image));
                }
            }
        }
        Ok(())
    }
}
struct Audio {
    stream: *mut sys::AAudioStream,
    rate: i32,
    channels: i32,
    base: f64,
    written: i64,
    pending: Vec<f32>,
    offset: usize,
    packet_frame: i64,
}
impl Drop for Audio {
    fn drop(&mut self) {
        unsafe {
            sys::AAudioStream_close(self.stream);
        }
    }
}
impl Audio {
    fn open(rate: i32, channels: i32, base: f64, playing: bool) -> Result<Self> {
        if rate <= 0 || !(1..=8).contains(&channels) {
            return Err(Error::unsupported("invalid decoded audio format"));
        }
        unsafe {
            let mut builder = ptr::null_mut();
            audio(sys::AAudio_createStreamBuilder(&mut builder))?;
            sys::AAudioStreamBuilder_setDirection(builder, 0);
            sys::AAudioStreamBuilder_setFormat(builder, 2); // AAUDIO_FORMAT_PCM_FLOAT
            sys::AAudioStreamBuilder_setSampleRate(builder, rate);
            sys::AAudioStreamBuilder_setChannelCount(builder, channels);
            sys::AAudioStreamBuilder_setSharingMode(builder, 1);
            let mut stream = ptr::null_mut();
            let status = sys::AAudioStreamBuilder_openStream(builder, &mut stream);
            sys::AAudioStreamBuilder_delete(builder);
            audio(status)?;
            if stream.is_null() {
                return Err(Error::exhausted());
            }
            let a = Self {
                stream,
                rate,
                channels,
                base,
                written: 0,
                pending: Vec::new(),
                offset: 0,
                packet_frame: 0,
            };
            if sys::AAudioStream_getSampleRate(stream) != rate
                || sys::AAudioStream_getChannelCount(stream) != channels
                || sys::AAudioStream_getFormat(stream) != 2
            {
                return Err(Error::unsupported(
                    "audio device could not negotiate decoded PCM",
                ));
            }
            sys::AAudioStream_setBufferSizeInFrames(
                stream,
                (rate / 20).max(sys::AAudioStream_getFramesPerBurst(stream) * 2),
            );
            if playing {
                audio(sys::AAudioStream_requestStart(stream))?;
            }
            Ok(a)
        }
    }
    fn read_frames(&self) -> i64 {
        unsafe {
            let (mut frames, mut time) = (0, 0);
            // Prefer the device's presentation clock. During startup a device
            // may not have a timestamp yet; the stream read counter is the
            // bounded fallback, never wall time advancing past queued audio.
            if sys::AAudioStream_getTimestamp(self.stream, 1, &mut frames, &mut time) < 0 {
                frames = sys::AAudioStream_getFramesRead(self.stream);
            }
            frames.max(0).min(self.written)
        }
    }
    fn position(&self) -> f64 {
        self.base + self.read_frames() as f64 / self.rate as f64
    }
    fn write(&mut self, volume: f64) -> Result<()> {
        let channels = self.channels as usize;
        if self.offset >= self.pending.len() {
            return Ok(());
        }
        let packet = self.packet_frame + (self.offset / channels) as i64;
        if packet < self.written {
            self.offset = (self.offset + ((self.written - packet) as usize) * channels)
                .min(self.pending.len());
            return Ok(());
        }
        let gap = (packet - self.written) as usize;
        let count = if gap > 0 {
            gap.min(1024)
        } else {
            ((self.pending.len() - self.offset) / channels).min(1024)
        };
        let mut samples = vec![0f32; count * channels];
        if gap == 0 {
            for (out, input) in samples.iter_mut().zip(&self.pending[self.offset..]) {
                *out = (*input as f64 * volume).clamp(-1.0, 1.0) as f32;
            }
        }
        let n = unsafe {
            sys::AAudioStream_write(self.stream, samples.as_ptr().cast(), count as i32, 0)
        };
        audio(n)?;
        self.written += n as i64;
        if gap == 0 {
            self.offset += n as usize * channels;
        }
        Ok(())
    }
}
struct Backend {
    equalizer: equalizer::Stream,
    video: Option<Track>,
    sound: Option<Track>,
    audio: Option<Audio>,
    frame: Option<Arc<VideoFrame>>,
    duration: f64,
    position: f64,
    target: f64,
    playing: bool,
    seeking: bool,
    ended: bool,
    volume: f64,
    tick: Instant,
    failure: Option<Error>,
    video_prerolled: bool,
}
impl Backend {
    fn open(path: &Path) -> Result<Self> {
        let video = Track::open(path, true)?;
        let sound = Track::open(path, false)?;
        if video.is_none() && sound.is_none() {
            return Err(Error::unsupported("file has no playable tracks"));
        }
        let duration = video
            .as_ref()
            .map_or(0.0, |t| t.duration)
            .max(sound.as_ref().map_or(0.0, |t| t.duration));
        Ok(Self {
            equalizer: equalizer::Stream::new(equalizer::Control::default()),
            video,
            sound,
            audio: None,
            frame: None,
            duration,
            position: 0.0,
            target: 0.0,
            playing: false,
            seeking: true,
            ended: false,
            volume: 1.0,
            tick: Instant::now(),
            failure: None,
            video_prerolled: false,
        })
    }
    fn check(&self) -> Result<()> {
        self.equalizer.control.check()?;
        if let Some(e) = &self.failure {
            Err(e.clone())
        } else {
            Ok(())
        }
    }
    fn info(&mut self) -> Result<PlaybackInfo> {
        self.check()?;
        Ok(PlaybackInfo {
            position: self.position,
            duration: self.duration,
            volume: self.volume,
            state: if self.seeking {
                PlaybackState::Buffering
            } else if self.ended {
                PlaybackState::Ended
            } else if self.playing {
                PlaybackState::Playing
            } else {
                PlaybackState::Paused
            },
        })
    }
    fn command(&mut self, command: i32, value: f64) -> Result<()> {
        self.check()?;
        match command {
            0 | 1 => {
                self.pump()?;
                self.playing = command == 0;
                self.tick = Instant::now();
                if let Some(a) = &self.audio {
                    unsafe {
                        audio(if self.playing {
                            sys::AAudioStream_requestStart(a.stream)
                        } else {
                            sys::AAudioStream_requestPause(a.stream)
                        })?;
                    }
                }
            }
            2 => {
                for t in [&mut self.video, &mut self.sound].into_iter().flatten() {
                    t.seek(value)?;
                }
                self.audio = None;
                self.equalizer.control.reset();
                self.frame = None;
                self.target = value;
                self.position = value;
                self.seeking = true;
                self.ended = false;
                self.video_prerolled = false;
                self.tick = Instant::now();
            }
            3 => self.volume = value,
            _ => return Err(Error::invalid("unknown playback command")),
        }
        Ok(())
    }
    fn pump(&mut self) -> Result<()> {
        self.check()?;
        let now = Instant::now();
        let elapsed = now.duration_since(self.tick).as_secs_f64();
        self.tick = now;
        let audio_done = self.sound.as_ref().is_none_or(|t| t.ended)
            && self
                .audio
                .as_ref()
                .is_none_or(|a| a.offset >= a.pending.len() && a.read_frames() >= a.written);
        if self.playing && !self.seeking && !self.ended {
            if !audio_done {
                if let Some(a) = &self.audio {
                    self.position = self.position.max(a.position());
                }
            } else {
                self.position += elapsed;
            }
        }
        if let Some(t) = &mut self.sound {
            t.feed()?;
            if self
                .audio
                .as_ref()
                .is_none_or(|a| a.offset >= a.pending.len())
            {
                t.output()?;
                if let Some((index, info)) = t.held {
                    if info.size > 0 {
                        unsafe {
                            let rate = integer(t.format, c"sample-rate", 0);
                            let channels = integer(t.format, c"channel-count", 0);
                            if integer(t.format, c"pcm-encoding", 2) != 2 {
                                return Err(Error::unsupported(
                                    "decoder did not produce signed 16-bit PCM",
                                ));
                            }
                            if self.audio.is_none() {
                                self.audio =
                                    Some(Audio::open(rate, channels, self.target, self.playing)?);
                            }
                            let a = self.audio.as_mut().unwrap();
                            if rate != a.rate || channels != a.channels {
                                return Err(Error::unsupported("midstream audio format change"));
                            }
                            let mut capacity = 0;
                            let data =
                                sys::AMediaCodec_getOutputBuffer(t.codec, index, &mut capacity);
                            if data.is_null()
                                || info.offset < 0
                                || info.size as usize > crate::MAX_BYTES / 2
                                || (info.offset as usize)
                                    .checked_add(info.size as usize)
                                    .is_none_or(|n| n > capacity)
                                || !(info.size as usize).is_multiple_of(2 * channels as usize)
                            {
                                return Err(failure("invalid decoded PCM buffer"));
                            }
                            let bytes = std::slice::from_raw_parts(
                                data.add(info.offset as usize),
                                info.size as usize,
                            );
                            // Preserve boosted headroom until volume is applied
                            // at the AAudio boundary. Process each decoded block
                            // once even when a device write accepts only part.
                            let mut pcm: Vec<u8> = bytes
                                .as_chunks::<2>()
                                .0
                                .iter()
                                .flat_map(|b| {
                                    (f32::from(i16::from_ne_bytes(*b)) / 32768.0).to_ne_bytes()
                                })
                                .collect();
                            self.equalizer.interleaved(
                                &mut pcm,
                                rate as f64,
                                channels as usize,
                                true,
                                false,
                            )?;
                            a.pending = pcm
                                .as_chunks::<4>()
                                .0
                                .iter()
                                .map(|b| f32::from_ne_bytes(*b))
                                .collect();
                            a.offset = 0;
                            a.packet_frame = ((info.presentationTimeUs as f64 / 1e6 - a.base)
                                * rate as f64)
                                .round() as i64;
                        }
                    }
                    t.release(false)?;
                }
            }
            if let Some(a) = &mut self.audio {
                a.write(self.volume)?;
            }
        }
        if let Some(t) = &mut self.video {
            if self.playing || !self.video_prerolled {
                t.feed()?;
                t.output()?;
                if let Some((_, info)) = t.held {
                    let pts = info.presentationTimeUs as f64 / 1e6;
                    if info.size == 0 || pts < self.target - 0.000001 {
                        t.release(false)?;
                    } else if !self.video_prerolled || pts <= self.position + 0.005 {
                        t.release(true)?;
                    }
                }
            }
            if let Some(r) = &t.reader {
                unsafe {
                    let mut image = ptr::null_mut();
                    let status = sys::AImageReader_acquireLatestImage(r.0, &mut image);
                    if status.0 == 0 {
                        let image = Image(image);
                        let mut pts = 0;
                        media(sys::AImage_getTimestamp(image.0, &mut pts))?;
                        if pts as f64 / 1e9 >= self.target - 0.000001 {
                            self.frame = Some(Arc::new(image_frame(&image, t.format, pts / 1000)?));
                            self.video_prerolled = true;
                        }
                    } else if status != sys::media_status_t::AMEDIA_IMGREADER_NO_BUFFER_AVAILABLE {
                        media(status)?;
                    }
                }
            }
        }
        if self.video.as_ref().is_none_or(|t| t.ended) || self.video_prerolled {
            self.seeking = false;
        }
        if audio_done && self.video.as_ref().is_none_or(|t| t.ended) {
            self.ended = true;
            self.position = self.duration.max(self.position);
        }
        if self.duration > 0.0 {
            self.position = self.position.min(self.duration);
        }
        Ok(())
    }
}

unsafe fn image_frame(
    image: &Image,
    format: *mut sys::AMediaFormat,
    pts: i64,
) -> Result<VideoFrame> {
    unsafe {
        let mut crop = std::mem::zeroed();
        media(sys::AImage_getCropRect(image.0, &mut crop))?;
        let (w, h) = (crop.right - crop.left, crop.bottom - crop.top);
        if crop.left < 0
            || crop.top < 0
            || w <= 0
            || h <= 0
            || w as u64 * h as u64 * 4 > crate::MAX_BYTES as u64
        {
            return Err(failure("invalid image crop"));
        }
        let mut planes = Vec::new();
        for i in 0..3 {
            let (mut data, mut length, mut row, mut pixel) = (ptr::null_mut(), 0, 0, 0);
            media(sys::AImage_getPlaneData(image.0, i, &mut data, &mut length))?;
            media(sys::AImage_getPlaneRowStride(image.0, i, &mut row))?;
            media(sys::AImage_getPlanePixelStride(image.0, i, &mut pixel))?;
            if data.is_null()
                || length <= 0
                || row <= 0
                || pixel <= 0
                || length as usize > crate::MAX_BYTES
            {
                return Err(failure("invalid image plane"));
            }
            planes.push((
                std::slice::from_raw_parts(data, length as usize),
                row as usize,
                pixel as usize,
            ));
        }
        let full = integer(format, c"color-range", 2) == 1;
        let bt709 = integer(format, c"color-standard", if w >= 1280 { 1 } else { 2 }) == 1;
        let mut pixels = vec![0; w as usize * h as usize * 4];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let mut sample = [0.0; 3];
                for (i, (data, row, pixel)) in planes.iter().enumerate() {
                    let shift = usize::from(i != 0);
                    let offset = ((y + crop.top as usize) >> shift) * row
                        + ((x + crop.left as usize) >> shift) * pixel;
                    sample[i] = *data
                        .get(offset)
                        .ok_or_else(|| failure("image plane is too short"))?
                        as f64;
                }
                let yy = if full {
                    sample[0]
                } else {
                    (sample[0] - 16.0) * 255.0 / 219.0
                };
                let scale = if full { 1.0 } else { 255.0 / 224.0 };
                let u = (sample[1] - 128.0) * scale;
                let v = (sample[2] - 128.0) * scale;
                let (r, g, b) = if bt709 {
                    (
                        yy + 1.5748 * v,
                        yy - 0.1873 * u - 0.4681 * v,
                        yy + 1.8556 * u,
                    )
                } else {
                    (
                        yy + 1.402 * v,
                        yy - 0.344136 * u - 0.714136 * v,
                        yy + 1.772 * u,
                    )
                };
                let offset = (y * w as usize + x) * 4;
                pixels[offset..offset + 4].copy_from_slice(&[
                    b.round().clamp(0.0, 255.0) as u8,
                    g.round().clamp(0.0, 255.0) as u8,
                    r.round().clamp(0.0, 255.0) as u8,
                    255,
                ]);
            }
        }
        VideoFrame::new(
            VideoDescriptor::new(VideoPixelFormat::Bgra, w as u32, h as u32, pts),
            &pixels,
            None,
        )
    }
}
