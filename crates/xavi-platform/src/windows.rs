//! Microsoft Media Foundation transforms. The initial byte-buffer path selects
//! the OS's synchronous AAC/H.264 transforms, not third-party registered MFTs.
//! COM and MF lifetimes are tied to the codec's creating worker thread.
use super::{NativeConfig, Output};
use crate::bitstream;
use std::mem::ManuallyDrop;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Variant::VARIANT;
use windows::core::{GUID, Interface};
use xavi_core::codec::Receive;
use xavi_core::{Error, ErrorKind, Result};

fn failure(e: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::InvalidState, format!("Media Foundation: {e}"))
}
struct Runtime;
impl Runtime {
    fn new() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(failure)?;
            if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                CoUninitialize();
                return Err(failure(e));
            }
            Ok(Self)
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}
pub(crate) struct Backend {
    transform: IMFTransform,
    output_type: IMFMediaType,
    config: NativeConfig,
    description: Vec<u8>,
    draining: bool,
    // Declared last: COM interfaces above must drop before MF/COM shutdown.
    _runtime: Runtime,
}
impl Backend {
    pub fn open(config: NativeConfig, description: &[u8]) -> Result<Self> {
        unsafe {
            let runtime = Runtime::new()?;
            let class = match config.mode {
                1 => AACMFTEncoder,
                2 => CLSID_MSAACDecMFT,
                3 => CLSID_MSH264EncoderMFT,
                4 => CLSID_MSH264DecoderMFT,
                _ => return Err(Error::invalid("invalid codec mode")),
            };
            let transform: IMFTransform = CoCreateInstance(&class, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| Error::unsupported(format!("OS codec unavailable: {e}")))?;
            let mut input_id = [0];
            let mut output_id = [0];
            if transform
                .GetStreamIDs(&mut input_id, &mut output_id)
                .is_ok()
                && (input_id != [0] || output_id != [0])
            {
                return Err(Error::unsupported(
                    "nonzero transform stream IDs are unsupported",
                ));
            }
            let encode = matches!(config.mode, 1 | 3);
            let compressed = media_type(&config, true, description)
                .map_err(|e| Error::unsupported(format!("compressed type: {e}")))?;
            let raw = media_type(&config, false, &[]).map_err(failure)?;
            if encode {
                transform
                    .SetOutputType(0, &compressed, 0)
                    .map_err(|e| Error::unsupported(format!("encoder output type: {e}")))?;
                transform
                    .SetInputType(0, &raw, 0)
                    .map_err(|e| Error::unsupported(format!("encoder input type: {e}")))?;
            } else {
                transform
                    .SetInputType(0, &compressed, 0)
                    .map_err(|e| Error::unsupported(format!("decoder input type: {e}")))?;
                transform
                    .SetOutputType(0, &raw, 0)
                    .map_err(|e| Error::unsupported(format!("decoder output type: {e}")))?;
            }
            if let Ok(attributes) = transform.GetAttributes() {
                let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(failure)?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(failure)?;
            Ok(Self {
                transform,
                output_type: if encode { compressed } else { raw },
                config,
                description: Vec::new(),
                draining: false,
                _runtime: runtime,
            })
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn send(
        &mut self,
        bytes: &[u8],
        timestamp: i64,
        duration: u64,
        frames: u32,
        format: u32,
        key: bool,
    ) -> Result<bool> {
        unsafe {
            if self.draining {
                return Err(failure("input after drain"));
            }
            let bytes = match self.config.mode {
                3 => crate::pixels::nv12(bytes, format, self.config.width, self.config.height)?,
                4 => bitstream::to_annex_b(bytes)?,
                _ => bytes.to_vec(),
            };
            let sample = sample_buffer(bytes.len() as u32, 0).map_err(failure)?;
            let buffer = sample.ConvertToContiguousBuffer().map_err(failure)?;
            let mut ptr = std::ptr::null_mut();
            let mut capacity = 0;
            buffer
                .Lock(&mut ptr, Some(&mut capacity), None)
                .map_err(failure)?;
            if ptr.is_null() || capacity < bytes.len() as u32 {
                let _ = buffer.Unlock();
                return Err(failure("invalid input buffer"));
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            buffer.Unlock().map_err(failure)?;
            buffer
                .SetCurrentLength(bytes.len() as u32)
                .map_err(failure)?;
            sample
                .SetSampleTime(
                    timestamp
                        .checked_mul(10)
                        .ok_or_else(|| Error::invalid("timestamp exceeds MF time range"))?,
                )
                .map_err(failure)?;
            let ticks = if self.config.mode == 1 {
                u64::from(frames) * 10_000_000 / u64::from(self.config.sample_rate)
            } else if duration > 0 {
                duration
                    .checked_mul(10)
                    .ok_or_else(|| Error::invalid("duration overflow"))?
            } else if self.config.mode == 3 {
                (10_000_000.0 / self.config.framerate).round() as u64
            } else {
                1
            };
            sample
                .SetSampleDuration(
                    i64::try_from(ticks.max(1))
                        .map_err(|_| Error::invalid("duration exceeds MF time range"))?,
                )
                .map_err(failure)?;
            sample
                .SetUINT32(&MFSampleExtension_CleanPoint, u32::from(key))
                .map_err(failure)?;
            if self.config.mode == 3 && key {
                let api: ICodecAPI = self.transform.cast().map_err(failure)?;
                api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32))
                    .map_err(failure)?;
            }
            match self.transform.ProcessInput(0, &sample, 0) {
                Ok(()) => Ok(true),
                Err(e) if e.code() == MF_E_NOTACCEPTING => Ok(false),
                Err(e) => Err(failure(e)),
            }
        }
    }
    pub fn receive(&mut self) -> Result<Receive<Output>> {
        unsafe {
            for _ in 0..8 {
                let info = self.transform.GetOutputStreamInfo(0).map_err(failure)?;
                if info.cbSize as usize > crate::MAX_BYTES {
                    return Err(Error::exhausted());
                }
                let sample = if info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0 {
                    None
                } else {
                    Some(sample_buffer(info.cbSize.max(1), info.cbAlignment).map_err(failure)?)
                };
                let mut output = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(sample),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                }];
                let mut status = 0;
                let result = self.transform.ProcessOutput(0, &mut output, &mut status);
                // These COM fields are ManuallyDrop in windows-rs. Always release
                // events and retain the returned sample, including on failed calls.
                let sample = ManuallyDrop::take(&mut output[0].pSample);
                drop(ManuallyDrop::take(&mut output[0].pEvents));
                match result {
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                        return Ok(if self.draining {
                            Receive::End
                        } else {
                            Receive::Pending
                        });
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        self.negotiate()?;
                        continue;
                    }
                    Err(e) => return Err(failure(e)),
                    Ok(()) => {}
                }
                let Some(sample) = sample else {
                    return Ok(Receive::Pending);
                };
                let bytes = sample_bytes(&sample)?;
                if bytes.is_empty() {
                    continue;
                }
                let mut o = Output {
                    bytes,
                    timestamp: sample.GetSampleTime().map_err(failure)? / 10,
                    duration: sample
                        .GetSampleDuration()
                        .ok()
                        .filter(|d| *d > 0)
                        .map_or(0, |d| d as u64 / 10),
                    key: sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0,
                    ..Default::default()
                };
                match self.config.mode {
                    1 => {
                        if o.duration == 0 {
                            o.duration = 1024 * 1_000_000 / u64::from(self.config.sample_rate);
                        }
                    }
                    2 => {
                        o.frames = (o.bytes.len() / (self.config.channels as usize * 2)) as u32;
                        o.sample_rate = self.config.sample_rate;
                        o.channels = self.config.channels;
                        o.format = 1;
                    }
                    3 => {
                        o.bytes = bitstream::from_annex_b(&o.bytes)?;
                        let media_type = self.transform.GetOutputCurrentType(0).map_err(failure)?;
                        let len = media_type
                            .GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER)
                            .map_err(failure)?;
                        if len as usize > crate::MAX_BYTES {
                            return Err(Error::exhausted());
                        }
                        let mut blob = vec![0; len as usize];
                        media_type
                            .GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut blob, None)
                            .map_err(failure)?;
                        let desc = bitstream::description(&blob)?;
                        if desc != self.description {
                            self.description = desc.clone();
                            o.description = desc;
                        }
                    }
                    4 => {
                        let size = self
                            .output_type
                            .GetUINT64(&MF_MT_FRAME_SIZE)
                            .map_err(failure)?;
                        let w = (size >> 32) as usize;
                        let h = (size & 0xffffffff) as usize;
                        let stride = self
                            .output_type
                            .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                            .unwrap_or(w as u32) as usize;
                        if w == 0
                            || h == 0
                            || !w.is_multiple_of(2)
                            || !h.is_multiple_of(2)
                            || stride < w
                            || stride.checked_mul(h).is_none_or(|n| {
                                n > crate::MAX_BYTES * 2 / 3 || n * 3 / 2 != o.bytes.len()
                            })
                        {
                            return Err(Error::unsupported("unsupported MF NV12 output layout"));
                        }
                        let mut packed = vec![0; w * h * 3 / 2];
                        for row in 0..h * 3 / 2 {
                            packed[row * w..(row + 1) * w]
                                .copy_from_slice(&o.bytes[row * stride..row * stride + w]);
                        }
                        o.bytes = packed;
                        o.width = w as u32;
                        o.height = h as u32;
                        o.format = 3;
                    }
                    _ => unreachable!(),
                }
                return Ok(Receive::Output(o));
            }
            Ok(Receive::Pending)
        }
    }
    fn negotiate(&mut self) -> Result<()> {
        unsafe {
            for index in 0..100 {
                let Ok(t) = self.transform.GetOutputAvailableType(0, index) else {
                    break;
                };
                let wanted = if self.config.mode == 2 {
                    MFAudioFormat_PCM
                } else {
                    MFVideoFormat_NV12
                };
                if t.GetGUID(&MF_MT_SUBTYPE).ok() != Some(wanted) {
                    continue;
                }
                if self.config.mode == 2
                    && (t.GetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE).ok() != Some(16)
                        || t.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).ok()
                            != Some(self.config.channels)
                        || t.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).ok()
                            != Some(self.config.sample_rate))
                {
                    continue;
                }
                if self.transform.SetOutputType(0, &t, 0).is_ok() {
                    self.output_type = t;
                    return Ok(());
                }
            }
            Err(Error::unsupported(
                "Media Foundation changed to an unsupported output format",
            ))
        }
    }
    pub fn drain(&mut self) -> Result<bool> {
        unsafe {
            if !self.draining {
                self.transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
                    .map_err(failure)?;
                self.transform
                    .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                    .map_err(failure)?;
                self.draining = true;
            }
            Ok(true)
        }
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        }
    }
}

