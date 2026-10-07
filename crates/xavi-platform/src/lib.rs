//! Codecs implemented by OS frameworks. No FFmpeg code is linked or selected.
//! The first profile is AAC-LC (`mp4a.40.2`) and H.264 Baseline level 3
//! (`avc1.42001E`). H.264 packets use four-byte NAL lengths and avcC description;
//! AAC packets are raw access units with AudioSpecificConfig, not ADTS.
//!
//! Construct and drive codecs on the same worker thread. The Apple backend
//! completes VideoToolbox callbacks before each native call returns. Audio
//! accepts incremental PCM and retains the converter's history across inputs.
//! AAC output includes converter priming/padding; gapless container trimming is
//! not implemented. [`mux`] writes encoded output to native MP4 containers;
//! codec and muxer bindings to VMs still require host-worker integration.
//!
//! Backends: AudioToolbox/VideoToolbox on macOS and iOS, Media Foundation on
//! Windows, NDK MediaCodec on Android API 28+, and system GStreamer 1.20+ on
//! Linux. Linux needs appsrc/appsink, audio/video conversion and parsers, plus
//! voaacenc, faad or fdkaacdec, openh264enc, and a VA or OpenH264 decoder. The
//! initial Linux encoder emits intra frames to honor arbitrary key requests.
//! Missing plugins/codecs are reported as unsupported. Codec GStreamer pipelines use
//! explicit factories; playback filters gst-libav before autoplug negotiation. Any future FFmpeg backend must
//! be a separate opt-in dependency and must not be an automatic fallback.
//!
//! Support probes open a native context; asynchronous negotiation and device
//! failures can still occur while pumping. Byte-buffer video is SDR; explicit
//! encoder color metadata, cropping and opaque GPU/vendor layouts are currently
//! rejected. Decoders return BGRA or NV12, according to the platform.
//! PCM and AAC timestamps must be contiguous within a segment; flush before a
//! discontinuity. Video encoder timestamps must strictly increase.

use std::sync::Arc;
use xavi_core::codec::*;
use xavi_core::*;

mod bitstream;
pub mod mux;
mod native;
#[cfg(any(test, target_os = "android", target_os = "windows"))]
mod pixels;
pub mod player;
use native::{Native, NativeConfig};

pub const AAC: &str = "mp4a.40.2";
pub const H264: &str = "avc1.42001E";
const MAX_BYTES: usize = 64 * 1024 * 1024;

fn invalid(message: &str) -> Error {
    Error::invalid(message)
}
fn audio_config(codec: &str, rate: u32, channels: u32) -> Result<[u8; 2]> {
    if codec.is_empty() || rate == 0 || channels == 0 {
        return Err(invalid("codec, rate and channels are required"));
    }
    if codec != AAC || !(1..=2).contains(&channels) {
        return Err(Error::unsupported(
            "platform audio profile supports AAC-LC mono/stereo",
        ));
    }
    let rates = [
        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000,
    ];
    let index = rates
        .iter()
        .position(|r| *r == rate)
        .ok_or_else(|| Error::unsupported("unsupported AAC sample rate"))? as u8;
    Ok([0x10 | (index >> 1), (index << 7) | ((channels as u8) << 3)])
}
fn video_config(codec: &str, width: u32, height: u32) -> Result<()> {
    if codec.is_empty() || width == 0 || height == 0 {
        return Err(invalid("codec and positive dimensions are required"));
    }
    if codec != H264 {
        return Err(Error::unsupported(
            "platform encoder profile is H.264 Baseline level 3",
        ));
    }
    if !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
        || u64::from(width) * u64::from(height) > (MAX_BYTES / 4) as u64
    {
        return Err(Error::unsupported(
            "video dimensions must be even and fit the platform frame limit",
        ));
    }
    Ok(())
}
fn chunk_valid(chunk: &EncodedChunk) -> Result<()> {
    if chunk.bytes().is_empty() || chunk.bytes().len() > MAX_BYTES {
        return Err(invalid("encoded access unit is empty or too large"));
    }
    Ok(())
}

