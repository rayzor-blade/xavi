//! Maintainer smoke test for native playback; applications use generated APIs.
use std::time::{Duration, Instant};
use xavi_platform::player::{PlaybackState, Player};

fn pump() {
    #[cfg(target_vendor = "apple")]
    unsafe {
        unsafe extern "C" {
            static kCFRunLoopDefaultMode: *const std::ffi::c_void;
            fn CFRunLoopRunInMode(
                mode: *const std::ffi::c_void,
                seconds: f64,
                return_after: bool,
            ) -> i32;
        }
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.005, false);
    }
    #[cfg(not(target_vendor = "apple"))]
    std::thread::sleep(Duration::from_millis(5));
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .expect("playback <local movie> [frame.ppm]");
    let capture = std::env::args().nth(2);
    let mut player = Player::open(path)?;
    let mut equalizer = xavi_core::equalizer::Settings::new(3)?;
    equalizer.set_band(0, 100.0, 6.0, 0.7)?;
    equalizer.set_band(1, 1000.0, -2.0, 1.0)?;
    equalizer.set_band(2, 8000.0, 3.0, 0.7)?;
    equalizer.set_preamp(-9.0)?;
    player.set_equalizer(equalizer)?;
    player.play()?;
    let started = Instant::now();
    let mut frames = 0;
    let mut phase = 0;
    let mut at = Instant::now();
    let mut target = 0.0;
    let mut latest = 0.0;
    let mut seek_frames = 0;
    loop {
        pump();
        if player.poll_frame()? {
            let frame = player.take_frame()?;
            latest = frame.info().timestamp as f64 / 1e6;
            frames += 1;
            if frames == 1 {
                println!("FRAME {:?}", frame.info());
                if let Some(path) = &capture {
                    use std::io::Write;
                    let info = frame.info();
                    let stride = frame.layout().planes()[0].layout.stride as usize;
                    let mut image = std::io::BufWriter::new(std::fs::File::create(path)?);
                    write!(
                        image,
                        "P6\n{} {}\n255\n",
                        info.coded_width, info.coded_height
                    )?;
                    for row in 0..info.coded_height as usize {
                        for x in 0..info.coded_width as usize {
                            let offset = row * stride + x * 4;
                            let bgra = &frame.bytes()[offset..offset + 4];
                            image.write_all(&[bgra[2], bgra[1], bgra[0]])?;
                        }
                    }
                }
            }
        }
        let info = player.info()?;
        match phase {
            0 if frames >= 5 && info.position > 0.2 => {
                equalizer.set_bypass(true);
                player.set_equalizer(equalizer)?;
                assert!(player.seek(-1.0).is_err());
                assert!(player.set_volume(2.0).is_err());
                player.pause()?;
                target = player.info()?.position;
                at = Instant::now();
                phase = 1;
            }
            1 if at.elapsed() > Duration::from_millis(300) => {
                assert!((info.position - target).abs() < 0.12, "pause: {info:?}");
                target = info.duration / 2.0;
                assert!(target > 0.5);
                player.seek(target)?;
                seek_frames = frames;
                player.set_volume(0.0)?;
                phase = 2;
            }
            2 if frames > seek_frames && info.state != PlaybackState::Buffering => {
                assert!(
                    (latest - target).abs() < 0.2,
                    "seek frame {latest} target {target}"
                );
                assert_eq!(info.volume, 0.0);
                player.set_volume(1.0)?;
                player.clear_equalizer()?;
                equalizer.set_bypass(false);
                player.set_equalizer(equalizer)?;
                player.play()?;
                at = Instant::now();
                phase = 3;
            }
            3 if at.elapsed() > Duration::from_millis(500) => {
                assert!(info.position > target + 0.2, "resume: {info:?}");
                player.seek((info.duration - 0.3).max(0.0))?;
                phase = 4;
            }
            4 if info.state == PlaybackState::Ended => {
                println!(
                    "PLAYBACK PASS: {frames} frames, pause, seek, volume, equalizer, resume, EOF"
                );
                break;
            }
            _ => {}
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "timeout phase {phase}: {info:?}, frames {frames}"
        );
    }
    Ok(())
}
