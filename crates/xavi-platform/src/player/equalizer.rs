//! Playback control snapshots are copied between blocks. Audio callbacks never
//! wait for the application thread and never call into a guest VM.
use std::sync::{
    Arc, Mutex, TryLockError,
    atomic::{AtomicBool, Ordering},
};
use xavi_core::{
    Error, ErrorKind, Result,
    equalizer::{Processor, Settings},
};

#[derive(Clone, Default)]
pub(crate) struct Control {
    value: Arc<Mutex<(Settings, u64)>>,
    failed: Arc<AtomicBool>,
}
impl Control {
    pub fn set(&self, settings: Settings) -> Result<()> {
        self.check()?;
        self.value
            .lock()
            .map_err(|_| Error::new(ErrorKind::InvalidState, "equalizer settings lock poisoned"))?
            .0 = settings;
        Ok(())
    }
    pub fn reset(&self) {
        if let Ok(mut v) = self.value.lock() {
            v.1 = v.1.wrapping_add(1);
        }
    }
    pub fn fail(&self) {
        self.failed.store(true, Ordering::Release);
    }
    pub fn check(&self) -> Result<()> {
        if self.failed.load(Ordering::Acquire) {
            Err(Error::new(
                ErrorKind::InvalidState,
                "native playback equalizer could not process PCM",
            ))
        } else {
            Ok(())
        }
    }
}

pub(crate) struct Stream {
    pub control: Control,
    pub processor: Processor,
    settings: Settings,
    epoch: u64,
}
impl Stream {
    pub fn new(control: Control) -> Self {
        Self {
            control,
            processor: Processor::default(),
            settings: Settings::default(),
            epoch: 0,
        }
    }
    pub fn prepare(&mut self, rate: f64, channels: usize, discontinuity: bool) -> Result<()> {
        match self.control.value.try_lock() {
            Ok(value) => {
                self.settings = value.0;
                if self.epoch != value.1 {
                    self.processor.reset();
                    self.epoch = value.1;
                }
            }
            Err(TryLockError::WouldBlock) => {} // Apply new controls on the next block.
            Err(TryLockError::Poisoned(_)) => {
                return Err(Error::new(
                    ErrorKind::InvalidState,
                    "equalizer settings lock poisoned",
                ));
            }
        }
        if discontinuity {
            self.processor.reset();
        }
        let result = self.processor.prepare(self.settings, rate, channels);
        if result.is_err() {
            self.control.fail();
        }
        result
    }
    pub fn interleaved(
        &mut self,
        bytes: &mut [u8],
        rate: f64,
        channels: usize,
        float: bool,
        discontinuity: bool,
    ) -> Result<()> {
        self.prepare(rate, channels, discontinuity)?;
        let unit = if float { 4 } else { 2 };
        if !bytes.len().is_multiple_of(channels * unit) {
            return Err(Error::invalid("partial PCM frame"));
        }
        for frame in bytes.chunks_exact_mut(channels * unit) {
            self.processor.begin_frame();
            for (channel, sample) in frame.chunks_exact_mut(unit).enumerate() {
                if float {
                    let value = self
                        .processor
                        .sample(channel, f32::from_ne_bytes(sample.try_into().unwrap()));
                    if !value.is_finite() {
                        return Err(Error::invalid("equalizer output exceeds finite F32 range"));
                    }
                    sample.copy_from_slice(&value.to_ne_bytes());
                } else {
                    let value = f32::from(i16::from_ne_bytes(sample.try_into().unwrap())) / 32768.0;
                    let value = self.processor.sample(channel, value);
                    sample.copy_from_slice(
                        &((value.clamp(-1.0, 1.0) * 32768.0).round() as i16).to_ne_bytes(),
                    );
                }
            }
        }
        Ok(())
    }
}

