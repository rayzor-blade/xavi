// Compiled in the runtime adapter beside the generated model, not in the safe
// backend crate. The host selects a stable context using crate::with_media.
use crate::*;
use xavi_core as core;

fn call<T: Default>(f: impl FnOnce(&xavi_backend::MediaBackend) -> core::Result<T>) -> T {
    match crate::with_media(f) {
        Ok(value) => value,
        Err(error) => {
            let kind = match error.kind {
                core::ErrorKind::InvalidArgument => ErrorKind::Type,
                _ => ErrorKind::Runtime,
            };
            host::raise(kind, &error.to_string());
            T::default()
        }
    }
}

fn size(value: i64) -> core::Result<u32> {
    u32::try_from(value).map_err(|_| core::Error::invalid("size must be in 0..=4294967295"))
}

fn duration(value: Option<i64>) -> core::Result<Option<u64>> {
    value
        .map(|value| {
            u64::try_from(value).map_err(|_| core::Error::invalid("duration must be nonnegative"))
        })
        .transpose()
}

fn optional_duration(value: Option<u64>) -> core::Result<OptionalDuration> {
    Ok(match value {
        None => OptionalDuration::Unknown,
        Some(value) => OptionalDuration::Value {
            microseconds: i64::try_from(value)
                .map_err(|_| core::Error::unsupported("duration exceeds the native i64 carrier"))?,
        },
    })
}

// Guest memory is borrowed only within these closures. No host allocation,
// callback, future resolution or error raising may occur until the borrow ends.
// The host guarantees valid pinned carriers and exclusive destination access.
fn read<T>(buffer: Buffer, f: impl FnOnce(&[u8]) -> core::Result<T>) -> core::Result<T> {
    let len = buffer.len();
    if len == 0 {
        return f(&[]);
    }
    if len > isize::MAX as usize || buffer.as_ptr().is_null() {
        return Err(core::Error::invalid("invalid source buffer"));
    }
    // SAFETY: carrier validity and pinning are the adapter host's contract.
    f(unsafe { std::slice::from_raw_parts(buffer.as_ptr(), len) })
}

fn write<T>(buffer: BufferMut, f: impl FnOnce(&mut [u8]) -> core::Result<T>) -> core::Result<T> {
    write_buffer(buffer.buffer(), f)
}

fn write_buffer<T>(
    buffer: Buffer,
    f: impl FnOnce(&mut [u8]) -> core::Result<T>,
) -> core::Result<T> {
    let len = buffer.len();
    if len == 0 {
        return f(&mut []);
    }
    let ptr = buffer.as_mut_ptr().unwrap_or_default();
    if len > isize::MAX as usize || ptr.is_null() {
        return Err(core::Error::invalid("invalid destination buffer"));
    }
    // SAFETY: the host supplies a pinned, exclusively writable destination.
    f(unsafe { std::slice::from_raw_parts_mut(ptr, len) })
}

// Match names, not ordinals: the IDL and core may evolve independently.
macro_rules! formats {
    ($from:ident, $to:ident, $native:ident, $core:ident, {$($a:ident => $b:ident),+ $(,)?}) => {
        fn $from(value: i32) -> core::Result<core::$core> {
            match $native::from_native(value) {
                $(Some($native::$a) => Ok(core::$core::$b),)+
                None => Err(core::Error::invalid("undeclared media enum value")),
            }
        }
        fn $to(value: core::$core) -> $native {
            match value { $(core::$core::$b => $native::$a,)+ }
        }
    };
}
formats!(audio_format_in, audio_format_out, AudioSampleFormat, AudioSampleFormat, {
    U8 => U8, S16 => S16, S32 => S32, F32 => F32,
    U8Planar => U8Planar, S16Planar => S16Planar, S32Planar => S32Planar, F32Planar => F32Planar,
});
formats!(video_format_in, video_format_out, VideoPixelFormat, VideoPixelFormat, {
    I420 => I420, I420A => I420A, I422 => I422, I444 => I444, NV12 => Nv12,
    RGBA => Rgba, RGBX => Rgbx, BGRA => Bgra, BGRX => Bgrx,
});
formats!(chunk_type_in, chunk_type_out, EncodedChunkType, EncodedChunkType, { Key => Key, Delta => Delta });
formats!(primaries_in, primaries_out, VideoColorPrimaries, VideoColorPrimaries, {
    Bt709 => Bt709, Bt470bg => Bt470bg, Smpte170m => Smpte170m, Bt2020 => Bt2020, Smpte432 => Smpte432,
});
formats!(transfer_in, transfer_out, VideoTransferCharacteristics, VideoTransferCharacteristics, {
    Bt709 => Bt709, Smpte170m => Smpte170m, Iec6196621 => Iec61966_2_1, Linear => Linear, Pq => Pq, Hlg => Hlg,
});
formats!(matrix_in, matrix_out, VideoMatrixCoefficients, VideoMatrixCoefficients, {
    Rgb => Rgb, Bt709 => Bt709, Bt470bg => Bt470bg, Smpte170m => Smpte170m, Bt2020Ncl => Bt2020Ncl,
});

