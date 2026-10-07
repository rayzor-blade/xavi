use ndk::media::media_format::MediaFormat;
use ndk_sys as sys;
use std::{fs::File, os::fd::AsRawFd, path::Path, ptr::NonNull};
use xavi_core::mux::{Mp4Config, Track};
use xavi_core::{EncodedChunk, EncodedChunkType, Error, ErrorKind, Result};
fn check(status: sys::media_status_t) -> Result<()> {
    if status == sys::media_status_t::AMEDIA_OK {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::Io,
            format!("AMediaMuxer status {}", status.0),
        ))
    }
}
pub(crate) struct Backend {
    handle: NonNull<sys::AMediaMuxer>,
    tracks: [usize; 2],
    ends: [Option<i64>; 2],
    stopped: bool,
    // Keep the original descriptor open until the native writer is deleted.
    _file: File,
}
impl Backend {
    pub fn open(path: &Path, config: &Mp4Config) -> Result<Self> {
        let file = File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| Error::new(ErrorKind::Io, e.to_string()))?;
        let handle = NonNull::new(unsafe {
            sys::AMediaMuxer_new(
                file.as_raw_fd(),
                sys::OutputFormat::AMEDIAMUXER_OUTPUT_FORMAT_MPEG_4,
            )
        })
        .ok_or_else(|| Error::unsupported("AMediaMuxer MP4 is unavailable"))?;
        let mut writer = Self {
            handle,
            tracks: [usize::MAX; 2],
            ends: [None; 2],
            stopped: false,
            _file: file,
        };
        if let Some(c) = &config.audio {
            let mut f = MediaFormat::new();
            f.set_str("mime", "audio/mp4a-latm");
            f.set_i32("sample-rate", c.sample_rate as i32);
            f.set_i32("channel-count", c.channels as i32);
            f.set_buffer("csd-0", &c.description);
            writer.add(Track::Audio, &f)?;
        }
        if let Some(c) = &config.video {
            let mut f = MediaFormat::new();
            f.set_str("mime", "video/avc");
            f.set_i32("width", c.coded_width.unwrap() as i32);
            f.set_i32("height", c.coded_height.unwrap() as i32);
            let (sps, pps) = crate::bitstream::parameter_sets(&c.description)?;
            f.set_buffer("csd-0", &[&[0, 0, 0, 1][..], sps].concat());
            f.set_buffer("csd-1", &[&[0, 0, 0, 1][..], pps].concat());
            writer.add(Track::Video, &f)?;
        }
        check(unsafe { sys::AMediaMuxer_start(handle.as_ptr()) })?;
        Ok(writer)
    }
    fn add(&mut self, track: Track, format: &MediaFormat) -> Result<()> {
        let index = unsafe { sys::AMediaMuxer_addTrack(self.handle.as_ptr(), format.as_ptr()) };
        self.tracks[track.index()] = usize::try_from(index)
            .map_err(|_| Error::unsupported("AMediaMuxer rejected track format"))?;
        Ok(())
    }
    pub fn write(
        &mut self,
        track: Track,
        chunk: &EncodedChunk,
        timestamp: i64,
        duration: u64,
    ) -> Result<bool> {
        let converted;
        let data = if track == Track::Video {
            converted = crate::bitstream::to_annex_b(chunk.bytes())?;
            converted.as_slice()
        } else {
            chunk.bytes()
        };
        let info = sys::AMediaCodecBufferInfo {
            offset: 0,
            size: data.len() as i32,
            presentationTimeUs: timestamp,
            flags: u32::from(chunk.kind() == EncodedChunkType::Key),
        };
        check(unsafe {
            sys::AMediaMuxer_writeSampleData(
                self.handle.as_ptr(),
                self.tracks[track.index()],
                data.as_ptr(),
                &info,
            )
        })?;
        self.ends[track.index()] = Some(timestamp + duration as i64);
        Ok(true)
    }
    pub fn end_track(&mut self, track: Track) -> Result<()> {
        // Empty EOS sample supplies the final sample duration to MPEG4Writer.
        if let Some(end) = self.ends[track.index()].take() {
            let info = sys::AMediaCodecBufferInfo {
                offset: 0,
                size: 0,
                presentationTimeUs: end,
                flags: 4,
            };
            check(unsafe {
                sys::AMediaMuxer_writeSampleData(
                    self.handle.as_ptr(),
                    self.tracks[track.index()],
                    [0u8].as_ptr(),
                    &info,
                )
            })?;
        }
        Ok(())
    }
    pub fn finish(&mut self) -> Result<bool> {
        if !self.stopped {
            self.stopped = true;
            check(unsafe { sys::AMediaMuxer_stop(self.handle.as_ptr()) })?;
        }
        Ok(true)
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        unsafe {
            sys::AMediaMuxer_delete(self.handle.as_ptr());
        }
    }
}
