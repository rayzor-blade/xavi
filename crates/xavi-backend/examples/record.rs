//! Record a generated animation and tone incrementally using native H.264/AAC.
//! cargo run -p xavi-backend --example record -- /tmp/animation.mp4
//! The output must not exist. AAC priming/padding is included, not trimmed.
//! Use QuickTime or VLC for both tracks; VS Code's preview lacks AAC audio.
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use xavi_backend::codec::{
    AAC, AudioEncodeSession, AudioEncoderConfig, H264, VideoEncodeInput, VideoEncodeSession,
    VideoEncoderConfig,
};
use xavi_backend::mux::{Mp4Config, Mp4Writer, Track};
use xavi_backend::stream::Limits;
use xavi_backend::{
    AudioData, AudioDescriptor, AudioSampleFormat, Error, ErrorKind, Result, VideoDescriptor,
    VideoFrame, VideoPixelFormat,
};

const SECONDS: u32 = 3;
const SAMPLE_RATE: u32 = 48_000;
const AUDIO_BLOCK: u32 = 1024;
const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const FPS: u32 = 30;
const INPUT_ITEMS: usize = 4;

fn wait(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(Error::new(ErrorKind::InvalidState, "recording timed out"));
    }
    std::thread::sleep(Duration::from_millis(1));
    Ok(())
}

fn tone(offset: u32, frames: u32) -> Result<Arc<AudioData>> {
    let pcm: Vec<u8> = (offset..offset + frames)
        .flat_map(|sample| {
            let phase = f64::from(sample) * 440.0 * std::f64::consts::TAU / f64::from(SAMPLE_RATE);
            ((phase.sin() * 8000.0) as i16).to_ne_bytes()
        })
        .collect();
    Ok(Arc::new(AudioData::new(
        AudioDescriptor {
            format: AudioSampleFormat::S16,
            sample_rate: SAMPLE_RATE as f32,
            number_of_frames: frames,
            number_of_channels: 1,
            timestamp: i64::from(offset) * 1_000_000 / i64::from(SAMPLE_RATE),
        },
        &pcm,
    )?))
}

fn animation(index: u32) -> Result<VideoEncodeInput> {
    let progress = f64::from(index) / f64::from(SECONDS * FPS - 1);
    let cx = 40 + (progress * f64::from(WIDTH - 80)) as i32;
    let cy = HEIGHT as i32 / 2 + (60.0 * (progress * std::f64::consts::TAU).sin()) as i32;
    let mut pixels = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for y in 0..HEIGHT as i32 {
        for x in 0..WIDTH as i32 {
            let ball = (x - cx).pow(2) + (y - cy).pow(2) < 28 * 28;
            let bar = y >= HEIGHT as i32 - 18 && x <= cx;
            let bgra = if ball {
                [50, 170, 250, 255]
            } else if bar {
                [190, 220, 230, 255]
            } else {
                [70 + (x / 12) as u8, 30 + (y / 10) as u8, 20, 255]
            };
            pixels.extend_from_slice(&bgra);
        }
    }
    // Derive adjacent timestamps from the frame clock to avoid rounding drift.
    let timestamp = i64::from(index) * 1_000_000 / i64::from(FPS);
    let end = i64::from(index + 1) * 1_000_000 / i64::from(FPS);
    let mut descriptor = VideoDescriptor::new(VideoPixelFormat::Bgra, WIDTH, HEIGHT, timestamp);
    descriptor.duration = Some((end - timestamp) as u64);
    Ok(VideoEncodeInput {
        frame: Arc::new(VideoFrame::new(descriptor, &pixels, None)?),
        key_frame: index.is_multiple_of(FPS),
    })
}