pub struct AudioEncoder {
    native: Native,
    config: AudioEncoderConfig,
    metadata: Option<AudioDecoderConfig>,
    origin: Option<i64>,
    input_frames: u64,
}
impl Engine for AudioEncoder {
    type Config = AudioEncoderConfig;
    type Input = AudioInput;
    type Output = EncodedOutput<AudioDecoderConfig>;
    fn open(config: &Self::Config) -> Result<Self> {
        let description = audio_config(&config.codec, config.sample_rate, config.channels)?;
        let bitrate = config.bitrate.unwrap_or(128_000);
        if bitrate == 0 || bitrate > i32::MAX as u64 {
            return Err(invalid("audio bitrate must fit a positive i32"));
        }
        let native = Native::open(
            NativeConfig {
                mode: 1,
                sample_rate: config.sample_rate,
                channels: config.channels,
                bitrate,
                ..Default::default()
            },
            &[],
        )?;
        Ok(Self {
            native,
            config: config.clone(),
            metadata: Some(AudioDecoderConfig {
                codec: AAC.into(),
                sample_rate: config.sample_rate,
                channels: config.channels,
                description: description.into(),
            }),
            origin: None,
            input_frames: 0,
        })
    }
    fn validate(&self, input: &Self::Input) -> Result<()> {
        let d = input.descriptor();
        if d.sample_rate != self.config.sample_rate as f32
            || d.number_of_channels != self.config.channels
        {
            return Err(invalid(
                "audio input must match the configured rate and channels",
            ));
        }
        if input.bytes().len() > MAX_BYTES {
            return Err(invalid("audio input exceeds the platform limit"));
        }
        Ok(())
    }
    fn send(&mut self, input: &Self::Input) -> Result<bool> {
        self.validate(input)?;
        let d = input.descriptor();
        if let Some(origin) = self.origin {
            let expected = i128::from(origin)
                + i128::from(self.input_frames) * 1_000_000 / i128::from(self.config.sample_rate);
            if (i128::from(d.timestamp) - expected).abs() > 1 {
                return Err(invalid(
                    "audio timestamps must be contiguous; flush before a discontinuity",
                ));
            }
        }
        let options = AudioCopyOptions {
            format: Some(AudioSampleFormat::S16),
            ..Default::default()
        };
        let len = input.allocation_size(options)? as usize;
        if len > MAX_BYTES {
            return Err(invalid("converted audio exceeds the platform limit"));
        }
        let mut bytes = vec![0; len];
        input.copy_to(&mut bytes, options)?;
        if !self.native.send(
            &bytes,
            d.timestamp,
            input.duration(),
            d.number_of_frames,
            1,
            true,
        )? {
            return Ok(false);
        }
        self.origin.get_or_insert(d.timestamp);
        self.input_frames = self
            .input_frames
            .checked_add(u64::from(d.number_of_frames))
            .ok_or_else(Error::exhausted)?;
        Ok(true)
    }
    fn receive(&mut self) -> Result<Receive<Self::Output>> {
        self.native.receive()?.map(|o| {
            Ok(EncodedOutput {
                chunk: Arc::new(EncodedChunk::new(
                    EncodedChunkType::Key,
                    o.timestamp,
                    Some(o.duration),
                    &o.bytes,
                )?),
                decoder_config: self.metadata.take(),
            })
        })
    }
    fn drain(&mut self) -> Result<bool> {
        self.native.drain()
    }
}

pub struct AudioDecoder {
    native: Native,
    config: AudioDecoderConfig,
    origin: Option<i64>,
    input_packets: u64,
}
impl Engine for AudioDecoder {
    type Config = AudioDecoderConfig;
    type Input = ChunkInput;
    type Output = Arc<AudioData>;
    fn open(config: &Self::Config) -> Result<Self> {
        let asc = audio_config(&config.codec, config.sample_rate, config.channels)?;
        if config.description.as_ref() != asc {
            return Err(invalid(
                "AAC description must match the configured AAC-LC rate and channels",
            ));
        }
        let native = Native::open(
            NativeConfig {
                mode: 2,
                sample_rate: config.sample_rate,
                channels: config.channels,
                ..Default::default()
            },
            &config.description,
        )?;
        Ok(Self {
            native,
            config: config.clone(),
            origin: None,
            input_packets: 0,
        })
    }
    fn validate(&self, input: &Self::Input) -> Result<()> {
        chunk_valid(input)
    }
    fn send(&mut self, input: &Self::Input) -> Result<bool> {
        self.validate(input)?;
        if let Some(origin) = self.origin {
            let expected = i128::from(origin)
                + i128::from(self.input_packets) * 1024 * 1_000_000
                    / i128::from(self.config.sample_rate);
            if (i128::from(input.timestamp()) - expected).abs() > 1 {
                return Err(invalid(
                    "AAC timestamps must be contiguous; flush before a discontinuity",
                ));
            }
        }
        let accepted = self.native.send(
            input.bytes(),
            input.timestamp(),
            input.duration().unwrap_or(0),
            0,
            0,
            input.kind() == EncodedChunkType::Key,
        )?;
        if accepted {
            self.origin.get_or_insert(input.timestamp());
            self.input_packets = self
                .input_packets
                .checked_add(1)
                .ok_or_else(Error::exhausted)?;
        }
        Ok(accepted)
    }
    fn receive(&mut self) -> Result<Receive<Self::Output>> {
        self.native.receive()?.map(|o| {
            if o.format != 1
                || o.sample_rate != self.config.sample_rate
                || o.channels != self.config.channels
            {
                return Err(Error::unsupported(
                    "decoder changed the configured PCM format",
                ));
            }
            Ok(Arc::new(AudioData::new(
                AudioDescriptor {
                    format: AudioSampleFormat::S16,
                    sample_rate: o.sample_rate as f32,
                    number_of_frames: o.frames,
                    number_of_channels: o.channels,
                    timestamp: o.timestamp,
                },
                &o.bytes,
            )?))
        })
    }
    fn drain(&mut self) -> Result<bool> {
        self.native.drain()
    }
}

