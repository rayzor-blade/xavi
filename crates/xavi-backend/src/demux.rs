//! Incremental MP4 packet reading. The portable parser does not decode media.
//! Metadata is bounded; packet payloads are read on demand on a private worker.
use crate::pipeline::{Configuration, Item, Reader, Worker, lock};
use std::{
    fs::File,
    io::{Read as IoRead, Seek, SeekFrom},
    path::PathBuf,
    sync::{Arc, Mutex, atomic::Ordering},
    thread,
    time::Duration,
};
use xavi_core::{
    codec::{AudioDecoderConfig, VideoDecoderConfig},
    stream::{self, Limits},
    *,
};
fn failure(e: impl std::fmt::Display) -> Error {
    Error::invalid(format!("MP4: {e}"))
}
fn io<T>(v: std::io::Result<T>) -> Result<T> {
    v.map_err(failure)
}
fn micros(value: i128, scale: u32) -> Result<i64> {
    if scale == 0 {
        return Err(failure("zero timescale"));
    }
    i64::try_from(value * 1_000_000 / i128::from(scale)).map_err(|_| failure("timestamp overflow"))
}
// Check container boundaries before handing metadata to the parser. Payloads
// (mdat) are skipped without allocating or loading a complete media file.
fn check_boxes(file: &mut File, end: u64, depth: u32, budget: &mut u64) -> Result<()> {
    if depth > 8 {
        return Err(failure("box nesting is too deep"));
    }
    while io(file.stream_position())? < end {
        let start = io(file.stream_position())?;
        if end - start < 8 {
            return Err(failure("truncated box header"));
        }
        let mut header = [0; 8];
        io(file.read_exact(&mut header))?;
        let mut size = u64::from(u32::from_be_bytes(header[..4].try_into().unwrap()));
        let mut header_size = 8;
        if size == 1 {
            let mut wide = [0; 8];
            io(file.read_exact(&mut wide))?;
            size = u64::from_be_bytes(wide);
            header_size = 16;
        }
        if size == 0 && &header[4..] == b"mdat" {
            size = end - start;
        }
        if size < header_size || size > end - start {
            return Err(failure("invalid box bounds"));
        }
        let next = start + size;
        if depth == 0 && (&header[4..] == b"moof" || &header[4..] == b"mfra") {
            return Err(Error::unsupported(
                "fragmented MP4 input is not supported by this reader",
            ));
        }
        if depth == 0 && matches!(&header[4..], b"moov" | b"ftyp" | b"emsg") {
            *budget = budget
                .checked_sub(size)
                .ok_or_else(|| failure("metadata exceeds 32 MiB"))?;
        }
        if matches!(
            &header[4..],
            b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"edts"
        ) {
            check_boxes(file, next, depth + 1, budget)?;
        }
        io(file.seek(SeekFrom::Start(next)))?;
    }
    Ok(())
}
struct Track {
    id: u32,
    next: u32,
    count: u32,
    scale: u32,
    offset: i64,
    audio: bool,
}
struct Source {
    file: mp4::Mp4Reader<File>,
    audio: Option<Track>,
    video: Option<Track>,
    configs: [Option<Configuration>; 2],
    duration: f64,
}
impl Source {
    fn open(path: PathBuf, limit: usize) -> Result<Self> {
        let mut file = io(File::open(path))?;
        let size = io(file.metadata())?.len();
        check_boxes(&mut file, size, 0, &mut (32 * 1024 * 1024))?;
        io(file.seek(SeekFrom::Start(0)))?;
        let reader = mp4::Mp4Reader::read_header(file, size).map_err(failure)?;
        if reader.timescale() == 0 || reader.tracks().len() > 32 {
            return Err(failure("invalid movie timescale or too many tracks"));
        }
        let mut audio = None;
        let mut video = None;
        let mut configs = [None, None];
        let mut tracks: Vec<_> = reader.tracks().values().collect();
        tracks.sort_by_key(|t| t.track_id());
        for track in tracks {
            let kind = track.track_type().map_err(failure)?;
            let is_audio = kind == mp4::TrackType::Audio;
            if !is_audio && kind != mp4::TrackType::Video {
                continue;
            }
            if (is_audio && audio.is_some()) || (!is_audio && video.is_some()) {
                continue;
            }
            if track.timescale() == 0 || track.sample_count() > 2_000_000 {
                return Err(failure("invalid track timescale or sample count"));
            }
            let stbl = &track.trak.mdia.minf.stbl;
            if stbl.stsz.sample_size as usize > limit
                || stbl.stsz.sample_sizes.iter().any(|s| *s as usize > limit)
                || stbl
                    .stsc
                    .entries
                    .iter()
                    .any(|e| e.samples_per_chunk == 0 || e.first_chunk == 0)
            {
                return Err(failure("invalid or oversized sample table"));
            }
            let mut offset = 0i64;
            if let Some(edts) = track.trak.edts.as_ref().and_then(|e| e.elst.as_ref()) {
                let mut media_edit = false;
                for entry in &edts.entries {
                    if entry.media_rate != 1 || entry.media_rate_fraction != 0 || media_edit {
                        return Err(Error::unsupported(
                            "multiple/rate-changing MP4 edits are not supported",
                        ));
                    }
                    let empty = entry.media_time == u64::MAX
                        || (edts.version == 0 && entry.media_time == u64::from(u32::MAX));
                    let delta = if empty {
                        micros(i128::from(entry.segment_duration), reader.timescale())?
                    } else {
                        media_edit = true;
                        -micros(i128::from(entry.media_time), track.timescale())?
                    };
                    offset = offset
                        .checked_add(delta)
                        .ok_or_else(|| failure("edit timestamp overflow"))?;
                }
            }
            let config = if is_audio {
                let rate = track.sample_freq_index().map_err(failure)?.freq();
                let channels = track.channel_config().map_err(failure)? as u32;
                if track.audio_profile().map_err(failure)? as u8 != 2
                    || !(1..=2).contains(&channels)
                {
                    return Err(Error::unsupported("MP4 audio requires AAC-LC mono/stereo"));
                }
                let index = track.sample_freq_index().map_err(failure)? as u8;
                Configuration::Audio(AudioDecoderConfig {
                    codec: xavi_platform::AAC.into(),
                    sample_rate: rate,
                    channels,
                    description: vec![0x10 | (index >> 1), (index << 7) | ((channels as u8) << 3)]
                        .into(),
                })
            } else {
                let avc = &stbl
                    .stsd
                    .avc1
                    .as_ref()
                    .ok_or_else(|| Error::unsupported("MP4 video requires H.264"))?
                    .avcc;
                if avc.length_size_minus_one & 3 != 3
                    || avc.sequence_parameter_sets.len() != 1
                    || avc.picture_parameter_sets.len() != 1
                {
                    return Err(Error::unsupported(
                        "AVC requires four-byte lengths and one SPS/PPS",
                    ));
                }
                let sps = track.sequence_parameter_set().map_err(failure)?;
                let pps = track.picture_parameter_set().map_err(failure)?;
                if sps.len() < 4 || sps.len() > 65535 || pps.is_empty() || pps.len() > 65535 {
                    return Err(failure("invalid AVC parameter sets"));
                }
                let mut description = vec![1, sps[1], sps[2], sps[3], 255, 225];
                description.extend_from_slice(&(sps.len() as u16).to_be_bytes());
                description.extend_from_slice(sps);
                description.push(1);
                description.extend_from_slice(&(pps.len() as u16).to_be_bytes());
                description.extend_from_slice(pps);
                Configuration::Video(VideoDecoderConfig {
                    codec: format!("avc1.{:02X}{:02X}{:02X}", sps[1], sps[2], sps[3]),
                    coded_width: Some(track.width().into()),
                    coded_height: Some(track.height().into()),
                    description: description.into(),
                    color_space: VideoColorSpace::default(),
                })
            };
            let state = Track {
                id: track.track_id(),
                next: 1,
                count: track.sample_count(),
                scale: track.timescale(),
                offset,
                audio: is_audio,
            };
            if is_audio {
                audio = Some(state);
                configs[0] = Some(config);
            } else {
                video = Some(state);
                configs[1] = Some(config);
            }
        }
        if audio.is_none() && video.is_none() {
            return Err(Error::unsupported(
                "MP4 has no supported audio/video tracks",
            ));
        }
        let duration = reader.moov.mvhd.duration as f64 / reader.timescale() as f64;
        Ok(Self {
            file: reader,
            audio,
            video,
            configs,
            duration,
        })
    }
    fn read(&mut self, index: usize) -> Result<Option<Item>> {
        let track = if index == 0 {
            &mut self.audio
        } else {
            &mut self.video
        };
        let Some(t) = track else {
            return Ok(None);
        };
        if t.next > t.count {
            return Ok(None);
        }
        let sample = self
            .file
            .read_sample(t.id, t.next)
            .map_err(failure)?
            .ok_or_else(|| failure("sample table ended unexpectedly"))?;
        t.next += 1;
        let timestamp = micros(
            i128::from(sample.start_time) + i128::from(sample.rendering_offset),
            t.scale,
        )?
        .checked_add(t.offset)
        .ok_or_else(|| failure("presentation timestamp overflow"))?;
        let duration = micros(i128::from(sample.duration), t.scale)? as u64;
        let chunk = Arc::new(EncodedChunk::new(
            if t.audio || sample.is_sync {
                EncodedChunkType::Key
            } else {
                EncodedChunkType::Delta
            },
            timestamp,
            Some(duration),
            &sample.bytes,
        )?);
        Ok(Some(if t.audio {
            Item::AudioChunk(chunk)
        } else {
            Item::VideoChunk(chunk)
        }))
    }
}

