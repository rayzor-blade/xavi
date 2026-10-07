//! NDK MediaCodec byte-buffer path, API 28+. No Java VM or application context
//! is retained. Unknown vendor pixel layouts fail instead of being guessed.
use super::{NativeConfig, Output};
use crate::bitstream;
use ndk::media::media_codec::{
    DequeuedInputBufferResult as Input, DequeuedOutputBufferInfoResult as Read, MediaCodec,
    MediaCodecDirection,
};
use ndk::media::media_format::MediaFormat;
use std::time::Duration;
use xavi_core::codec::Receive;
use xavi_core::{Error, ErrorKind, Result};

fn failure(error: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::InvalidState, format!("MediaCodec: {error}"))
}
struct Pending {
    bytes: Vec<u8>,
    offset: usize,
    pts: u64,
    key: bool,
}
pub(crate) struct Backend {
    codec: MediaCodec,
    config: NativeConfig,
    pending: Option<Pending>,
    origin: Option<i64>,
    description: Vec<u8>,
    eos: bool,
    ended: bool,
    width: usize,
    height: usize,
    stride: usize,
    slice_height: usize,
    color: i32,
}
impl Backend {
    pub fn open(config: NativeConfig, description: &[u8]) -> Result<Self> {
        let encode = matches!(config.mode, 1 | 3);
        let audio = config.mode <= 2;
        let mime = if audio {
            "audio/mp4a-latm"
        } else {
            "video/avc"
        };
        let codec = (if encode {
            MediaCodec::from_encoder_type(mime)
        } else {
            MediaCodec::from_decoder_type(mime)
        })
        .ok_or_else(|| Error::unsupported("MediaCodec codec is unavailable"))?;
        let mut format = MediaFormat::new();
        format.set_str("mime", mime);
        if audio {
            format.set_i32("sample-rate", config.sample_rate as i32);
            format.set_i32("channel-count", config.channels as i32);
            format.set_i32("aac-profile", 2);
            format.set_i32("pcm-encoding", 2);
            if !encode {
                format.set_buffer("csd-0", description);
            }
        } else {
            if config.width > 0 {
                format.set_i32("width", config.width as i32);
                format.set_i32("height", config.height as i32);
            }
            format.set_i32("color-format", 21); // COLOR_FormatYUV420SemiPlanar
            if encode {
                format.set_f32("frame-rate", config.framerate as f32);
                format.set_i32("i-frame-interval", 4);
                format.set_i32("profile", 1);
                format.set_i32("level", 256); // AVC baseline, level 3
            } else {
                let (sps, pps) = bitstream::parameter_sets(description)?;
                format.set_buffer("csd-0", &[&[0, 0, 0, 1][..], sps].concat());
                format.set_buffer("csd-1", &[&[0, 0, 0, 1][..], pps].concat());
            }
        }
        if encode {
            format.set_i32("bitrate", config.bitrate as i32);
        }
        codec
            .configure(
                &format,
                None,
                if encode {
                    MediaCodecDirection::Encoder
                } else {
                    MediaCodecDirection::Decoder
                },
            )
            .map_err(|e| Error::unsupported(format!("MediaCodec configuration: {e}")))?;
        codec.start().map_err(failure)?;
        Ok(Self {
            codec,
            width: config.width as usize,
            height: config.height as usize,
            stride: config.width as usize,
            slice_height: config.height as usize,
            color: 21,
            config,
            pending: None,
            origin: None,
            description: Vec::new(),
            eos: false,
            ended: false,
        })
    }
    #[allow(clippy::too_many_arguments)]
    pub fn send(
        &mut self,
        bytes: &[u8],
        timestamp: i64,
        _: u64,
        _: u32,
        format: u32,
        key: bool,
    ) -> Result<bool> {
        if self.pending.is_some() {
            return Ok(false);
        }
        if self.eos {
            return Err(failure("input after EOS"));
        }
        let origin = *self.origin.get_or_insert(timestamp);
        let pts = u64::try_from(i128::from(timestamp) - i128::from(origin))
            .map_err(|_| Error::invalid("timestamp precedes the segment origin"))?;
        let bytes = match self.config.mode {
            3 => crate::pixels::nv12(bytes, format, self.config.width, self.config.height)?,
            4 => bitstream::to_annex_b(bytes)?,
            _ => bytes.to_vec(),
        };
        if self.config.mode == 3 && key {
            let mut parameters = MediaFormat::new();
            parameters.set_i32("request-sync", 0);
            self.codec.set_parameters(parameters).map_err(failure)?;
        }
        self.pending = Some(Pending {
            bytes,
            offset: 0,
            pts,
            key,
        });
        self.feed()?;
        Ok(true)
    }
    fn feed(&mut self) -> Result<()> {
        let Some(pending) = &mut self.pending else {
            return Ok(());
        };
        let Input::Buffer(mut buffer) = self
            .codec
            .dequeue_input_buffer(Duration::ZERO)
            .map_err(failure)?
        else {
            return Ok(());
        };
        let capacity = buffer.buffer_mut().len();
        let remaining = pending.bytes.len() - pending.offset;
        let stride = (self.config.channels * 2) as usize;
        let len = if self.config.mode == 1 {
            remaining.min((capacity / stride) * stride)
        } else {
            remaining
        };
        if len == 0 || len > capacity {
            return Err(Error::unsupported(
                "MediaCodec input access unit exceeds its native buffer",
            ));
        }
        for (target, source) in buffer.buffer_mut()[..len]
            .iter_mut()
            .zip(&pending.bytes[pending.offset..pending.offset + len])
        {
            target.write(*source);
        }
        let pts = pending
            .pts
            .checked_add(if self.config.mode == 1 {
                (((pending.offset / stride) as u64) * 1_000_000)
                    / u64::from(self.config.sample_rate)
            } else {
                0
            })
            .filter(|pts| *pts <= i64::MAX as u64)
            .ok_or_else(|| Error::invalid("MediaCodec timestamp overflow"))?;
        self.codec
            .queue_input_buffer(buffer, 0, len, pts, u32::from(pending.key))
            .map_err(failure)?;
        pending.offset += len;
        if pending.offset == pending.bytes.len() {
            self.pending = None;
        }
        Ok(())
    }
    fn changed(&mut self) -> Result<()> {
        let format = self.codec.output_format();
        if self.config.mode == 2
            && (format.i32("sample-rate") != Some(self.config.sample_rate as i32)
                || format.i32("channel-count") != Some(self.config.channels as i32)
                || format.i32("pcm-encoding").unwrap_or(2) != 2)
        {
            return Err(Error::unsupported(
                "MediaCodec changed the configured PCM format",
            ));
        }
        if self.config.mode == 3 {
            let mut bytes = format.buffer("csd-0").unwrap_or(&[]).to_vec();
            bytes.extend_from_slice(format.buffer("csd-1").unwrap_or(&[]));
            self.description = bitstream::description(&bytes)?;
        }
        if self.config.mode == 4 {
            self.width = format
                .i32("width")
                .and_then(|v| usize::try_from(v).ok())
                .unwrap_or(0);
            self.height = format
                .i32("height")
                .and_then(|v| usize::try_from(v).ok())
                .unwrap_or(0);
            self.stride = format
                .i32("stride")
                .and_then(|v| usize::try_from(v).ok())
                .unwrap_or(self.width)
                .max(self.width);
            self.slice_height = format
                .i32("slice-height")
                .and_then(|v| usize::try_from(v).ok())
                .unwrap_or(self.height)
                .max(self.height);
            self.color = format.i32("color-format").unwrap_or(0);
            if format.i32("crop-left").unwrap_or(0) != 0
                || format.i32("crop-top").unwrap_or(0) != 0
                || format
                    .i32("crop-right")
                    .is_some_and(|v| v != (self.width as i32) - 1)
                || format
                    .i32("crop-bottom")
                    .is_some_and(|v| v != (self.height as i32) - 1)
            {
                return Err(Error::unsupported(
                    "MediaCodec cropped output is not supported by this byte-buffer profile",
                ));
            }
            if !matches!(self.color, 19 | 21) {
                return Err(Error::unsupported(
                    "MediaCodec returned an opaque or vendor YUV layout",
                ));
            }
        }
        Ok(())
    }
    pub fn receive(&mut self) -> Result<Receive<Output>> {
        if self.ended {
            return Ok(Receive::End);
        }
        for _ in 0..8 {
            match self
                .codec
                .dequeue_output_buffer(Duration::ZERO)
                .map_err(failure)?
            {
                Read::TryAgainLater => {
                    self.feed()?;
                    return Ok(Receive::Pending);
                }
                Read::OutputFormatChanged => {
                    self.changed()?;
                }
                Read::OutputBuffersChanged => {}
                Read::Buffer(buffer) => {
                    let info = *buffer.info();
                    let result = (|| {
                        let start = usize::try_from(info.offset()).map_err(failure)?;
                        let len = usize::try_from(info.size()).map_err(failure)?;
                        // EOS may have no backing output buffer at all.
                        if len == 0 {
                            return Ok(Vec::new());
                        }
                        if len > crate::MAX_BYTES {
                            return Err(Error::exhausted());
                        }
                        let data = buffer
                            .buffer()
                            .get(start..start.checked_add(len).ok_or_else(Error::exhausted)?)
                            .ok_or_else(|| failure("invalid output span"))?
                            .to_vec();
                        Ok(data)
                    })();
                    self.codec
                        .release_output_buffer(buffer, false)
                        .map_err(failure)?;
                    let data = result?;
                    if (info.flags() & 4) != 0 {
                        self.ended = true;
                    }
                    if (info.flags() & 2) != 0 {
                        if self.config.mode == 3 {
                            self.description = bitstream::description(&data)?;
                        }
                        continue;
                    }
                    if data.is_empty() {
                        if self.ended {
                            return Ok(Receive::End);
                        }
                        continue;
                    }
                    let timestamp = i64::try_from(
                        i128::from(self.origin.unwrap_or(0))
                            + i128::from(info.presentation_time_us()),
                    )
                    .map_err(failure)?;
                    let mut o = Output {
                        bytes: data,
                        timestamp,
                        key: (info.flags() & 1) != 0,
                        ..Default::default()
                    };
                    match self.config.mode {
                        1 => {
                            o.duration = (1024 * 1_000_000) / u64::from(self.config.sample_rate);
                        }
                        2 => {
                            o.frames =
                                (o.bytes.len() / ((self.config.channels as usize) * 2)) as u32;
                            o.sample_rate = self.config.sample_rate;
                            o.channels = self.config.channels;
                            o.format = 1;
                        }
                        3 => {
                            o.bytes = bitstream::from_annex_b(&o.bytes)?;
                            o.description = std::mem::take(&mut self.description);
                        }
                        4 => {
                            o.bytes = self.pack_yuv(&o.bytes)?;
                            o.width = self.width as u32;
                            o.height = self.height as u32;
                            o.format = 3;
                        }
                        _ => unreachable!(),
                    }
                    return Ok(Receive::Output(o));
                }
            }
        }
        Ok(Receive::Pending)
    }
    fn pack_yuv(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        let (w, h, s, sh) = (self.width, self.height, self.stride, self.slice_height);
        if w == 0
            || h == 0
            || !w.is_multiple_of(2)
            || !h.is_multiple_of(2)
            || !s.is_multiple_of(2)
            || !sh.is_multiple_of(2)
            || s.checked_mul(sh)
                .is_none_or(|n| n > (crate::MAX_BYTES * 2) / 3 || (n * 3) / 2 > bytes.len())
        {
            return Err(failure("invalid YUV output layout"));
        }
        let mut out = vec![0; w * h * 3 / 2];
        for row in 0..h {
            out[row * w..(row + 1) * w].copy_from_slice(&bytes[row * s..row * s + w]);
        }
        for row in 0..h / 2 {
            for x in 0..w / 2 {
                let (u, v) = if self.color == 21 {
                    let p = s * sh + row * s + x * 2;
                    (bytes[p], bytes[p + 1])
                } else {
                    let p = s * sh + row * (s / 2) + x;
                    (bytes[p], bytes[p + (s * sh) / 4])
                };
                let p = w * h + row * w + x * 2;
                out[p] = u;
                out[p + 1] = v;
            }
        }
        Ok(out)
    }
    pub fn drain(&mut self) -> Result<bool> {
        if self.eos {
            return Ok(true);
        }
        self.feed()?;
        if self.pending.is_some() {
            return Ok(false);
        }
        let Input::Buffer(buffer) = self
            .codec
            .dequeue_input_buffer(Duration::ZERO)
            .map_err(failure)?
        else {
            return Ok(false);
        };
        self.codec
            .queue_input_buffer(buffer, 0, 0, 0, 4)
            .map_err(failure)?;
        self.eos = true;
        Ok(true)
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        let _ = self.codec.stop();
    }
}