fn audio_options(options: &AudioDataCopyToOptions) -> core::Result<core::AudioCopyOptions> {
    Ok(core::AudioCopyOptions {
        plane_index: size(options.planeIndex)?,
        frame_offset: size(options.frameOffset.unwrap_or(0))?,
        frame_count: options.frameCount.map(size).transpose()?,
        format: options.format.map(audio_format_in).transpose()?,
    })
}

pub fn audio_create(init: &AudioDataInit) -> i32 {
    call(|media| {
        let descriptor = core::AudioDescriptor {
            format: audio_format_in(init.format)?,
            sample_rate: init.sampleRate,
            number_of_frames: size(init.numberOfFrames)?,
            number_of_channels: size(init.numberOfChannels)?,
            timestamp: init.timestamp,
        };
        // Core permits u64 durations. The native profile must be representable
        // on every runtime, including its derived AudioData duration getter.
        let value = read(init.data.get(), |bytes| {
            core::AudioData::new(descriptor, bytes)
        })?;
        i64::try_from(value.duration())
            .map_err(|_| core::Error::unsupported("duration exceeds the native i64 carrier"))?;
        media.retain_audio(std::sync::Arc::new(value))
    })
}

pub fn audio_format(this: i32) -> i32 {
    call(|m| Ok(audio_format_out(m.audio(this)?.descriptor().format).native()))
}
pub fn audio_sample_rate(this: i32) -> f32 {
    call(|m| Ok(m.audio(this)?.descriptor().sample_rate))
}
pub fn audio_frames(this: i32) -> i64 {
    call(|m| Ok(i64::from(m.audio(this)?.descriptor().number_of_frames)))
}
pub fn audio_channels(this: i32) -> i64 {
    call(|m| Ok(i64::from(m.audio(this)?.descriptor().number_of_channels)))
}
pub fn audio_timestamp(this: i32) -> i64 {
    call(|m| Ok(m.audio(this)?.descriptor().timestamp))
}
pub fn audio_duration(this: i32) -> i64 {
    call(|m| {
        i64::try_from(m.audio(this)?.duration())
            .map_err(|_| core::Error::unsupported("duration exceeds the native i64 carrier"))
    })
}
pub fn audio_allocation_size(this: i32, options: &AudioDataCopyToOptions) -> i64 {
    call(|m| {
        Ok(i64::from(
            m.audio_allocation_size(this, audio_options(options)?)?,
        ))
    })
}
pub fn audio_copy_to(this: i32, destination: BufferMut, options: &AudioDataCopyToOptions) {
    call(|m| {
        let options = audio_options(options)?;
        write(destination, |bytes| m.copy_audio(this, bytes, options))
    })
}
pub fn audio_clone(this: i32) -> i32 {
    call(|m| m.clone_audio(this))
}
pub fn audio_close(this: i32) {
    close(this, core::handles::Kind::AudioData);
}

