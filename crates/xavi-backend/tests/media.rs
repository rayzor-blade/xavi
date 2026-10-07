use std::sync::Arc;

use xavi_backend::stream::{Limits, Read, channel};
use xavi_backend::*;

fn audio_descriptor() -> AudioDescriptor {
    AudioDescriptor {
        format: AudioSampleFormat::U8,
        sample_rate: 48_000.0,
        number_of_frames: 2,
        number_of_channels: 1,
        timestamp: -10,
    }
}

#[test]
fn closing_a_handle_preserves_clones_and_work_already_in_flight() {
    let backend = MediaBackend::new();
    let handle = backend.create_audio(audio_descriptor(), &[4, 5]).unwrap();
    let duplicate = backend.clone_audio(handle).unwrap();
    let retained = backend.audio(handle).unwrap();
    assert!(Arc::ptr_eq(&retained, &backend.audio(duplicate).unwrap()));
    backend.release(handle).unwrap();
    backend.release(handle).unwrap();
    let reused = backend.create_audio(audio_descriptor(), &[6, 7]).unwrap();
    assert_eq!(
        backend.audio(handle).unwrap_err().kind,
        ErrorKind::InvalidState
    );
    let mut out = [0; 2];
    backend
        .copy_audio(duplicate, &mut out, AudioCopyOptions::default())
        .unwrap();
    assert_eq!(out, [4, 5]);
    backend.release(duplicate).unwrap();
    assert_eq!(retained.bytes(), [4, 5]);
    backend
        .copy_audio(reused, &mut out, AudioCopyOptions::default())
        .unwrap();
    assert_eq!(out, [6, 7]);
    backend.release(reused).unwrap();
    assert_eq!(backend.live_resources().unwrap(), 0);
}

#[test]
fn resources_are_typed_and_failed_creation_does_not_leak() {
    let backend = MediaBackend::new();
    assert!(backend.create_audio(audio_descriptor(), &[0]).is_err());
    let video = backend
        .create_video(
            VideoDescriptor::new(VideoPixelFormat::Rgba, 1, 1, 0),
            &[1, 2, 3, 4],
            None,
        )
        .unwrap();
    let clone = backend.clone_video(video).unwrap();
    assert_eq!(
        backend.audio(video).unwrap_err().kind,
        ErrorKind::InvalidState
    );
    assert!(backend.clone_audio(video).is_err());
    backend.release(video).unwrap();
    let mut out = [0; 4];
    backend
        .copy_video(clone, &mut out, &VideoCopyOptions::default())
        .unwrap();
    assert_eq!(out, [1, 2, 3, 4]);
    backend.release(clone).unwrap();
    assert_eq!(backend.live_resources().unwrap(), 0);
}

#[test]
fn chunks_preserve_timing_kind_and_unknown_duration() {
    let backend = MediaBackend::new();
    let audio = backend
        .create_audio_chunk(EncodedChunkType::Key, -5, None, &[1, 2, 3])
        .unwrap();
    let video = backend
        .create_video_chunk(EncodedChunkType::Delta, -7, Some(0), &[])
        .unwrap();
    let chunk = backend.audio_chunk(audio).unwrap();
    assert_eq!(chunk.timestamp(), -5);
    assert_eq!(chunk.duration(), None);
    assert_eq!(chunk.byte_len(), 3);
    assert_eq!(chunk.kind(), EncodedChunkType::Key);
    let mut short = [99; 2];
    assert!(backend.copy_audio_chunk(audio, &mut short).is_err());
    assert_eq!(short, [99; 2]);
    let mut out = [99; 5];
    backend.copy_audio_chunk(audio, &mut out).unwrap();
    assert_eq!(out, [1, 2, 3, 99, 99]);
    assert_eq!(backend.video_chunk(video).unwrap().duration(), Some(0));
    assert!(backend.audio_chunk(video).is_err());
    backend.copy_video_chunk(video, &mut []).unwrap();
    backend.release(audio).unwrap();
    backend.release(video).unwrap();
    assert_eq!(chunk.bytes(), [1, 2, 3]);
    assert_eq!(backend.live_resources().unwrap(), 0);
}

#[test]
fn a_stream_retains_frames_across_release_and_thread_handoff() {
    let backend = MediaBackend::new();
    let handle = backend.create_audio(audio_descriptor(), &[9, 8]).unwrap();
    let (mut producer, mut consumer) = channel(Limits {
        max_items: 2,
        max_bytes: 4,
    })
    .unwrap();
    producer.try_send(backend.audio(handle).unwrap()).unwrap();
    backend.release(handle).unwrap();
    assert_eq!(backend.live_resources().unwrap(), 0);
    let worker = std::thread::spawn(move || {
        producer.finish().unwrap();
    });
    worker.join().unwrap();
    let Read::Item(frame) = consumer.try_next().unwrap() else {
        panic!("expected queued frame")
    };
    assert_eq!(frame.bytes(), [9, 8]);
    assert_eq!(frame.descriptor().timestamp, -10);
    assert!(matches!(consumer.try_next().unwrap(), Read::End));
}

#[test]
fn input_can_arrive_as_incremental_nonseekable_bytes() {
    let (mut source, mut input) = channel(Limits {
        max_items: 2,
        max_bytes: 4,
    })
    .unwrap();
    let fragments = [vec![1, 2, 3], vec![4], vec![5, 6]];
    let mut output = Vec::new();
    for fragment in fragments {
        source.try_send(fragment).unwrap();
        if let Read::Item(bytes) = input.try_next().unwrap() {
            output.extend(bytes);
        }
    }
    drop(source);
    assert!(matches!(input.try_next().unwrap(), Read::End));
    assert_eq!(output, [1, 2, 3, 4, 5, 6]);
}