unsafe fn media_type(
    c: &NativeConfig,
    compressed: bool,
    description: &[u8],
) -> windows::core::Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(
            &MF_MT_MAJOR_TYPE,
            if c.mode <= 2 {
                &MFMediaType_Audio
            } else {
                &MFMediaType_Video
            },
        )?;
        let subtype: GUID = if c.mode <= 2 {
            if compressed {
                MFAudioFormat_AAC
            } else {
                MFAudioFormat_PCM
            }
        } else if compressed {
            MFVideoFormat_H264
        } else {
            MFVideoFormat_NV12
        };
        t.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
        if c.mode <= 2 {
            t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, c.sample_rate)?;
            t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, c.channels)?;
            t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            t.SetUINT32(
                &MF_MT_AUDIO_BLOCK_ALIGNMENT,
                if compressed { 1 } else { c.channels * 2 },
            )?;
            if compressed {
                t.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?;
                if c.mode == 1 {
                    t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, (c.bitrate / 8) as u32)?;
                }
                if !description.is_empty() {
                    let mut blob = vec![0; 12];
                    blob[2] = 0xfe;
                    blob.extend_from_slice(description);
                    t.SetBlob(&MF_MT_USER_DATA, &blob)?;
                }
            } else {
                t.SetUINT32(
                    &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
                    c.sample_rate * c.channels * 2,
                )?;
            }
        } else {
            if c.width > 0 {
                t.SetUINT64(
                    &MF_MT_FRAME_SIZE,
                    (u64::from(c.width) << 32) | u64::from(c.height),
                )?;
            }
            t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
            if c.mode == 3 {
                t.SetUINT64(
                    &MF_MT_FRAME_RATE,
                    (((c.framerate * 1000.0).round() as u64) << 32) | 1000,
                )?;
                if compressed {
                    t.SetUINT32(&MF_MT_AVG_BITRATE, c.bitrate as u32)?;
                    t.SetUINT32(&MF_MT_MPEG2_PROFILE, 66)?;
                    t.SetUINT32(&MF_MT_MPEG2_LEVEL, 30)?;
                }
            }
            if compressed && !description.is_empty() {
                let (sps, pps) = bitstream::parameter_sets(description).map_err(|_| {
                    windows::core::Error::from_hresult(windows::Win32::Foundation::E_INVALIDARG)
                })?;
                t.SetBlob(
                    &MF_MT_MPEG_SEQUENCE_HEADER,
                    &[&[0, 0, 0, 1][..], sps, &[0, 0, 0, 1][..], pps].concat(),
                )?;
            }
        }
        Ok(t)
    }
}
unsafe fn sample_buffer(len: u32, alignment: u32) -> windows::core::Result<IMFSample> {
    unsafe {
        let sample = MFCreateSample()?;
        let buffer = MFCreateAlignedMemoryBuffer(len, alignment)?;
        sample.AddBuffer(&buffer)?;
        Ok(sample)
    }
}
unsafe fn sample_bytes(sample: &IMFSample) -> Result<Vec<u8>> {
    unsafe {
        let buffer = sample.ConvertToContiguousBuffer().map_err(failure)?;
        let mut ptr = std::ptr::null_mut();
        let mut len = 0;
        buffer
            .Lock(&mut ptr, None, Some(&mut len))
            .map_err(failure)?;
        let result = if len == 0 {
            Ok(Vec::new())
        } else if ptr.is_null() || len as usize > crate::MAX_BYTES {
            Err(Error::exhausted())
        } else {
            Ok(std::slice::from_raw_parts(ptr, len as usize).to_vec())
        };
        buffer.Unlock().map_err(failure)?;
        result
    }
}