fn rect(value: &DOMRectInit) -> core::Result<core::Rect> {
    core::Rect::from_f64(
        value.x.unwrap_or(0.0),
        value.y.unwrap_or(0.0),
        value.width.unwrap_or(0.0),
        value.height.unwrap_or(0.0),
    )
}
fn layouts(values: &[PlaneLayout]) -> core::Result<Option<Vec<core::PlaneLayout>>> {
    if values.is_empty() {
        return Ok(None);
    }
    values
        .iter()
        .map(|value| {
            Ok(core::PlaneLayout {
                offset: size(value.offset)?,
                stride: size(value.stride)?,
            })
        })
        .collect::<core::Result<Vec<_>>>()
        .map(Some)
}
fn video_options(options: &VideoFrameCopyToOptions) -> core::Result<core::VideoCopyOptions> {
    Ok(core::VideoCopyOptions {
        rect: options.rect.as_ref().map(rect).transpose()?,
        layout: layouts(&options.layout)?,
        format: options.format.map(video_format_in).transpose()?,
    })
}
fn color_space(value: &VideoColorSpaceInit) -> core::Result<core::VideoColorSpace> {
    Ok(core::VideoColorSpace {
        primaries: value.primaries.map(primaries_in).transpose()?,
        transfer: value.transfer.map(transfer_in).transpose()?,
        matrix: value.matrix.map(matrix_in).transpose()?,
        full_range: value.fullRange,
    })
}
pub fn video_create(data: Buffer, init: &VideoFrameBufferInit) -> i32 {
    call(|m| {
        let descriptor = core::VideoDescriptor {
            format: video_format_in(init.format)?,
            coded_width: size(init.codedWidth)?,
            coded_height: size(init.codedHeight)?,
            timestamp: init.timestamp,
            duration: duration(init.duration)?,
            visible_rect: init.visibleRect.as_ref().map(rect).transpose()?,
            display_width: init.displayWidth.map(size).transpose()?,
            display_height: init.displayHeight.map(size).transpose()?,
            color_space: init
                .colorSpace
                .as_ref()
                .map(color_space)
                .transpose()?
                .unwrap_or_default(),
        };
        let layout = layouts(&init.layout)?;
        read(data, |bytes| {
            m.create_video(descriptor, bytes, layout.as_deref())
        })
    })
}
pub fn video_format(this: i32) -> i32 {
    call(|m| Ok(video_format_out(m.video(this)?.info().format).native()))
}
pub fn video_coded_width(this: i32) -> i64 {
    call(|m| Ok(i64::from(m.video(this)?.info().coded_width)))
}
pub fn video_coded_height(this: i32) -> i64 {
    call(|m| Ok(i64::from(m.video(this)?.info().coded_height)))
}
pub fn video_display_width(this: i32) -> i64 {
    call(|m| Ok(i64::from(m.video(this)?.info().display_width)))
}
pub fn video_display_height(this: i32) -> i64 {
    call(|m| Ok(i64::from(m.video(this)?.info().display_height)))
}
pub fn video_timestamp(this: i32) -> i64 {
    call(|m| Ok(m.video(this)?.info().timestamp))
}
pub fn video_duration(this: i32) -> OptionalDuration {
    call(|m| optional_duration(m.video(this)?.info().duration))
}
pub fn video_visible_rect(this: i32) -> PixelRect {
    call(|m| {
        let r = m.video(this)?.info().visible_rect;
        Ok(PixelRect::Value {
            x: r.x.into(),
            y: r.y.into(),
            width: r.width.into(),
            height: r.height.into(),
        })
    })
}
pub fn video_color_space(this: i32) -> ColorSpace {
    call(|m| {
        let c = m.video(this)?.info().color_space;
        Ok(ColorSpace::Value {
            primaries: c
                .primaries
                .map(|v| OptionalPrimaries::Value {
                    value: primaries_out(v),
                })
                .unwrap_or_default(),
            transfer: c
                .transfer
                .map(|v| OptionalTransfer::Value {
                    value: transfer_out(v),
                })
                .unwrap_or_default(),
            matrix: c
                .matrix
                .map(|v| OptionalMatrix::Value {
                    value: matrix_out(v),
                })
                .unwrap_or_default(),
            fullRange: c
                .full_range
                .map(|value| OptionalBool::Value { value })
                .unwrap_or_default(),
        })
    })
}
pub fn video_allocation_size(this: i32, options: &VideoFrameCopyToOptions) -> i64 {
    call(|m| {
        Ok(i64::from(
            m.video_allocation_size(this, &video_options(options)?)?,
        ))
    })
}
pub fn video_copy_to(
    this: i32,
    destination: BufferMut,
    options: &VideoFrameCopyToOptions,
) -> Future<PlaneLayouts> {
    // Root the destination before allocating a future (which may trigger GC).
    let destination = Rooted::new(destination.buffer());
    let future = Rooted::new(Future::<PlaneLayouts>::new());
    let result = crate::with_media(|m| {
        let options = video_options(options)?;
        let result = write_buffer(destination.get(), |bytes| {
            m.copy_video(this, bytes, &options)
        })?;
        m.create_plane_layouts(result)
    });
    match result {
        Ok(handle) => {
            if !future
                .get()
                .resolve_boxed(Box::new(PlaneLayouts::from_handle(handle)))
            {
                crate::with_media(|m| {
                    let _ = m.release(handle);
                });
            }
        }
        Err(error) => {
            future.get().reject(Text::new(&error.to_string()).value());
        }
    }
    future.get()
}
pub fn video_clone(this: i32) -> i32 {
    call(|m| m.clone_video(this))
}
pub fn video_close(this: i32) {
    close(this, core::handles::Kind::VideoFrame);
}

pub fn layouts_count(this: i32) -> i32 {
    call(|m| Ok(m.plane_layouts(this)?.len() as i32))
}
fn plane(m: &xavi_backend::MediaBackend, this: i32, index: i32) -> core::Result<core::PlaneLayout> {
    let index = usize::try_from(index)
        .map_err(|_| core::Error::invalid("plane index must be nonnegative"))?;
    m.plane_layouts(this)?
        .get(index)
        .copied()
        .ok_or_else(|| core::Error::invalid("plane index is out of range"))
}
pub fn layouts_offset(this: i32, index: i32) -> i64 {
    call(|m| Ok(i64::from(plane(m, this, index)?.offset)))
}
pub fn layouts_stride(this: i32, index: i32) -> i64 {
    call(|m| Ok(i64::from(plane(m, this, index)?.stride)))
}
pub fn layouts_close(this: i32) {
    close(this, core::handles::Kind::PlaneLayouts);
}

fn close(this: i32, kind: core::handles::Kind) {
    call(|m| {
        if this != 0 && core::handles::Kind::of(this) != Some(kind) {
            return Err(core::Error::invalid("wrong media handle type"));
        }
        m.release(this)
    });
}