pub struct VideoEncoder {
    native: Native,
    config: VideoEncoderConfig,
    last_pts: Option<i64>,
    need_metadata: bool,
}
impl Engine for VideoEncoder {
    type Config = VideoEncoderConfig;
    type Input = VideoEncodeInput;
    type Output = EncodedOutput<VideoDecoderConfig>;
    fn open(config: &Self::Config) -> Result<Self> {
        video_config(&config.codec, config.width, config.height)?;
        if config.bitrate == 0
            || config.bitrate > i32::MAX as u64
            || !config.framerate.is_finite()
            || config.framerate <= 0.0
            || config.framerate > 240.0
        {
            return Err(invalid("invalid video bitrate or frame rate"));
        }
        let native = Native::open(
            NativeConfig {
                mode: 3,
                width: config.width,
                height: config.height,
                bitrate: config.bitrate,
                framerate: config.framerate,
                ..Default::default()
            },
            &[],
        )?;
        Ok(Self {
            native,
            config: config.clone(),
            last_pts: None,
            need_metadata: true,
        })
    }
    fn validate(&self, input: &Self::Input) -> Result<()> {
        let i = input.frame.info();
        if i.coded_width != self.config.width
            || i.coded_height != self.config.height
            || i.visible_rect
                != (Rect {
                    x: 0,
                    y: 0,
                    width: i.coded_width,
                    height: i.coded_height,
                })
        {
            return Err(invalid(
                "encoder input must match configured dimensions and use the full coded rectangle",
            ));
        }
        if i.display_width != i.coded_width
            || i.display_height != i.coded_height
            || i.color_space != VideoColorSpace::default()
        {
            return Err(Error::unsupported(
                "display aspect and explicit color metadata are not yet supported by this encoder profile",
            ));
        }
        if !matches!(
            i.format,
            VideoPixelFormat::Bgra
                | VideoPixelFormat::Rgba
                | VideoPixelFormat::Nv12
                | VideoPixelFormat::I420
        ) {
            return Err(Error::unsupported(
                "platform video input supports BGRA, RGBA, NV12 or I420",
            ));
        }
        Ok(())
    }
    fn send(&mut self, input: &Self::Input) -> Result<bool> {
        self.validate(input)?;
        let i = input.frame.info();
        if self.last_pts.is_some_and(|pts| i.timestamp <= pts) {
            return Err(invalid("encoder timestamps must increase within a segment"));
        }
        let options = VideoCopyOptions::default();
        let mut bytes = vec![0; input.frame.allocation_size(&options)? as usize];
        input.frame.copy_to(&mut bytes, &options)?;
        let format = match i.format {
            VideoPixelFormat::Bgra => 2,
            VideoPixelFormat::Rgba => {
                for p in bytes.as_chunks_mut::<4>().0 {
                    p.swap(0, 2);
                }
                2
            }
            VideoPixelFormat::Nv12 => 3,
            VideoPixelFormat::I420 => {
                let y = (i.coded_width as usize) * (i.coded_height as usize);
                let uv = bytes[y..].to_vec();
                for (index, pair) in bytes[y..].as_chunks_mut::<2>().0.iter_mut().enumerate() {
                    pair[0] = uv[index];
                    pair[1] = uv[uv.len() / 2 + index];
                }
                3
            }
            _ => unreachable!(),
        };
        let accepted = self.native.send(
            &bytes,
            i.timestamp,
            i.duration.unwrap_or(0),
            0,
            format,
            input.key_frame,
        )?;
        if accepted {
            self.last_pts = Some(i.timestamp);
        }
        Ok(accepted)
    }
    fn receive(&mut self) -> Result<Receive<Self::Output>> {
        self.native.receive()?.map(|o| {
            if self.need_metadata && (o.description.is_empty() || !o.key) {
                return Err(Error::new(
                    ErrorKind::InvalidState,
                    "platform did not emit an initial AVC key chunk and configuration",
                ));
            }
            let decoder_config = if o.description.is_empty() {
                None
            } else {
                bitstream::parameter_sets(&o.description)?;
                self.need_metadata = false;
                Some(VideoDecoderConfig {
                    codec: format!(
                        "avc1.{:02X}{:02X}{:02X}",
                        o.description[1], o.description[2], o.description[3]
                    ),
                    coded_width: Some(self.config.width),
                    coded_height: Some(self.config.height),
                    description: o.description.into(),
                    color_space: VideoColorSpace::default(),
                })
            };
            Ok(EncodedOutput {
                chunk: Arc::new(EncodedChunk::new(
                    if o.key {
                        EncodedChunkType::Key
                    } else {
                        EncodedChunkType::Delta
                    },
                    o.timestamp,
                    (o.duration > 0).then_some(o.duration),
                    &o.bytes,
                )?),
                decoder_config,
            })
        })
    }
    fn drain(&mut self) -> Result<bool> {
        self.native.drain()
    }
}

