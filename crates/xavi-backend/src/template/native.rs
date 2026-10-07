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
