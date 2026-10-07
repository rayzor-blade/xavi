//! Run with `cargo run -p xavi-backend --example stream`.
//! A capture or decode task can use Sender::send to await capacity; a runtime
//! adapter can use Receiver::next and map completion onto its future carrier.

use xavi_backend::stream::{Limits, Read, channel};
use xavi_backend::*;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let backend = MediaBackend::new();
    let descriptor = AudioDescriptor {
        format: AudioSampleFormat::U8,
        sample_rate: 48_000.0,
        number_of_frames: 3,
        number_of_channels: 2,
        timestamp: 0,
    };
    let handle = backend.create_audio(descriptor, &[0, 128, 64, 192, 128, 255])?;
    let (mut capture, mut playback) = channel(Limits {
        max_items: 8,
        max_bytes: 64 * 1024,
    })?;
    capture
        .try_send(backend.audio(handle)?)
        .map_err(|e| e.error)?;
    backend.release(handle)?;
    capture.finish()?;

    while let Read::Item(frame) = playback.try_next()? {
        let options = AudioCopyOptions {
            plane_index: 1,
            format: Some(AudioSampleFormat::F32Planar),
            ..Default::default()
        };
        let mut bytes = vec![0; frame.allocation_size(options)? as usize];
        frame.copy_to(&mut bytes, options)?;
        let samples: Vec<_> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_ne_bytes(*b))
            .collect();
        println!(
            "PTS {} us, channel 1: {samples:?}",
            frame.descriptor().timestamp
        );
    }
    assert_eq!(backend.live_resources()?, 0);
    Ok(())
}