pub struct VideoDecoder {
    native: Native,
    need_key: bool,
    config: VideoDecoderConfig,
}
impl Engine for VideoDecoder {
    type Config = VideoDecoderConfig;
    type Input = ChunkInput;
    type Output = Arc<VideoFrame>;
    fn open(config: &Self::Config) -> Result<Self> {
        if config.codec.is_empty() {
            return Err(invalid("codec is required"));
        }
        if !config.codec.starts_with("avc1.") || config.codec.len() != 11 {
            return Err(Error::unsupported(
                "platform video decoder profile expects AVC configuration",
            ));
        }
        let profile = u32::from_str_radix(&config.codec[5..], 16)
            .map_err(|_| invalid("malformed AVC codec string"))?;
        if profile >> 16 != 66 {
            return Err(Error::unsupported(
                "initial video decoder profile supports H.264 Baseline",
            ));
        }
        if config.description.len() < 11 || config.description.len() > MAX_BYTES {
            return Err(invalid("AVC description is missing or invalid"));
        }
        let d = &config.description;
        bitstream::parameter_sets(d)?;
        if profile != u32::from_be_bytes([0, d[1], d[2], d[3]]) {
            return Err(invalid("AVC description and codec string disagree"));
        }
        if config.coded_width.is_some() != config.coded_height.is_some()
            || config.coded_width == Some(0)
            || config.coded_height == Some(0)
        {
            return Err(invalid(
                "coded dimensions must both be positive or both omitted",
            ));
        }
        if config
            .coded_width
            .zip(config.coded_height)
            .is_some_and(|(w, h)| u64::from(w) * u64::from(h) > (MAX_BYTES / 4) as u64)
        {
            return Err(Error::unsupported(
                "coded dimensions exceed the platform limit",
            ));
        }
        let native = Native::open(
            NativeConfig {
                mode: 4,
                width: config.coded_width.unwrap_or(0),
                height: config.coded_height.unwrap_or(0),
                ..Default::default()
            },
            &config.description,
        )?;
        Ok(Self {
            native,
            need_key: true,
            config: config.clone(),
        })
    }
    fn validate(&self, input: &Self::Input) -> Result<()> {
        chunk_valid(input)
    }
    fn send(&mut self, input: &Self::Input) -> Result<bool> {
        self.validate(input)?;
        if self.need_key && input.kind() != EncodedChunkType::Key {
            return Err(invalid(
                "decoder requires a key chunk after configure or flush",
            ));
        }
        let accepted = self.native.send(
            input.bytes(),
            input.timestamp(),
            input.duration().unwrap_or(0),
            0,
            0,
            input.kind() == EncodedChunkType::Key,
        )?;
        if accepted {
            self.need_key = false;
        }
        Ok(accepted)
    }
    fn receive(&mut self) -> Result<Receive<Self::Output>> {
        self.native.receive()?.map(|o| {
            let format = match o.format {
                2 => VideoPixelFormat::Bgra,
                3 => VideoPixelFormat::Nv12,
                _ => return Err(Error::unsupported("unsupported decoded pixel format")),
            };
            let mut d = VideoDescriptor::new(format, o.width, o.height, o.timestamp);
            d.duration = (o.duration > 0).then_some(o.duration);
            d.color_space = self.config.color_space;
            Ok(Arc::new(VideoFrame::new(d, &o.bytes, None)?))
        })
    }
    fn drain(&mut self) -> Result<bool> {
        self.native.drain()
    }
}

trait MapOutput<T> {
    fn map<U>(self, f: impl FnOnce(T) -> Result<U>) -> Result<Receive<U>>;
}
impl<T> MapOutput<T> for Receive<T> {
    fn map<U>(self, f: impl FnOnce(T) -> Result<U>) -> Result<Receive<U>> {
        Ok(match self {
            Receive::Output(value) => Receive::Output(f(value)?),
            Receive::Pending => Receive::Pending,
            Receive::End => Receive::End,
        })
    }
}