pub fn audio_chunk_create(init: &EncodedAudioChunkInit) -> i32 {
    call(|m| {
        let kind = chunk_type_in(init.r#type)?;
        let duration = duration(init.duration)?;
        read(init.data.get(), |bytes| {
            m.create_audio_chunk(kind, init.timestamp, duration, bytes)
        })
    })
}
pub fn audio_chunk_type(this: i32) -> i32 {
    call(|m| Ok(chunk_type_out(m.audio_chunk(this)?.kind()).native()))
}
pub fn audio_chunk_timestamp(this: i32) -> i64 {
    call(|m| Ok(m.audio_chunk(this)?.timestamp()))
}
pub fn audio_chunk_duration(this: i32) -> OptionalDuration {
    call(|m| optional_duration(m.audio_chunk(this)?.duration()))
}
pub fn audio_chunk_byte_length(this: i32) -> i64 {
    call(|m| Ok(i64::from(m.audio_chunk(this)?.byte_len())))
}
pub fn audio_chunk_copy_to(this: i32, destination: BufferMut) {
    call(|m| write(destination, |bytes| m.copy_audio_chunk(this, bytes)))
}
pub fn audio_chunk_close(this: i32) {
    close(this, core::handles::Kind::EncodedAudioChunk);
}

pub fn video_chunk_create(init: &EncodedVideoChunkInit) -> i32 {
    call(|m| {
        let kind = chunk_type_in(init.r#type)?;
        let duration = duration(init.duration)?;
        read(init.data.get(), |bytes| {
            m.create_video_chunk(kind, init.timestamp, duration, bytes)
        })
    })
}
pub fn video_chunk_type(this: i32) -> i32 {
    call(|m| Ok(chunk_type_out(m.video_chunk(this)?.kind()).native()))
}
pub fn video_chunk_timestamp(this: i32) -> i64 {
    call(|m| Ok(m.video_chunk(this)?.timestamp()))
}
pub fn video_chunk_duration(this: i32) -> OptionalDuration {
    call(|m| optional_duration(m.video_chunk(this)?.duration()))
}
pub fn video_chunk_byte_length(this: i32) -> i64 {
    call(|m| Ok(i64::from(m.video_chunk(this)?.byte_len())))
}
pub fn video_chunk_copy_to(this: i32, destination: BufferMut) {
    call(|m| write(destination, |bytes| m.copy_video_chunk(this, bytes)))
}
pub fn video_chunk_close(this: i32) {
    close(this, core::handles::Kind::EncodedVideoChunk);
}

pub fn player_open(path: Text) -> i32 {
    call(|m| m.player_open(path.as_str()))
}
pub fn player_state(this: i32) -> i32 {
    call(|m| m.with_player(this, |player| {
        use xavi_backend::player::PlaybackState as State;
        Ok(match player.info()?.state {
            State::Opening => PlaybackState::Opening,
            State::Paused => PlaybackState::Paused,
            State::Playing => PlaybackState::Playing,
            State::Buffering => PlaybackState::Buffering,
            State::Ended => PlaybackState::Ended,
        }.native())
    }))
}
pub fn player_position(this: i32) -> f64 {
    call(|m| m.with_player(this, |p| Ok(p.info()?.position)))
}
pub fn player_duration(this: i32) -> f64 {
    call(|m| m.with_player(this, |p| Ok(p.info()?.duration)))
}
pub fn player_volume(this: i32) -> f64 {
    call(|m| m.with_player(this, |p| Ok(p.info()?.volume)))
}
pub fn player_play(this: i32) {
    call(|m| m.with_player(this, |p| p.play()))
}
pub fn player_pause(this: i32) {
    call(|m| m.with_player(this, |p| p.pause()))
}
pub fn player_seek(this: i32, seconds: f64) {
    call(|m| m.with_player(this, |p| p.seek(seconds)))
}
pub fn player_set_volume(this: i32, volume: f64) {
    call(|m| m.with_player(this, |p| p.set_volume(volume)))
}
pub fn player_poll_frame(this: i32) -> bool {
    call(|m| m.with_player(this, |p| p.poll_frame()))
}
pub fn player_take_frame(this: i32) -> i32 {
    call(|m| m.player_take_frame(this))
}
pub fn player_close(this: i32) {
    close(this, core::handles::Kind::MediaPlayer);
}

// Pull-based codecs/streams keep all guest allocation and exceptions here.
use xavi_backend::pipeline as pipe;
use std::sync::Arc;
fn status_out(value: pipe::Status) -> i32 {
    match value { pipe::Status::Pending => StreamReadStatus::Pending, pipe::Status::Ready => StreamReadStatus::Ready, pipe::Status::Ended => StreamReadStatus::Ended }.native()
}
fn payload_kind(value: i32) -> core::Result<pipe::MediaKind> {
    match MediaPayloadKind::from_native(value) {
        Some(MediaPayloadKind::Audio) => Ok(pipe::MediaKind::Audio), Some(MediaPayloadKind::Video) => Ok(pipe::MediaKind::Video),
        Some(MediaPayloadKind::AudioChunk) => Ok(pipe::MediaKind::AudioChunk), Some(MediaPayloadKind::VideoChunk) => Ok(pipe::MediaKind::VideoChunk),
        Some(MediaPayloadKind::Bytes) => Ok(pipe::MediaKind::Bytes), _ => Err(core::Error::invalid("undeclared payload kind")),
    }
}
fn media_lock<T>(value: &std::sync::Mutex<T>) -> core::Result<std::sync::MutexGuard<'_,T>> {
    value.lock().map_err(|_| core::Error::new(core::ErrorKind::InvalidState,"media worker lock is poisoned"))
}
fn audio_configuration(m: &xavi_backend::MediaBackend, id: i32) -> core::Result<core::codec::AudioDecoderConfig> {
    match &*m.configs(id)? { pipe::Configuration::Audio(c) => Ok(c.clone()), _ => Err(core::Error::invalid("expected audio configuration")) }
}
fn video_configuration(m: &xavi_backend::MediaBackend, id: i32) -> core::Result<core::codec::VideoDecoderConfig> {
    match &*m.configs(id)? { pipe::Configuration::Video(c) => Ok(c.clone()), _ => Err(core::Error::invalid("expected video configuration")) }
}
fn config_bytes(buffer: Buffer) -> core::Result<Arc<[u8]>> {
    read(buffer, |bytes| { if bytes.len() > 1024*1024 { return Err(core::Error::invalid("codec description exceeds 1 MiB")); } Ok(Arc::from(bytes)) })
}
pub fn configuration_audio(codec: Text, sample_rate: i64, channels: i64, description: Buffer) -> i32 {
    call(|m| m.insert_configs(pipe::Configuration::Audio(core::codec::AudioDecoderConfig { codec: codec.as_str().into(), sample_rate: size(sample_rate)?, channels: size(channels)?, description: config_bytes(description)? })))
}
pub fn configuration_video(codec: Text, width: i64, height: i64, description: Buffer) -> i32 {
    call(|m| m.insert_configs(pipe::Configuration::Video(core::codec::VideoDecoderConfig { codec: codec.as_str().into(), coded_width: Some(size(width)?), coded_height: Some(size(height)?), description: config_bytes(description)?, color_space: core::VideoColorSpace::default() })))
}
pub fn configuration_codec(this: i32) -> Text {
    let codec = call(|m| Ok(match &*m.configs(this)? { pipe::Configuration::Audio(c) => c.codec.clone(), pipe::Configuration::Video(c) => c.codec.clone() }));
    Text::new(&codec)
}
pub fn configuration_sample_rate(this: i32) -> i64 { call(|m| Ok(audio_configuration(m,this)?.sample_rate.into())) }
pub fn configuration_channels(this: i32) -> i64 { call(|m| Ok(audio_configuration(m,this)?.channels.into())) }
pub fn configuration_width(this: i32) -> i64 { call(|m| Ok(video_configuration(m,this)?.coded_width.unwrap_or(0).into())) }
pub fn configuration_height(this: i32) -> i64 { call(|m| Ok(video_configuration(m,this)?.coded_height.unwrap_or(0).into())) }
pub fn configuration_description_size(this: i32) -> i64 { call(|m| Ok(m.configs(this)?.description().len() as i64)) }
pub fn configuration_copy_description(this: i32, destination: BufferMut) { call(|m| {
    let config = m.configs(this)?; write(destination, |bytes| { let src = config.description(); if bytes.len() < src.len() { return Err(core::Error::invalid("description destination is too small")); } bytes[..src.len()].copy_from_slice(src); Ok(()) })
}) }
pub fn configuration_close(this: i32) { close(this,core::handles::Kind::CodecConfiguration); }
fn retain_item(m: &xavi_backend::MediaBackend, item: pipe::Item) -> core::Result<i32> {
    match item { pipe::Item::Audio(v) => m.retain_audio(v), pipe::Item::Video(v) => m.retain_video(v), pipe::Item::AudioChunk(v) => m.retain_audio_chunk(v), pipe::Item::VideoChunk(v) => m.retain_video_chunk(v), _ => Err(core::Error::invalid("expected a media resource")) }
}
pub fn queue_create(kind: i32, items: i32, bytes: i64) -> i32 { call(|m| m.insert_queues(pipe::Queue::new(payload_kind(kind)?,pipe::limits(items,bytes)?)?)) }
pub fn queue_poll(this: i32) -> i32 { call(|m| Ok(status_out(media_lock(&m.queues(this)?.output)?.status()?))) }
pub fn queue_finish(this: i32) { call(|m| m.queues(this)?.finish()) }
pub fn queue_close(this: i32) { close(this,core::handles::Kind::MediaQueue); }
pub fn queue_write_bytes(this: i32, value: Buffer) -> bool { call(|m| {
    let queue = m.queues(this)?;
    read(value, |bytes| { if bytes.len() > 256*1024*1024 { return Err(core::Error::invalid("byte chunk exceeds 256 MiB")); } queue.write(pipe::Item::Bytes(Arc::from(bytes))) })
}) }
pub fn queue_byte_length(this: i32) -> i64 { call(|m| { let queue = m.queues(this)?; let mut output = media_lock(&queue.output)?; match output.peek()? { pipe::Item::Bytes(bytes) => Ok(bytes.len() as i64), _ => Err(core::Error::invalid("expected byte queue")) } }) }
pub fn queue_read_bytes(this: i32, destination: BufferMut) -> i64 { call(|m| {
    let queue = m.queues(this)?; let mut output = media_lock(&queue.output)?;
    let size = match output.peek()? { pipe::Item::Bytes(src) => write(destination, |bytes| { if bytes.len() < src.len() { return Err(core::Error::invalid("byte destination is too small")); } bytes[..src.len()].copy_from_slice(src); Ok(src.len() as i64) })?, _ => return Err(core::Error::invalid("expected byte queue")) };
    output.take()?; Ok(size)
}) }
fn session(m: &xavi_backend::MediaBackend, handle: i32, input: pipe::MediaKind, output: pipe::MediaKind) -> core::Result<Arc<pipe::Session>> {
    let s = m.codecs(handle)?; if s.input_kind != input || s.output_kind != output { return Err(core::Error::invalid("wrong codec handle type")); } s.worker.check()?; Ok(s)
}
pub fn audio_encoder_create(codec: Text, rate: i64, channels: i64, bitrate: i64, items: i32, bytes: i64) -> i32 { call(|m| {
    let limits = pipe::limits(items,bytes)?; let bitrate = u64::try_from(bitrate).map_err(|_| core::Error::invalid("negative bitrate"))?;
    m.insert_codecs(pipe::Session::audio_encoder(core::codec::AudioEncoderConfig { codec: codec.as_str().into(), sample_rate: size(rate)?, channels: size(channels)?, bitrate: Some(bitrate) }, limits,limits)?)
}) }
pub fn video_encoder_create(codec: Text, width: i64, height: i64, bitrate: i64, framerate: f64, items: i32, bytes: i64) -> i32 { call(|m| {
    let limits = pipe::limits(items,bytes)?; let bitrate = u64::try_from(bitrate).map_err(|_| core::Error::invalid("negative bitrate"))?;
    m.insert_codecs(pipe::Session::video_encoder(core::codec::VideoEncoderConfig { codec: codec.as_str().into(), width: size(width)?, height: size(height)?, bitrate, framerate },limits,limits)?)
}) }
pub fn audio_decoder_create(items: i32, bytes: i64, config: i32) -> i32 { call(|m| { let limits = pipe::limits(items,bytes)?; m.insert_codecs(pipe::Session::audio_decoder(audio_configuration(m,config)?,limits,limits)?) }) }
pub fn video_decoder_create(items: i32, bytes: i64, config: i32) -> i32 { call(|m| { let limits = pipe::limits(items,bytes)?; m.insert_codecs(pipe::Session::video_decoder(video_configuration(m,config)?,limits,limits)?) }) }
fn mux_create(path: Text, audio: Option<i32>, video: Option<i32>, origin: i64, default_duration: i64, items: i32, bytes: i64) -> i32 { call(|m| {
    let config = core::mux::Mp4Config { audio: audio.map(|id| audio_configuration(m,id)).transpose()?, video: video.map(|id| video_configuration(m,id)).transpose()?, timestamp_origin: origin, default_video_duration: duration((default_duration != 0).then_some(default_duration))? };
    m.insert_writers(pipe::Writer::new(path.as_str().into(),config,pipe::limits(items,bytes)?)?)
}) }
pub fn mux_audio(path: Text, audio: i32, origin: i64, items: i32, bytes: i64) -> i32 { mux_create(path,Some(audio),None,origin,0,items,bytes) }
pub fn mux_video(path: Text, video: i32, origin: i64, default_duration: i64, items: i32, bytes: i64) -> i32 { mux_create(path,None,Some(video),origin,default_duration,items,bytes) }
pub fn mux_audio_video(path: Text, audio: i32, video: i32, origin: i64, default_duration: i64, items: i32, bytes: i64) -> i32 { mux_create(path,Some(audio),Some(video),origin,default_duration,items,bytes) }
pub fn mux_finish(this: i32) { call(|m| m.writers(this)?.finish()) }
pub fn mux_finished(this: i32) -> bool { call(|m| m.writers(this)?.finished()) }
pub fn mux_close(this: i32) { close(this,core::handles::Kind::MediaMuxer); }
pub fn mux_end_audio(this: i32) { call(|m| m.writers(this)?.end_audio()) }
pub fn mux_end_video(this: i32) { call(|m| m.writers(this)?.end_video()) }
pub fn demux_open(path: Text, items: i32, bytes: i64) -> i32 { call(|m| m.insert_readers(xavi_backend::demux::Demuxer::open(path.as_str().into(),pipe::limits(items,bytes)?)?)) }
pub fn demux_duration(this: i32) -> f64 { call(|m| { let reader=m.readers(this)?;reader.worker.check()?;let value=*media_lock(&reader.duration)?;Ok(value) }) }
pub fn demux_close(this: i32) { close(this,core::handles::Kind::MediaDemuxer); }
pub fn audio_slice(this: i32, offset: i64, count: i64, timestamp: i64) -> i32 { call(|m| m.retain_audio(Arc::new(m.audio(this)?.slice(size(offset)?,size(count)?,timestamp)?))) }
pub fn audio_retime(this: i32, timestamp: i64) -> i32 { call(|m| m.retain_audio(Arc::new(m.audio(this)?.retime(timestamp)?))) }
pub fn audio_gain(this: i32, gain: f64) -> i32 { call(|m| m.retain_audio(Arc::new(m.audio(this)?.gain(gain)?))) }
pub fn audio_mix(this: i32, other: i32, gain: f64) -> i32 { call(|m| m.retain_audio(Arc::new(m.audio(this)?.mix(&*m.audio(other)?,gain)?))) }
pub fn video_retime(this: i32, timestamp: i64, length: i64) -> i32 { call(|m| m.retain_video(Arc::new(m.video(this)?.retime(timestamp,duration((length != 0).then_some(length))?)?))) }
pub fn video_crop(this: i32, x: i64, y: i64, width: i64, height: i64) -> i32 { call(|m| m.retain_video(Arc::new(m.video(this)?.crop(core::Rect { x: size(x)?, y: size(y)?, width: size(width)?, height: size(height)? })?))) }
pub fn video_resize(this: i32, width: i64, height: i64) -> i32 { call(|m| m.retain_video(Arc::new(m.video(this)?.resize(size(width)?,size(height)?)?))) }
pub fn video_blend(this: i32, other: i32, opacity: f64) -> i32 { call(|m| m.retain_video(Arc::new(m.video(this)?.blend(&*m.video(other)?,opacity)?))) }
pub fn queue_write_audio(this: i32, value: i32) -> bool { call(|m| m.queues(this)?.write(pipe::Item::Audio(m.audio(value)?))) }
pub fn queue_read_audio(this: i32) -> i32 { call(|m| { let queue=m.queues(this)?; if queue.kind != pipe::MediaKind::Audio { return Err(core::Error::invalid("wrong queue type")); } let item=media_lock(&queue.output)?.take()?; retain_item(m,item) }) }
pub fn queue_write_video(this: i32, value: i32) -> bool { call(|m| m.queues(this)?.write(pipe::Item::Video(m.video(value)?))) }
pub fn queue_read_video(this: i32) -> i32 { call(|m| { let queue=m.queues(this)?; if queue.kind != pipe::MediaKind::Video { return Err(core::Error::invalid("wrong queue type")); } let item=media_lock(&queue.output)?.take()?; retain_item(m,item) }) }
pub fn queue_write_audio_chunk(this: i32, value: i32) -> bool { call(|m| m.queues(this)?.write(pipe::Item::AudioChunk(m.audio_chunk(value)?))) }
pub fn queue_read_audio_chunk(this: i32) -> i32 { call(|m| { let queue=m.queues(this)?; if queue.kind != pipe::MediaKind::AudioChunk { return Err(core::Error::invalid("wrong queue type")); } let item=media_lock(&queue.output)?.take()?; retain_item(m,item) }) }
pub fn queue_write_video_chunk(this: i32, value: i32) -> bool { call(|m| m.queues(this)?.write(pipe::Item::VideoChunk(m.video_chunk(value)?))) }
pub fn queue_read_video_chunk(this: i32) -> i32 { call(|m| { let queue=m.queues(this)?; if queue.kind != pipe::MediaKind::VideoChunk { return Err(core::Error::invalid("wrong queue type")); } let item=media_lock(&queue.output)?.take()?; retain_item(m,item) }) }
pub fn audio_encoder_write(this: i32, value: i32) -> bool { call(|m| session(m,this,pipe::MediaKind::Audio,pipe::MediaKind::AudioChunk)?.write(pipe::Item::Audio(m.audio(value)?))) }
pub fn audio_encoder_poll(this: i32) -> i32 { call(|m| { let s=session(m,this,pipe::MediaKind::Audio,pipe::MediaKind::AudioChunk)?; Ok(status_out(media_lock(&s.output)?.status()?)) }) }
pub fn audio_encoder_read(this: i32) -> i32 { call(|m| { let s=session(m,this,pipe::MediaKind::Audio,pipe::MediaKind::AudioChunk)?; let item=media_lock(&s.output)?.take()?; retain_item(m,item) }) }
pub fn audio_encoder_finish(this: i32) { call(|m| session(m,this,pipe::MediaKind::Audio,pipe::MediaKind::AudioChunk)?.finish()) }
pub fn audio_encoder_close(this: i32) { close(this,core::handles::Kind::MediaCodec); }
pub fn audio_encoder_configuration(this: i32) -> i32 { call(|m| m.insert_configs(session(m,this,pipe::MediaKind::Audio,pipe::MediaKind::AudioChunk)?.configuration()?)) }
pub fn video_encoder_write(this: i32, value: i32) -> bool { call(|m| session(m,this,pipe::MediaKind::Video,pipe::MediaKind::VideoChunk)?.write(pipe::Item::Video(m.video(value)?))) }
pub fn video_encoder_poll(this: i32) -> i32 { call(|m| { let s=session(m,this,pipe::MediaKind::Video,pipe::MediaKind::VideoChunk)?; Ok(status_out(media_lock(&s.output)?.status()?)) }) }
pub fn video_encoder_read(this: i32) -> i32 { call(|m| { let s=session(m,this,pipe::MediaKind::Video,pipe::MediaKind::VideoChunk)?; let item=media_lock(&s.output)?.take()?; retain_item(m,item) }) }
pub fn video_encoder_finish(this: i32) { call(|m| session(m,this,pipe::MediaKind::Video,pipe::MediaKind::VideoChunk)?.finish()) }
pub fn video_encoder_close(this: i32) { close(this,core::handles::Kind::MediaCodec); }
pub fn video_encoder_configuration(this: i32) -> i32 { call(|m| m.insert_configs(session(m,this,pipe::MediaKind::Video,pipe::MediaKind::VideoChunk)?.configuration()?)) }
pub fn audio_decoder_write(this: i32, value: i32) -> bool { call(|m| session(m,this,pipe::MediaKind::AudioChunk,pipe::MediaKind::Audio)?.write(pipe::Item::AudioChunk(m.audio_chunk(value)?))) }
pub fn audio_decoder_poll(this: i32) -> i32 { call(|m| { let s=session(m,this,pipe::MediaKind::AudioChunk,pipe::MediaKind::Audio)?; Ok(status_out(media_lock(&s.output)?.status()?)) }) }
pub fn audio_decoder_read(this: i32) -> i32 { call(|m| { let s=session(m,this,pipe::MediaKind::AudioChunk,pipe::MediaKind::Audio)?; let item=media_lock(&s.output)?.take()?; retain_item(m,item) }) }
pub fn audio_decoder_finish(this: i32) { call(|m| session(m,this,pipe::MediaKind::AudioChunk,pipe::MediaKind::Audio)?.finish()) }
pub fn audio_decoder_close(this: i32) { close(this,core::handles::Kind::MediaCodec); }
pub fn video_decoder_write(this: i32, value: i32) -> bool { call(|m| session(m,this,pipe::MediaKind::VideoChunk,pipe::MediaKind::Video)?.write(pipe::Item::VideoChunk(m.video_chunk(value)?))) }
pub fn video_decoder_poll(this: i32) -> i32 { call(|m| { let s=session(m,this,pipe::MediaKind::VideoChunk,pipe::MediaKind::Video)?; Ok(status_out(media_lock(&s.output)?.status()?)) }) }
pub fn video_decoder_read(this: i32) -> i32 { call(|m| { let s=session(m,this,pipe::MediaKind::VideoChunk,pipe::MediaKind::Video)?; let item=media_lock(&s.output)?.take()?; retain_item(m,item) }) }
pub fn video_decoder_finish(this: i32) { call(|m| session(m,this,pipe::MediaKind::VideoChunk,pipe::MediaKind::Video)?.finish()) }
pub fn video_decoder_close(this: i32) { close(this,core::handles::Kind::MediaCodec); }
pub fn mux_write_audio(this: i32, value: i32) -> bool { call(|m| m.writers(this)?.write(pipe::Item::AudioChunk(m.audio_chunk(value)?))) }
pub fn demux_has_audio(this: i32) -> bool { call(|m| { let reader=m.readers(this)?; reader.worker.check()?; let has=media_lock(&reader.configs)?[0].is_some(); Ok(has) }) }
pub fn demux_audio_configuration(this: i32) -> i32 { call(|m| { let reader=m.readers(this)?; reader.worker.check()?; let config=media_lock(&reader.configs)?[0].clone().ok_or_else(|| core::Error::invalid("track is absent"))?; m.insert_configs(config) }) }
pub fn demux_audio_status(this: i32) -> i32 { call(|m| { let reader=m.readers(this)?; reader.worker.check()?; Ok(status_out(media_lock(&reader.audio)?.status()?)) }) }
pub fn demux_read_audio(this: i32) -> i32 { call(|m| { let reader=m.readers(this)?; reader.worker.check()?; let item=media_lock(&reader.audio)?.take()?; retain_item(m,item) }) }
pub fn mux_write_video(this: i32, value: i32) -> bool { call(|m| m.writers(this)?.write(pipe::Item::VideoChunk(m.video_chunk(value)?))) }
pub fn demux_has_video(this: i32) -> bool { call(|m| { let reader=m.readers(this)?; reader.worker.check()?; let has=media_lock(&reader.configs)?[1].is_some(); Ok(has) }) }
pub fn demux_video_configuration(this: i32) -> i32 { call(|m| { let reader=m.readers(this)?; reader.worker.check()?; let config=media_lock(&reader.configs)?[1].clone().ok_or_else(|| core::Error::invalid("track is absent"))?; m.insert_configs(config) }) }
pub fn demux_video_status(this: i32) -> i32 { call(|m| { let reader=m.readers(this)?; reader.worker.check()?; Ok(status_out(media_lock(&reader.video)?.status()?)) }) }
pub fn demux_read_video(this: i32) -> i32 { call(|m| { let reader=m.readers(this)?; reader.worker.check()?; let item=media_lock(&reader.video)?.take()?; retain_item(m,item) }) }
