use std::sync::Arc;

use crate::format::SampleType;
use crate::{AudioSampleFormat, Error, Result, byte_len, snapshot};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioDescriptor {
    pub format: AudioSampleFormat,
    pub sample_rate: f32,
    pub number_of_frames: u32,
    pub number_of_channels: u32,
    pub timestamp: i64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AudioCopyOptions {
    pub plane_index: u32,
    pub frame_offset: u32,
    pub frame_count: Option<u32>,
    pub format: Option<AudioSampleFormat>,
}

/// Immutable, tightly packed PCM. Clone shares bytes; it does not copy samples.
#[derive(Clone, Debug)]
pub struct AudioData {
    descriptor: AudioDescriptor,
    duration: u64,
    data: Arc<[u8]>,
}

struct CopyPlan {
    format: AudioSampleFormat,
    frames: u32,
    channels: u32,
    bytes: u32,
}

impl AudioData {
    pub fn new(descriptor: AudioDescriptor, data: &[u8]) -> Result<Self> {
        let d = descriptor;
        if !d.sample_rate.is_finite()
            || d.sample_rate <= 0.0
            || d.number_of_frames == 0
            || d.number_of_channels == 0
        {
            return Err(Error::invalid(
                "sample rate, frame count and channel count must be positive and finite",
            ));
        }
        let samples = u64::from(d.number_of_frames) * u64::from(d.number_of_channels);
        let bytes = samples
            .checked_mul(u64::from(d.format.bytes_per_sample()))
            .ok_or_else(|| Error::invalid("audio sample size overflow"))?;
        let bytes = byte_len(bytes)? as usize;
        if data.len() < bytes {
            return Err(Error::invalid("audio input buffer is too small"));
        }
        let duration =
            (f64::from(d.number_of_frames) * 1_000_000.0 / f64::from(d.sample_rate)).floor();
        // u64::MAX rounds to 2^64 as f64, which is already outside u64.
        if !duration.is_finite() || duration >= u64::MAX as f64 {
            return Err(Error::invalid(
                "audio duration exceeds the microsecond range",
            ));
        }
        Ok(Self {
            descriptor,
            duration: duration as u64,
            data: snapshot(&data[..bytes])?,
        })
    }

    pub fn descriptor(&self) -> AudioDescriptor {
        self.descriptor
    }
    pub fn duration(&self) -> u64 {
        self.duration
    }
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    pub fn allocation_size(&self, options: AudioCopyOptions) -> Result<u32> {
        Ok(self.plan(options)?.bytes)
    }

    /// Converts PCM representation and channel arrangement; never resamples.
    /// Integer conversion rounds to the nearest value and saturates; NaN becomes
    /// silence for integer output. Same sample types copy their bits exactly,
    /// including float NaN payloads, signed zero and values outside [-1, 1].
    /// All validation happens before the first destination byte is written.
    pub fn copy_to(&self, destination: &mut [u8], options: AudioCopyOptions) -> Result<()> {
        let plan = self.plan(options)?;
        if destination.len() < plan.bytes as usize {
            return Err(Error::invalid("audio destination buffer is too small"));
        }
        let d = self.descriptor;
        let in_size = d.format.bytes_per_sample() as usize;
        let out_size = plan.format.bytes_per_sample() as usize;
        for frame in 0..plan.frames as usize {
            for output_channel in 0..plan.channels as usize {
                let channel = if plan.format.is_planar() {
                    options.plane_index as usize
                } else {
                    output_channel
                };
                let source_frame = options.frame_offset as usize + frame;
                let sample = if d.format.is_planar() {
                    channel * d.number_of_frames as usize + source_frame
                } else {
                    source_frame * d.number_of_channels as usize + channel
                };
                let input = &self.data[sample * in_size..(sample + 1) * in_size];
                let out = (frame * plan.channels as usize + output_channel) * out_size;
                let output = &mut destination[out..out + out_size];
                if d.format.sample_type() == plan.format.sample_type() {
                    output.copy_from_slice(input);
                } else {
                    write_sample(
                        output,
                        plan.format.sample_type(),
                        read_sample(input, d.format.sample_type()),
                    );
                }
            }
        }
        Ok(())
    }

    fn plan(&self, options: AudioCopyOptions) -> Result<CopyPlan> {
        let d = self.descriptor;
        let format = options.format.unwrap_or(d.format);
        let planes = if format.is_planar() {
            d.number_of_channels
        } else {
            1
        };
        if options.plane_index >= planes {
            return Err(Error::invalid(
                "audio plane index is outside the output format",
            ));
        }
        if options.frame_offset >= d.number_of_frames {
            return Err(Error::invalid("audio frame offset is outside the source"));
        }
        let remaining = d.number_of_frames - options.frame_offset;
        let frames = options.frame_count.unwrap_or(remaining);
        if frames > remaining {
            return Err(Error::invalid("audio copy range exceeds the source"));
        }
        let channels = if format.is_planar() {
            1
        } else {
            d.number_of_channels
        };
        let bytes = u64::from(frames) * u64::from(channels);
        let bytes = bytes
            .checked_mul(u64::from(format.bytes_per_sample()))
            .ok_or_else(|| Error::invalid("audio copy size overflow"))?;
        Ok(CopyPlan {
            format,
            frames,
            channels,
            bytes: byte_len(bytes)?,
        })
    }
}

fn read_sample(bytes: &[u8], format: SampleType) -> f64 {
    match format {
        SampleType::U8 => (f64::from(bytes[0]) - 128.0) / 128.0,
        SampleType::S16 => f64::from(i16::from_ne_bytes(bytes.try_into().unwrap())) / 32768.0,
        SampleType::S32 => f64::from(i32::from_ne_bytes(bytes.try_into().unwrap())) / 2147483648.0,
        SampleType::F32 => f64::from(f32::from_ne_bytes(bytes.try_into().unwrap())),
    }
}

fn write_sample(bytes: &mut [u8], format: SampleType, value: f64) {
    let integer_value = if value.is_nan() {
        0.0
    } else {
        value.clamp(-1.0, 1.0)
    };
    match format {
        SampleType::U8 => {
            bytes[0] = (integer_value * 128.0 + 128.0).round().clamp(0.0, 255.0) as u8
        }
        SampleType::S16 => {
            bytes.copy_from_slice(&((integer_value * 32768.0).round() as i16).to_ne_bytes())
        }
        SampleType::S32 => {
            bytes.copy_from_slice(&((integer_value * 2147483648.0).round() as i32).to_ne_bytes())
        }
        SampleType::F32 => bytes.copy_from_slice(&(value as f32).to_ne_bytes()),
    }
}