fn main() -> Result<()> {
    let path = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or_else(|| Error::invalid("usage: record <new-output.mp4>"))?,
    );
    let mut audio = AudioEncodeSession::new(
        Limits {
            max_items: INPUT_ITEMS,
            max_bytes: INPUT_ITEMS * AUDIO_BLOCK as usize * 2,
        },
        Limits {
            max_items: 2,
            max_bytes: 64 * 1024,
        },
    )?;
    audio.configure(AudioEncoderConfig {
        codec: AAC.into(),
        sample_rate: SAMPLE_RATE,
        channels: 1,
        bitrate: Some(96_000),
    })?;
    let mut video = VideoEncodeSession::new(
        Limits {
            max_items: INPUT_ITEMS,
            max_bytes: INPUT_ITEMS * (WIDTH * HEIGHT * 4) as usize,
        },
        Limits {
            max_items: 2,
            max_bytes: 2 * 1024 * 1024,
        },
    )?;
    video.configure(VideoEncoderConfig {
        codec: H264.into(),
        width: WIDTH,
        height: HEIGHT,
        bitrate: 1_500_000,
        framerate: f64::from(FPS),
    })?;

    let mut writer: Option<Mp4Writer> = None;
    let (mut audio_offset, mut video_index) = (0, 0);
    let (mut audio_packet, mut video_packet) = (None, None);
    let (mut audio_flush, mut video_flush) = (None, None);
    let (mut audio_ended, mut video_ended) = (false, false);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !audio_ended || !video_ended {
        // Each input fits one queue slot and its corresponding byte budget.
        // Generate only when there is space, so raw media stays bounded too.
        if audio_offset < SECONDS * SAMPLE_RATE && audio.queue_size() < INPUT_ITEMS {
            let frames = AUDIO_BLOCK.min(SECONDS * SAMPLE_RATE - audio_offset);
            audio
                .try_submit(tone(audio_offset, frames)?)
                .map_err(|rejected| rejected.error)?;
            audio_offset += frames;
        }
        if video_index < SECONDS * FPS && video.queue_size() < INPUT_ITEMS {
            video
                .try_submit(animation(video_index)?)
                .map_err(|rejected| rejected.error)?;
            video_index += 1;
        }
        if audio_offset == SECONDS * SAMPLE_RATE && audio_flush.is_none() {
            audio_flush = Some(audio.begin_flush()?);
        }
        if video_index == SECONDS * FPS && video_flush.is_none() {
            video_flush = Some(video.begin_flush()?);
        }
        audio.pump(32)?;
        video.pump(32)?;
        // Keep at most one unwritten packet per track outside the codec queues.
        if audio_packet.is_none() {
            audio_packet = audio.next_output();
        }
        if video_packet.is_none() {
            video_packet = video.next_output();
        }
        if writer.is_none()
            && let (Some(a), Some(v)) = (&audio_packet, &video_packet)
        {
            writer = Some(Mp4Writer::create(
                &path,
                Mp4Config {
                    audio: Some(a.decoder_config.clone().ok_or_else(|| {
                        Error::invalid("first audio packet has no track configuration")
                    })?),
                    video: Some(v.decoder_config.clone().ok_or_else(|| {
                        Error::invalid("first video packet has no track configuration")
                    })?),
                    timestamp_origin: a.chunk.timestamp().min(v.chunk.timestamp()),
                    default_video_duration: None,
                },
                2 * 1024 * 1024,
            )?);
        }
        if let Some(writer) = writer.as_mut() {
            // Prefer the earlier packet. If its input is backpressured, offer
            // the other track as well so the native interleaver can advance.
            let order = match (&audio_packet, &video_packet) {
                (Some(a), Some(v)) if v.chunk.timestamp() < a.chunk.timestamp() => {
                    [Track::Video, Track::Audio]
                }
                _ => [Track::Audio, Track::Video],
            };
            for track in order {
                let packet = match track {
                    Track::Audio => audio_packet.as_ref().map(|p| &p.chunk),
                    Track::Video => video_packet.as_ref().map(|p| &p.chunk),
                };
                if let Some(packet) = packet
                    && writer.write(track, packet)?
                {
                    match track {
                        Track::Audio => audio_packet = None,
                        Track::Video => video_packet = None,
                    }
                    break;
                }
            }
            // A flushed codec may still have a packet retained by this loop.
            // End each track only after that last packet has reached the muxer.
            if !audio_ended
                && audio_packet.is_none()
                && let Some(token) = audio_flush
                && audio.flush_complete(token)?
            {
                writer.end_track(Track::Audio)?;
                audio_ended = true;
            }
            if !video_ended
                && video_packet.is_none()
                && let Some(token) = video_flush
                && video.flush_complete(token)?
            {
                writer.end_track(Track::Video)?;
                video_ended = true;
            }
        }
        wait(deadline)?;
    }
    let mut writer = writer.ok_or_else(|| Error::invalid("encoders produced no packets"))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !writer.finish()? {
        wait(deadline)?;
    }
    println!(
        "Recorded {SECONDS}s of {WIDTH}x{HEIGHT} H.264 video at {FPS} fps with an AAC tone: {}",
        path.display()
    );
    println!("Open with QuickTime or VLC for video and sound; VS Code's preview lacks AAC audio.");
    Ok(())
}
