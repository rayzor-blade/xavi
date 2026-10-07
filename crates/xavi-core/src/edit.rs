//! Immutable audio/video edits. Returned media owns its storage; inputs stay valid.
use crate::*;

const MAX_EDIT_BYTES: usize = 64 * 1024 * 1024;
fn buffer(size: usize) -> Result<Vec<u8>> {
    if size > MAX_EDIT_BYTES {
        return Err(Error::invalid("edit exceeds the 64 MiB frame limit"));
    }
    let mut out = Vec::new();
    out.try_reserve_exact(size)
        .map_err(|_| Error::exhausted())?;
    out.resize(size, 0);
    Ok(out)
}
fn pcm(audio: &AudioData, offset: u32, count: u32) -> Result<Vec<u8>> {
    let options = AudioCopyOptions {
        frame_offset: offset,
        frame_count: Some(count),
        format: Some(AudioSampleFormat::F32),
        ..Default::default()
    };
    let mut bytes = buffer(audio.allocation_size(options)? as usize)?;
    audio.copy_to(&mut bytes, options)?;
    Ok(bytes)
}
impl AudioData {
    /// Extract samples and assign their position on the output timeline.
    pub fn slice(&self, offset: u32, count: u32, timestamp: i64) -> Result<Self> {
        let bytes = pcm(self, offset, count)?;
        Self::new(
            AudioDescriptor {
                format: AudioSampleFormat::F32,
                number_of_frames: count,
                timestamp,
                ..self.descriptor()
            },
            &bytes,
        )
    }
    pub fn retime(&self, timestamp: i64) -> Result<Self> {
        Self::new(
            AudioDescriptor {
                timestamp,
                ..self.descriptor()
            },
            self.bytes(),
        )
    }
    /// Gain and mix return interleaved f32 without clipping; integer conversion clips.
    pub fn gain(&self, gain: f64) -> Result<Self> {
        if !gain.is_finite() {
            return Err(Error::invalid("gain must be finite"));
        }
        let mut bytes = pcm(self, 0, self.descriptor().number_of_frames)?;
        for sample in bytes.as_chunks_mut::<4>().0 {
            let value = f32::from_ne_bytes(*sample) as f64 * gain;
            if !value.is_finite() || value.abs() > f32::MAX as f64 {
                return Err(Error::invalid("gain produces a non-finite sample"));
            }
            sample.copy_from_slice(&(value as f32).to_ne_bytes());
        }
        Self::new(
            AudioDescriptor {
                format: AudioSampleFormat::F32,
                ..self.descriptor()
            },
            &bytes,
        )
    }
    pub fn mix(&self, other: &Self, gain: f64) -> Result<Self> {
        let a = self.descriptor();
        let b = other.descriptor();
        if a.sample_rate != b.sample_rate
            || a.number_of_channels != b.number_of_channels
            || a.number_of_frames != b.number_of_frames
            || a.timestamp != b.timestamp
            || !gain.is_finite()
        {
            return Err(Error::invalid(
                "mix requires matching rate, channels, sample count and timestamp, and finite gain",
            ));
        }
        let mut left = pcm(self, 0, a.number_of_frames)?;
        let right = pcm(other, 0, b.number_of_frames)?;
        for (x, y) in left
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(right.as_chunks::<4>().0)
        {
            let value = f32::from_ne_bytes(*x) as f64 + f32::from_ne_bytes(*y) as f64 * gain;
            if !value.is_finite() || value.abs() > f32::MAX as f64 {
                return Err(Error::invalid("mix produces a non-finite sample"));
            }
            x.copy_from_slice(&(value as f32).to_ne_bytes());
        }
        Self::new(
            AudioDescriptor {
                format: AudioSampleFormat::F32,
                ..a
            },
            &left,
        )
    }
}
fn video_descriptor(info: VideoInfo) -> VideoDescriptor {
    VideoDescriptor {
        format: info.format,
        coded_width: info.coded_width,
        coded_height: info.coded_height,
        timestamp: info.timestamp,
        duration: info.duration,
        visible_rect: Some(info.visible_rect),
        display_width: Some(info.display_width),
        display_height: Some(info.display_height),
        color_space: info.color_space,
    }
}
impl VideoFrame {
    pub fn retime(&self, timestamp: i64, duration: Option<u64>) -> Result<Self> {
        let mut descriptor = video_descriptor(self.info());
        descriptor.timestamp = timestamp;
        descriptor.duration = duration;
        Self::new(descriptor, self.bytes(), Some(&self.layout().layouts()))
    }
    pub fn crop(&self, rect: Rect) -> Result<Self> {
        let options = VideoCopyOptions {
            rect: Some(rect),
            ..Default::default()
        };
        let mut bytes = buffer(self.allocation_size(&options)? as usize)?;
        self.copy_to(&mut bytes, &options)?;
        let mut descriptor = VideoDescriptor::new(
            self.info().format,
            rect.width,
            rect.height,
            self.info().timestamp,
        );
        descriptor.duration = self.info().duration;
        descriptor.color_space = self.info().color_space;
        Self::new(descriptor, &bytes, None)
    }
    /// Nearest-neighbor resize, independently sampling each pixel-format plane.
    /// Preserve format/color metadata; the result uses the full coded rectangle.
    pub fn resize(&self, width: u32, height: u32) -> Result<Self> {
        let info = self.info();
        let source = self.crop(info.visible_rect)?;
        let target = FrameLayout::new(info.format, width, height, None)?;
        let mut bytes = buffer(target.byte_len() as usize)?;
        for (index, &(_, _, sample_bytes)) in info.format.planes().iter().enumerate() {
            let src = source.layout().planes()[index];
            let dst = target.planes()[index];
            let unit = sample_bytes as usize;
            let sw = src.row_bytes as usize / unit;
            let dw = dst.row_bytes as usize / unit;
            for y in 0..dst.rows as usize {
                for x in 0..dw {
                    let from = src.layout.offset as usize
                        + (y * src.rows as usize / dst.rows as usize) * src.layout.stride as usize
                        + (x * sw / dw) * unit;
                    let to = dst.layout.offset as usize + y * dst.layout.stride as usize + x * unit;
                    bytes[to..to + unit].copy_from_slice(&source.bytes()[from..from + unit]);
                }
            }
        }
        let mut descriptor = VideoDescriptor::new(info.format, width, height, info.timestamp);
        descriptor.duration = info.duration;
        descriptor.color_space = info.color_space;
        Self::new(descriptor, &bytes, None)
    }
    /// Crossfade matching RGB frames; no implicit color-space conversion.
    pub fn blend(&self, other: &Self, opacity: f64) -> Result<Self> {
        let a = self.info();
        let b = other.info();
        if !opacity.is_finite()
            || !(0.0..=1.0).contains(&opacity)
            || a.format != b.format
            || a.visible_rect.width != b.visible_rect.width
            || a.visible_rect.height != b.visible_rect.height
            || a.color_space != b.color_space
            || !matches!(
                a.format,
                VideoPixelFormat::Rgba
                    | VideoPixelFormat::Bgra
                    | VideoPixelFormat::Rgbx
                    | VideoPixelFormat::Bgrx
            )
        {
            return Err(Error::invalid(
                "blend requires matching RGB frames and opacity in 0..=1",
            ));
        }
        let left = self.crop(a.visible_rect)?;
        let right = other.crop(b.visible_rect)?;
        let mut bytes = buffer(left.bytes().len())?;
        for ((out, x), y) in bytes.iter_mut().zip(left.bytes()).zip(right.bytes()) {
            *out = (f64::from(*x) * (1.0 - opacity) + f64::from(*y) * opacity).round() as u8;
        }
        Self::new(video_descriptor(left.info()), &bytes, None)
    }
}
