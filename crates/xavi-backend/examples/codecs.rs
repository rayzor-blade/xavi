//! Stream generated PCM through the system AAC encoder with bounded queues.
//! cargo run -p xavi-backend --example codecs
//! Linux requires the system GStreamer plugins listed in xavi-platform's docs.
use std::sync::Arc;
use std::time::{Duration, Instant};
use xavi_backend::codec::{AAC, AudioEncodeSession, AudioEncoderConfig, FlushToken};
use xavi_backend::stream::Limits;
use xavi_backend::{AudioData, AudioDescriptor, AudioSampleFormat, Error, ErrorKind, Result};

fn collect(encoder: &mut AudioEncodeSession, packets: &mut usize, bytes: &mut usize) -> Result<()> {
    encoder.pump(32)?;
    while let Some(output) = encoder.next_output() {
        if let Some(config) = output.decoder_config {
            println!("Decoder configuration: {config:?}");
        }
        *packets += 1;
        *bytes += output.chunk.bytes().len();
        // Hand output.chunk to a bounded transport or native container writer.
        // An AAC access unit by itself is not a complete media file.
    }
    Ok(())
}
fn wait(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(Error::new(
            ErrorKind::InvalidState,
            "native encoder timed out",
        ));
    }
    // A host executor can schedule the next pump instead of blocking a thread.
    std::thread::sleep(Duration::from_millis(1));
    Ok(())
}
fn flush(
    encoder: &mut AudioEncodeSession,
    token: FlushToken,
    packets: &mut usize,
    bytes: &mut usize,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !encoder.flush_complete(token)? {
        collect(encoder, packets, bytes)?;
        wait(deadline)?;
    }
    Ok(())
}
fn main() -> Result<()> {
    // Construct and drive on one worker thread, outside the runtime event loop.
    let mut encoder = AudioEncodeSession::new(
        Limits {
            max_items: 4,
            max_bytes: 256 * 1024,
        },
        Limits {
            max_items: 2,
            max_bytes: 64 * 1024,
        },
    )?;
    encoder.configure(AudioEncoderConfig {
        codec: AAC.into(),
        sample_rate: 48_000,
        channels: 1,
        bitrate: Some(96_000),
    })?;
    let (mut packets, mut bytes) = (0, 0);
    for block in 0..32 {
        let samples: Vec<u8> = (0..1024)
            .flat_map(|i| {
                let phase = (block * 1024 + i) as f64 * 440.0 * std::f64::consts::TAU / 48_000.0;
                ((phase.sin() * 8_000.0) as i16).to_ne_bytes()
            })
            .collect();
        let mut input = Arc::new(AudioData::new(
            AudioDescriptor {
                format: AudioSampleFormat::S16,
                sample_rate: 48_000.0,
                number_of_frames: 1024,
                number_of_channels: 1,
                timestamp: i64::from(block) * 1024 * 1_000_000 / 48_000,
            },
            &samples,
        )?);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match encoder.try_submit(input) {
                Ok(()) => break,
                Err(rejected) if rejected.error.kind == ErrorKind::WouldBlock => {
                    input = rejected.value
                }
                Err(rejected) => return Err(rejected.error),
            }
            collect(&mut encoder, &mut packets, &mut bytes)?;
            wait(deadline)?;
        }
        collect(&mut encoder, &mut packets, &mut bytes)?;
    }
    let token = encoder.begin_flush()?;
    flush(&mut encoder, token, &mut packets, &mut bytes)?;
    println!("Encoded 32768 PCM frames into {packets} AAC access units ({bytes} bytes).");
    encoder.close();
    Ok(())
}
