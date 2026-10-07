use super::{Runtime, failure, media_type, sample_buffer};
use crate::native::NativeConfig;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::Win32::Media::MediaFoundation::*;
use windows::core::PCWSTR;
use xavi_core::mux::{Mp4Config, Track};
use xavi_core::{EncodedChunk, EncodedChunkType, Error, Result};

pub(crate) struct Backend {
    writer: IMFSinkWriter,
    tracks: [u32; 2],
    finished: bool,
    _runtime: Runtime,
}
impl Backend {
    pub fn open(path: &Path, config: &Mp4Config) -> Result<Self> {
        unsafe {
            let runtime = Runtime::new()?;
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 1).map_err(failure)?;
            let attributes = attributes.unwrap();
            // Fixed passthrough types. No encoders or third-party transforms.
            attributes
                .SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)
                .map_err(failure)?;
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            let writer = MFCreateSinkWriterFromURL(PCWSTR(wide.as_ptr()), None, &attributes)
                .map_err(failure)?;
            let mut result = Self {
                writer,
                tracks: [u32::MAX; 2],
                finished: false,
                _runtime: runtime,
            };
            if let Some(c) = &config.audio {
                let config = NativeConfig {
                    mode: 2,
                    sample_rate: c.sample_rate,
                    channels: c.channels,
                    ..Default::default()
                };
                result.add(Track::Audio, &config, &c.description)?;
            }
            if let Some(c) = &config.video {
                let config = NativeConfig {
                    mode: 4,
                    width: c.coded_width.unwrap(),
                    height: c.coded_height.unwrap(),
                    ..Default::default()
                };
                result.add(Track::Video, &config, &c.description)?;
            }
            result.writer.BeginWriting().map_err(failure)?;
            Ok(result)
        }
    }
    unsafe fn add(
        &mut self,
        track: Track,
        config: &NativeConfig,
        description: &[u8],
    ) -> Result<()> {
        unsafe {
            let media = media_type(config, true, description).map_err(failure)?;
            let index = self
                .writer
                .AddStream(&media)
                .map_err(|e| Error::unsupported(format!("MP4 track format: {e}")))?;
            self.writer
                .SetInputMediaType(index, &media, None)
                .map_err(|e| Error::unsupported(format!("MP4 passthrough: {e}")))?;
            self.tracks[track.index()] = index;
            Ok(())
        }
    }
    pub fn write(
        &mut self,
        track: Track,
        chunk: &EncodedChunk,
        timestamp: i64,
        duration: u64,
    ) -> Result<bool> {
        let converted;
        let bytes = if track == Track::Video {
            converted = crate::bitstream::to_annex_b(chunk.bytes())?;
            converted.as_slice()
        } else {
            chunk.bytes()
        };
        unsafe {
            let sample = sample_buffer(bytes.len() as u32, 0).map_err(failure)?;
            let buffer = sample.ConvertToContiguousBuffer().map_err(failure)?;
            let mut ptr = std::ptr::null_mut();
            let mut capacity = 0;
            buffer
                .Lock(&mut ptr, Some(&mut capacity), None)
                .map_err(failure)?;
            if ptr.is_null() || (capacity as usize) < bytes.len() {
                let _ = buffer.Unlock();
                return Err(failure("invalid sink input buffer"));
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            buffer.Unlock().map_err(failure)?;
            buffer
                .SetCurrentLength(bytes.len() as u32)
                .map_err(failure)?;
            sample.SetSampleTime(timestamp * 10).map_err(failure)?;
            sample
                .SetSampleDuration(duration as i64 * 10)
                .map_err(failure)?;
            sample
                .SetUINT32(
                    &MFSampleExtension_CleanPoint,
                    u32::from(chunk.kind() == EncodedChunkType::Key),
                )
                .map_err(failure)?;
            // Synchronous MF mode applies native throttling on this worker.
            self.writer
                .WriteSample(self.tracks[track.index()], &sample)
                .map_err(failure)?;
        }
        Ok(true)
    }
    pub fn end_track(&mut self, track: Track) -> Result<()> {
        unsafe {
            self.writer
                .NotifyEndOfSegment(self.tracks[track.index()])
                .map_err(failure)
        }
    }
    pub fn finish(&mut self) -> Result<bool> {
        if !self.finished {
            unsafe {
                self.writer.Finalize().map_err(failure)?;
            }
            self.finished = true;
        }
        Ok(true)
    }
}