pub struct Demuxer {
    pub audio: Mutex<Reader<Item>>,
    pub video: Mutex<Reader<Item>>,
    pub configs: Arc<Mutex<[Option<Configuration>; 2]>>,
    pub duration: Arc<Mutex<f64>>,
    pub worker: Worker,
}
impl Demuxer {
    pub fn open(path: PathBuf, limits: Limits) -> Result<Self> {
        let (a, ar) = stream::channel(limits)?;
        let (v, vr) = stream::channel(limits)?;
        let configs = Arc::new(Mutex::new([None, None]));
        let info = configs.clone();
        let duration = Arc::new(Mutex::new(0.0));
        let time = duration.clone();
        let worker = Worker::start("xavi-demux", move |cancel, initialized| {
            let opened = Source::open(path, limits.max_bytes.min(64 * 1024 * 1024));
            let mut source = match opened {
                Ok(source) => source,
                Err(error) => {
                    let _ = initialized.send(Err(error.clone()));
                    return Err(error);
                }
            };
            *lock(&info)? = source.configs.clone();
            *lock(&time)? = source.duration;
            let _ = initialized.send(Ok(()));
            let mut outputs = [a, v];
            let mut pending = [None, None];
            let mut ended = [false, false];
            let result = (|| {
                while !cancel.load(Ordering::Acquire) {
                    for i in 0..2 {
                        if ended[i] {
                            continue;
                        }
                        if pending[i].is_none() {
                            pending[i] = source.read(i)?;
                        }
                        if let Some(item) = pending[i].take() {
                            match outputs[i].try_send(item) {
                                Ok(()) => {}
                                Err(e) if e.error.kind == ErrorKind::WouldBlock => {
                                    pending[i] = Some(e.value)
                                }
                                Err(e) => return Err(e.error),
                            }
                        } else {
                            outputs[i].finish()?;
                            ended[i] = true;
                        }
                    }
                    if ended == [true, true] {
                        return Ok(());
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                Ok(())
            })();
            if let Err(error) = &result {
                for output in &mut outputs {
                    let _ = output.fail(error.clone());
                }
            }
            result
        })?;
        Ok(Self {
            audio: Mutex::new(Reader::new(ar)),
            video: Mutex::new(Reader::new(vr)),
            configs,
            duration,
            worker,
        })
    }
}
