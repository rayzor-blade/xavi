//! Run on the target device/OS with its native codecs installed. Linux also
//! requires the system GStreamer plugins listed in xavi-platform's crate docs.
#![cfg(any(
    target_vendor = "apple",
    target_os = "windows",
    target_os = "android",
    target_os = "linux"
))]
use std::sync::Arc;
use std::time::{Duration, Instant};
use xavi_core::codec::*;
use xavi_core::stream::Limits;
use xavi_core::*;
use xavi_platform::{AAC, AudioDecoder, AudioEncoder, H264, VideoDecoder, VideoEncoder};

fn session<E: Engine>() -> Codec<E> {
    Codec::new(
        Limits {
            max_items: 4,
            max_bytes: 2_000_000,
        },
        Limits {
            max_items: 2,
            max_bytes: 2_000_000,
        },
    )
    .unwrap()
}
fn pump<E: Engine>(codec: &mut Codec<E>, output: &mut Vec<E::Output>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let steps = codec.pump(32).unwrap();
        while let Some(value) = codec.next_output() {
            output.push(value);
        }
        if steps == 0 && codec.queue_size() == 0 {
            return;
        }
        if steps == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    panic!("codec did not settle");
}
fn flush<E: Engine>(codec: &mut Codec<E>, output: &mut Vec<E::Output>) {
    let token = codec.begin_flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        pump(codec, output);
        if codec.flush_complete(token).unwrap() {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("flush did not complete");
}

fn wait_for_output<E: Engine>(codec: &mut Codec<E>, output: &mut Vec<E::Output>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while output.is_empty() && Instant::now() < deadline {
        pump(codec, output);
        if output.is_empty() {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    assert!(
        !output.is_empty(),
        "streaming codec must produce output before EOS"
    );
}

#[test]
fn aac_encodes_incremental_pcm_and_decodes_access_units() {
    let config = AudioEncoderConfig {
        codec: AAC.into(),
        sample_rate: 48000,
        channels: 1,
        bitrate: Some(96000),
    };
    assert!(support::<AudioEncoder>(config.clone()).unwrap().supported);
    let mut encoder = session::<AudioEncoder>();
    encoder.configure(config).unwrap();
    let mut encoded = Vec::new();
    let mut offset = 0;
    for frames in [257, 1791, 333, 4096, 4096] {
        let samples: Vec<u8> = (offset..offset + frames)
            .flat_map(|i| (((i % 100) as i16 - 50) * 200).to_ne_bytes())
            .collect();
        let audio = Arc::new(
            AudioData::new(
                AudioDescriptor {
                    format: AudioSampleFormat::S16,
                    sample_rate: 48000.0,
                    number_of_frames: frames,
                    number_of_channels: 1,
                    timestamp: -5000 + i64::from(offset) * 1_000_000 / 48000,
                },
                &samples,
            )
            .unwrap(),
        );
        encoder.try_submit(audio).unwrap();
        pump(&mut encoder, &mut encoded);
        offset += frames;
    }
    wait_for_output(&mut encoder, &mut encoded);
    flush(&mut encoder, &mut encoded);
    let config = encoded[0].decoder_config.clone().unwrap();
    assert_eq!(config.description.as_ref(), &[0x11, 0x88]);
    let mut decoder = session::<AudioDecoder>();
    decoder.configure(config).unwrap();
    let mut decoded = Vec::new();
    for packet in &encoded {
        decoder.try_submit(packet.chunk.clone()).unwrap();
        pump(&mut decoder, &mut decoded);
    }
    flush(&mut decoder, &mut decoded);
    assert!(!decoded.is_empty());
    assert!(
        decoded
            .iter()
            .map(|a| u64::from(a.descriptor().number_of_frames))
            .sum::<u64>()
            >= u64::from(offset)
    );
    assert_eq!(decoded[0].descriptor().timestamp, -5000);
    assert!(
        decoded
            .windows(2)
            .all(|p| p[0].descriptor().timestamp < p[1].descriptor().timestamp)
    );
    let energy: i64 = decoded
        .iter()
        .flat_map(|a| a.bytes().as_chunks::<2>().0)
        .map(|b| i64::from(i16::from_ne_bytes(*b)).abs())
        .sum();
    assert!(energy > 1000 * i64::from(offset));
    assert_eq!(encoder.state(), CodecState::Configured);
    encoder.reset().unwrap();
    decoder.close();
}

#[test]
fn h264_round_trip_preserves_frames_timestamps_and_flush_lifecycle() {
    let config = VideoEncoderConfig {
        codec: H264.into(),
        width: 128,
        height: 96,
        bitrate: 500_000,
        framerate: 30.0,
    };
    assert!(support::<VideoEncoder>(config.clone()).unwrap().supported);
    let mut encoder = session::<VideoEncoder>();
    encoder.configure(config).unwrap();
    let mut packets = Vec::new();
    for index in 0..3 {
        let pixels = [30, 70 + index as u8 * 10, 180, 255].repeat(128 * 96);
        let mut d = VideoDescriptor::new(VideoPixelFormat::Bgra, 128, 96, -10_000 + index * 33_333);
        d.duration = Some(33_333);
        encoder
            .try_submit(VideoEncodeInput {
                frame: Arc::new(VideoFrame::new(d, &pixels, None).unwrap()),
                key_frame: index == 0,
            })
            .unwrap();
        pump(&mut encoder, &mut packets);
    }
    wait_for_output(&mut encoder, &mut packets);
    flush(&mut encoder, &mut packets);
    assert_eq!(packets.len(), 3);
    let config = packets[0].decoder_config.clone().unwrap();
    assert_eq!(packets[0].chunk.kind(), EncodedChunkType::Key);
    let mut decoder = session::<VideoDecoder>();
    decoder.configure(config).unwrap();
    let mut frames = Vec::new();
    for packet in &packets {
        decoder.try_submit(packet.chunk.clone()).unwrap();
        pump(&mut decoder, &mut frames);
    }
    flush(&mut decoder, &mut frames);
    assert_eq!(frames.len(), 3);
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(frame.info().coded_width, 128);
        assert_eq!(frame.info().coded_height, 96);
        assert_eq!(frame.info().timestamp, -10_000 + index as i64 * 33_333);
        match frame.info().format {
            VideoPixelFormat::Bgra => {
                let pixel = &frame.bytes()[..4];
                assert!((i16::from(pixel[0]) - 30).abs() < 20);
                assert!((i16::from(pixel[2]) - 180).abs() < 20);
            }
            VideoPixelFormat::Nv12 => {
                let y = 16 + ((66 * 180 + 129 * (70 + index as i32 * 10) + 25 * 30 + 128) >> 8);
                assert!((i32::from(frame.bytes()[0]) - y).abs() < 20);
            }
            other => panic!("unexpected decoded format: {other:?}"),
        }
    }
    // A post-flush decoder accepts a new key chunk and keeps the same config.
    decoder.try_submit(packets[0].chunk.clone()).unwrap();
    let mut again = Vec::new();
    flush(&mut decoder, &mut again);
    assert_eq!(again.len(), 1);
    let delta = Arc::new(
        EncodedChunk::new(
            EncodedChunkType::Delta,
            packets[0].chunk.timestamp(),
            packets[0].chunk.duration(),
            packets[0].chunk.bytes(),
        )
        .unwrap(),
    );
    decoder.try_submit(delta).unwrap();
    assert_eq!(
        decoder.pump(32).unwrap_err().kind,
        ErrorKind::InvalidArgument
    );
    assert_eq!(decoder.state(), CodecState::Closed);
}

#[test]
fn support_distinguishes_invalid_and_unsupported_configuration() {
    let mut c = AudioEncoderConfig {
        codec: "opus".into(),
        sample_rate: 48000,
        channels: 1,
        bitrate: None,
    };
    assert!(!support::<AudioEncoder>(c.clone()).unwrap().supported);
    c.sample_rate = 0;
    assert_eq!(
        support::<AudioEncoder>(c).unwrap_err().kind,
        ErrorKind::InvalidArgument
    );
    assert!(
        !support::<VideoEncoder>(VideoEncoderConfig {
            codec: H264.into(),
            width: u32::MAX - 1,
            height: u32::MAX - 1,
            bitrate: 500_000,
            framerate: 30.0,
        })
        .unwrap()
        .supported
    );
}

#[test]
fn discontinuous_pcm_requires_a_new_segment() {
    let mut encoder = session::<AudioEncoder>();
    encoder
        .configure(AudioEncoderConfig {
            codec: AAC.into(),
            sample_rate: 48_000,
            channels: 1,
            bitrate: Some(96_000),
        })
        .unwrap();
    let audio = Arc::new(
        AudioData::new(
            AudioDescriptor {
                format: AudioSampleFormat::S16,
                sample_rate: 48_000.0,
                number_of_frames: 1024,
                number_of_channels: 1,
                timestamp: 0,
            },
            &[0; 2048],
        )
        .unwrap(),
    );
    encoder.try_submit(audio.clone()).unwrap();
    pump(&mut encoder, &mut Vec::new());
    encoder.try_submit(audio).unwrap();
    assert_eq!(
        encoder.pump(32).unwrap_err().kind,
        ErrorKind::InvalidArgument
    );
    assert_eq!(encoder.state(), CodecState::Closed);
}

#[test]
fn native_mp4_muxes_encoded_tracks_and_only_publishes_a_finished_file() {
    use xavi_core::mux::{Mp4Config, MuxState, Muxer, Track};
    use xavi_platform::mux::NativeMuxer;
    let mut audio = session::<AudioEncoder>();
    audio
        .configure(AudioEncoderConfig {
            codec: AAC.into(),
            sample_rate: 48000,
            channels: 1,
            bitrate: Some(96000),
        })
        .unwrap();
    audio
        .try_submit(Arc::new(
            AudioData::new(
                AudioDescriptor {
                    format: AudioSampleFormat::S16,
                    sample_rate: 48000.0,
                    number_of_frames: 8192,
                    number_of_channels: 1,
                    timestamp: -5000,
                },
                &[0; 16384],
            )
            .unwrap(),
        ))
        .unwrap();
    let mut audio_packets = Vec::new();
    flush(&mut audio, &mut audio_packets);
    let mut video = session::<VideoEncoder>();
    video
        .configure(VideoEncoderConfig {
            codec: H264.into(),
            width: 128,
            height: 96,
            bitrate: 500_000,
            framerate: 30.0,
        })
        .unwrap();
    let mut video_packets = Vec::new();
    for i in 0..3 {
        let mut d = VideoDescriptor::new(VideoPixelFormat::Bgra, 128, 96, -5000 + i * 33_333);
        d.duration = Some(33_333);
        video
            .try_submit(VideoEncodeInput {
                frame: Arc::new(
                    VideoFrame::new(d, &[20, 80, 180, 255].repeat(128 * 96), None).unwrap(),
                ),
                key_frame: i == 0,
            })
            .unwrap();
        pump(&mut video, &mut video_packets);
    }
    flush(&mut video, &mut video_packets);
    let timestamp_origin = audio_packets[0]
        .chunk
        .timestamp()
        .min(video_packets[0].chunk.timestamp());
    let config = Mp4Config {
        audio: audio_packets[0].decoder_config.clone(),
        video: video_packets[0].decoder_config.clone(),
        timestamp_origin,
        default_video_duration: Some(33_333),
    };
    struct Temp(std::path::PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let temp = Temp(std::env::temp_dir().join(format!(
            "xavi-native-mux-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
    std::fs::create_dir(&temp.0).unwrap();
    let path = temp.0.join("recording with spaces 雪.mp4");
    let mut writer = Muxer::<NativeMuxer>::create(&path, config.clone(), 2_000_000).unwrap();
    assert!(!path.exists());
    let tracks: [Vec<Arc<EncodedChunk>>; 2] = [
        audio_packets.iter().map(|p| p.chunk.clone()).collect(),
        video_packets.iter().map(|p| p.chunk.clone()).collect(),
    ];
    let mut index = [0; 2];
    let mut ended = [false; 2];
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ended.iter().all(|v| *v) {
        assert!(
            Instant::now() < deadline,
            "muxer stalled on interleaved input"
        );
        for (i, track) in [Track::Audio, Track::Video].into_iter().enumerate() {
            if ended[i] {
                continue;
            }
            if let Some(packet) = tracks[i].get(index[i]) {
                if writer.write(track, packet).unwrap() {
                    index[i] += 1;
                }
            } else {
                writer.end_track(track).unwrap();
                ended[i] = true;
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    while !writer.finish().unwrap() {
        assert!(!path.exists());
        assert!(Instant::now() < deadline, "muxer finalization stalled");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(writer.state(), MuxState::Finished);
    drop(writer);
    let bytes = std::fs::read(&path).unwrap();
    let top = mp4_boxes(&bytes);
    assert!(top.iter().any(|(t, _)| *t == *b"ftyp"));
    assert!(top.iter().any(|(t, b)| *t == *b"mdat" && !b.is_empty()));
    let movie = top
        .iter()
        .find(|(t, _)| *t == *b"moov")
        .expect("finalized movie index")
        .1;
    let mut counts = Vec::new();
    sample_counts(movie, &mut counts);
    counts.sort_unstable();
    assert_eq!(counts.len(), 2);
    assert_eq!(counts[0], video_packets.len() as u32);
    // AVAssetWriter can prepend AAC decoder preroll and describe its exclusion
    // through an edit list. Verify every submitted AAC access unit survives in
    // order, and that the movie timeline retains the submitted duration.
    let audio_track = mp4_boxes(movie)
        .into_iter()
        .filter(|(t, _)| t == b"trak")
        .find(|(_, t)| &child(child(t, b"mdia"), b"hdlr")[8..12] == b"soun")
        .unwrap()
        .1;
    let stored = track_samples(&bytes, audio_track);
    let submitted: Vec<&[u8]> = audio_packets.iter().map(|p| p.chunk.bytes()).collect();
    assert!(
        stored.ends_with(&submitted),
        "muxer must preserve encoded AAC bytes in order"
    );
    assert!(
        bytes.windows(4).any(|w| w == b"esds"),
        "AAC codec configuration must be present"
    );
    let header = child(movie, b"mvhd");
    let (scale, duration) = if header[0] == 0 {
        (
            u32::from_be_bytes(header[12..16].try_into().unwrap()),
            u64::from(u32::from_be_bytes(header[16..20].try_into().unwrap())),
        )
    } else {
        (
            u32::from_be_bytes(header[20..24].try_into().unwrap()),
            u64::from_be_bytes(header[24..32].try_into().unwrap()),
        )
    };
    let expected_end = tracks
        .iter()
        .flatten()
        .map(|p| p.timestamp() + p.duration().unwrap_or(33_333) as i64 - timestamp_origin)
        .max()
        .unwrap();
    let actual_end = (duration * 1_000_000 / u64::from(scale)) as i64;
    assert!(
        (actual_end - expected_end).abs() <= 2000,
        "movie timeline changed: {actual_end} versus {expected_end}"
    );
    let cancelled = temp.0.join("cancelled.mp4");
    let mut aborted = Muxer::<NativeMuxer>::create(&cancelled, config.clone(), 2_000_000).unwrap();
    aborted.close();
    assert!(!cancelled.exists());
    assert!(Muxer::<NativeMuxer>::create(&path, config, 2_000_000).is_err());
    assert_eq!(
        std::fs::read_dir(&temp.0).unwrap().count(),
        1,
        "private staging files must be removed"
    );
}

fn mp4_boxes(mut bytes: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut result = Vec::new();
    while !bytes.is_empty() {
        assert!(bytes.len() >= 8, "truncated MP4 box");
        let size = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
        let (size, header) = match size {
            0 => (bytes.len(), 8),
            1 => {
                assert!(bytes.len() >= 16);
                (
                    u64::from_be_bytes(bytes[8..16].try_into().unwrap()) as usize,
                    16,
                )
            }
            n => (n, 8),
        };
        assert!(
            size >= header && size <= bytes.len(),
            "invalid MP4 box span"
        );
        result.push((bytes[4..8].try_into().unwrap(), &bytes[header..size]));
        bytes = &bytes[size..];
    }
    result
}
fn sample_counts(bytes: &[u8], counts: &mut Vec<u32>) {
    for (kind, data) in mp4_boxes(bytes) {
        match &kind {
            b"trak" | b"mdia" | b"minf" | b"stbl" => sample_counts(data, counts),
            b"stsz" => {
                assert!(data.len() >= 12);
                counts.push(u32::from_be_bytes(data[8..12].try_into().unwrap()));
            }
            _ => {}
        }
    }
}

fn child<'a>(bytes: &'a [u8], name: &[u8; 4]) -> &'a [u8] {
    mp4_boxes(bytes)
        .into_iter()
        .find(|(t, _)| t == name)
        .expect("required MP4 box")
        .1
}
fn track_samples<'a>(file: &'a [u8], track: &[u8]) -> Vec<&'a [u8]> {
    let table = child(child(child(track, b"mdia"), b"minf"), b"stbl");
    let sizes = child(table, b"stsz");
    let fixed = u32::from_be_bytes(sizes[4..8].try_into().unwrap()) as usize;
    let count = u32::from_be_bytes(sizes[8..12].try_into().unwrap()) as usize;
    let sizes: Vec<usize> = if fixed > 0 {
        vec![fixed; count]
    } else {
        sizes[12..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|v| u32::from_be_bytes(*v) as usize)
            .collect()
    };
    assert_eq!(sizes.len(), count);
    let offsets = mp4_boxes(table)
        .into_iter()
        .find(|(t, _)| t == b"stco" || t == b"co64")
        .unwrap();
    let offsets: Vec<usize> = if offsets.0 == *b"stco" {
        offsets.1[8..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|v| u32::from_be_bytes(*v) as usize)
            .collect()
    } else {
        offsets.1[8..]
            .as_chunks::<8>()
            .0
            .iter()
            .map(|v| u64::from_be_bytes(*v) as usize)
            .collect()
    };
    let mapping: Vec<(usize, usize)> = child(table, b"stsc")[8..]
        .as_chunks::<12>()
        .0
        .iter()
        .map(|v| {
            (
                u32::from_be_bytes(v[..4].try_into().unwrap()) as usize,
                u32::from_be_bytes(v[4..8].try_into().unwrap()) as usize,
            )
        })
        .collect();
    let mut samples = Vec::new();
    for (index, mut offset) in offsets.into_iter().enumerate() {
        let per_chunk = mapping
            .iter()
            .rev()
            .find(|(start, _)| *start <= index + 1)
            .unwrap()
            .1;
        for _ in 0..per_chunk {
            let size = sizes[samples.len()];
            samples.push(&file[offset..offset + size]);
            offset += size;
        }
    }
    assert_eq!(samples.len(), count);
    samples
}
