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