#[cfg(target_vendor = "apple")]
mod apple {
    use super::*;
    use std::ffi::c_void;
    #[repr(C)]
    struct Buffer {
        channels: u32,
        size: u32,
        data: *mut u8,
    }
    // These functions are private to apple.m. Each tap has its own Stream and
    // serialized callbacks; the tap's finalize callback owns its destruction.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn xavi_eq_stream_new(control: *const Control) -> *mut c_void {
        Box::into_raw(Box::new(Stream::new(unsafe { &*control }.clone()))).cast()
    }
    #[unsafe(no_mangle)]
    unsafe extern "C" fn xavi_eq_stream_fork(seed: *const Stream) -> *mut c_void {
        Box::into_raw(Box::new(Stream::new(unsafe { &*seed }.control.clone()))).cast()
    }
    #[unsafe(no_mangle)]
    unsafe extern "C" fn xavi_eq_stream_drop(stream: *mut Stream) {
        if !stream.is_null() {
            drop(unsafe { Box::from_raw(stream) });
        }
    }
    #[unsafe(no_mangle)]
    unsafe extern "C" fn xavi_eq_process(
        stream: *mut Stream,
        buffers: *mut Buffer,
        count: u32,
        frames: u32,
        rate: f64,
        channels: u32,
        float: bool,
        planar: bool,
        discontinuity: bool,
    ) -> bool {
        let stream = unsafe { &mut *stream };
        let result = (|| {
            stream.prepare(rate, channels as usize, discontinuity)?;
            let unit = if float { 4 } else { 2 };
            let expected = if planar { channels } else { 1 };
            if buffers.is_null() || count != expected {
                return Err(Error::invalid("unexpected audio buffer layout"));
            }
            let buffers = unsafe { std::slice::from_raw_parts_mut(buffers, count as usize) };
            for buffer in buffers.iter() {
                let n = if planar { 1 } else { channels };
                if buffer.channels != n
                    || buffer.data.is_null()
                    || u64::from(frames) * u64::from(n) * unit as u64 > u64::from(buffer.size)
                {
                    return Err(Error::invalid("invalid audio buffer"));
                }
            }
            if !planar {
                let bytes = unsafe {
                    std::slice::from_raw_parts_mut(
                        buffers[0].data,
                        frames as usize * channels as usize * unit,
                    )
                };
                return stream.interleaved(bytes, rate, channels as usize, float, false);
            }
            for frame in 0..frames as usize {
                stream.processor.begin_frame();
                for (channel, buffer) in buffers.iter_mut().enumerate() {
                    let bytes = unsafe {
                        std::slice::from_raw_parts_mut(buffer.data.add(frame * unit), unit)
                    };
                    let value = if float {
                        f32::from_ne_bytes(bytes.try_into().unwrap())
                    } else {
                        f32::from(i16::from_ne_bytes(bytes.try_into().unwrap())) / 32768.0
                    };
                    let value = stream.processor.sample(channel, value);
                    if !value.is_finite() {
                        return Err(Error::invalid("equalizer output exceeds finite F32 range"));
                    }
                    if float {
                        bytes.copy_from_slice(&value.to_ne_bytes());
                    } else {
                        bytes.copy_from_slice(
                            &((value.clamp(-1.0, 1.0) * 32768.0).round() as i16).to_ne_bytes(),
                        );
                    }
                }
            }
            Ok(())
        })();
        if result.is_err() {
            stream.control.fail();
        }
        result.is_ok()
    }
    #[test]
    fn apple_tap_handles_planar_pcm_and_rejects_short_buffers() {
        let control = Control::default();
        let mut settings = Settings::new(1).unwrap();
        settings.set_preamp(-6.020599913279624).unwrap();
        control.set(settings).unwrap();
        let mut stream = Stream::new(control.clone());
        let mut left = [1.0f32, 0.5];
        let mut right = [-0.5f32, 0.25];
        let mut buffers = [
            Buffer {
                channels: 1,
                size: 8,
                data: left.as_mut_ptr().cast(),
            },
            Buffer {
                channels: 1,
                size: 8,
                data: right.as_mut_ptr().cast(),
            },
        ];
        assert!(unsafe {
            xavi_eq_process(
                &mut stream,
                buffers.as_mut_ptr(),
                2,
                2,
                48000.0,
                2,
                true,
                true,
                true,
            )
        });
        assert_eq!(left, [0.5, 0.25]);
        assert_eq!(right, [-0.25, 0.125]);
        buffers[1].size = 4;
        assert!(!unsafe {
            xavi_eq_process(
                &mut stream,
                buffers.as_mut_ptr(),
                2,
                2,
                48000.0,
                2,
                true,
                true,
                false,
            )
        });
        assert!(control.check().is_err());
        assert_eq!(left, [0.5, 0.25]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn float_playback_preserves_headroom_until_the_output_device() {
        let control = Control::default();
        let mut settings = Settings::new(1).unwrap();
        settings.set_band(0, 1000.0, 12.0, 2.0).unwrap();
        control.set(settings).unwrap();
        let mut stream = Stream::new(control);
        let mut samples: Vec<u8> = (0..4800)
            .flat_map(|n| {
                (0.75 * (std::f32::consts::TAU * 1000.0 * n as f32 / 48000.0).sin()).to_ne_bytes()
            })
            .collect();
        stream
            .interleaved(&mut samples, 48000.0, 1, true, false)
            .unwrap();
        let peak = samples
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_ne_bytes(*b).abs())
            .fold(0.0f32, f32::max);
        assert!(peak > 2.9 && peak < 3.1, "unexpected clipping: {peak}");
    }
    #[test]
    fn callbacks_use_previous_controls_instead_of_waiting_for_a_writer() {
        let control = Control::default();
        let mut stream = Stream::new(control.clone());
        let _writing = control.value.lock().unwrap();
        let mut pcm = 0.5f32.to_ne_bytes();
        stream
            .interleaved(&mut pcm, 48000.0, 1, true, false)
            .unwrap();
        assert_eq!(f32::from_ne_bytes(pcm), 0.5);
    }
}
