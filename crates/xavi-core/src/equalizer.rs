//! Stateful parametric EQ using the peaking biquads from the W3C Audio EQ Cookbook:
//! <https://www.w3.org/TR/audio-eq-cookbook/>. No codec or platform dependencies.
use crate::{AudioCopyOptions, AudioData, AudioDescriptor, AudioSampleFormat, Error, Result};

pub const MAX_BANDS: usize = 16;
pub const MAX_CHANNELS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    pub frequency: f64,
    pub gain_db: f64,
    pub q: f64,
    pub enabled: bool,
}
impl Default for Band {
    fn default() -> Self {
        Self {
            frequency: 1000.0,
            gain_db: 0.0,
            q: 1.0,
            enabled: false,
        }
    }
}

/// Validated, fixed-size settings. A player copies these, never the filter history.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    bands: [Band; MAX_BANDS],
    count: usize,
    preamp_db: f64,
    bypass: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            bands: [Band::default(); MAX_BANDS],
            count: 0,
            preamp_db: 0.0,
            bypass: false,
        }
    }
}
impl Settings {
    pub fn new(bands: usize) -> Result<Self> {
        if !(1..=MAX_BANDS).contains(&bands) {
            return Err(Error::invalid("equalizer band count must be in 1..=16"));
        }
        Ok(Self {
            count: bands,
            ..Self::default()
        })
    }
    pub fn bands(&self) -> &[Band] {
        &self.bands[..self.count]
    }
    pub fn band(&self, index: usize) -> Result<Band> {
        self.bands()
            .get(index)
            .copied()
            .ok_or_else(|| Error::invalid("equalizer band index out of range"))
    }
    pub fn set_band(&mut self, index: usize, frequency: f64, gain_db: f64, q: f64) -> Result<()> {
        self.band(index)?;
        if !frequency.is_finite()
            || !(1.0..=96_000.0).contains(&frequency)
            || !gain_db.is_finite()
            || !(-24.0..=24.0).contains(&gain_db)
            || !q.is_finite()
            || !(0.1..=20.0).contains(&q)
        {
            return Err(Error::invalid(
                "equalizer requires frequency 1..=96000 Hz, gain -24..=24 dB and Q 0.1..=20",
            ));
        }
        self.bands[index] = Band {
            frequency,
            gain_db,
            q,
            enabled: true,
        };
        Ok(())
    }
    pub fn disable_band(&mut self, index: usize) -> Result<()> {
        self.band(index)?;
        self.bands[index].enabled = false;
        Ok(())
    }
    pub fn preamp_db(&self) -> f64 {
        self.preamp_db
    }
    pub fn set_preamp(&mut self, db: f64) -> Result<()> {
        if !db.is_finite() || !(-60.0..=0.0).contains(&db) {
            return Err(Error::invalid("equalizer preamp must be in -60..=0 dB"));
        }
        self.preamp_db = db;
        Ok(())
    }
    pub fn bypassed(&self) -> bool {
        self.bypass
    }
    pub fn set_bypass(&mut self, bypass: bool) {
        self.bypass = bypass;
    }
}

#[derive(Clone, Copy, Debug)]
struct Coefficients([f64; 5]);
impl Coefficients {
    const IDENTITY: Self = Self([1.0, 0.0, 0.0, 0.0, 0.0]);
    fn new(band: Band, rate: f64) -> Self {
        // A preset remains useful at a lower sample rate: unrepresentable bands
        // become identity filters instead of wrapping around Nyquist.
        if !band.enabled || band.gain_db == 0.0 || band.frequency >= rate / 2.0 {
            return Self::IDENTITY;
        }
        let omega = std::f64::consts::TAU * band.frequency / rate;
        let alpha = omega.sin() / (2.0 * band.q);
        let a = 10.0f64.powf(band.gain_db / 40.0);
        let a0 = 1.0 + alpha / a;
        Self([
            (1.0 + alpha * a) / a0,
            -2.0 * omega.cos() / a0,
            (1.0 - alpha * a) / a0,
            -2.0 * omega.cos() / a0,
            (1.0 - alpha / a) / a0,
        ])
    }
}

/// Allocation-free DSP after construction, owned by one audio processing thread.
/// Platform bridges prepare once per block and process each channel independently.
#[derive(Clone)]
pub struct Processor {
    settings: Settings,
    format: Option<(f64, usize)>,
    coefficients: [Coefficients; MAX_BANDS],
    target: [Coefficients; MAX_BANDS],
    history: [[[f64; 2]; MAX_CHANNELS]; MAX_BANDS],
    preamp: f64,
    wet: f64,
    transition: u32,
}
impl Default for Processor {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            format: None,
            coefficients: [Coefficients::IDENTITY; MAX_BANDS],
            target: [Coefficients::IDENTITY; MAX_BANDS],
            history: [[[0.0; 2]; MAX_CHANNELS]; MAX_BANDS],
            preamp: 1.0,
            wet: 1.0,
            transition: 0,
        }
    }
}
impl Processor {
    pub fn reset(&mut self) {
        self.format = None;
        self.history.fill([[0.0; 2]; MAX_CHANNELS]);
    }
    pub fn prepare(&mut self, settings: Settings, rate: f64, channels: usize) -> Result<()> {
        if !rate.is_finite()
            || !(1000.0..=384_000.0).contains(&rate)
            || !(1..=MAX_CHANNELS).contains(&channels)
        {
            return Err(Error::invalid(
                "equalizer requires 1000..=384000 Hz and 1..=32 channels",
            ));
        }
        let format_changed = self.format != Some((rate, channels));
        if !format_changed && self.settings == settings {
            return Ok(());
        }
        self.target = settings.bands.map(|b| Coefficients::new(b, rate));
        self.settings = settings;
        if format_changed {
            self.history.fill([[0.0; 2]; MAX_CHANNELS]);
            self.coefficients = self.target;
            self.preamp = 10.0f64.powf(settings.preamp_db / 20.0);
            self.wet = if settings.bypass { 0.0 } else { 1.0 };
            self.transition = 0;
        } else {
            // Smooth coefficient, headroom and bypass changes over 10 ms.
            self.transition = (rate * 0.01).ceil() as u32;
        }
        self.format = Some((rate, channels));
        Ok(())
    }
    pub fn begin_frame(&mut self) {
        if self.transition > 0 {
            let n = f64::from(self.transition);
            for (current, target) in self.coefficients.iter_mut().zip(&self.target) {
                for (a, b) in current.0.iter_mut().zip(target.0) {
                    *a += (b - *a) / n;
                }
            }
            self.preamp += (10.0f64.powf(self.settings.preamp_db / 20.0) - self.preamp) / n;
            self.wet += ((if self.settings.bypass { 0.0 } else { 1.0 }) - self.wet) / n;
            self.transition -= 1;
        }
    }
    /// Process one sample after `begin_frame`. Invalid native PCM is silenced,
    /// and never poisons subsequent filter history. Does not clip finite output.
    pub fn sample(&mut self, channel: usize, input: f32) -> f32 {
        if channel >= self.format.map_or(0, |(_, c)| c) {
            return 0.0;
        }
        if !input.is_finite() {
            for band in &mut self.history {
                band[channel] = [0.0; 2];
            }
            return 0.0;
        }
        let mut value = f64::from(input) * self.preamp;
        for (coeff, state) in self.coefficients.iter().zip(&mut self.history) {
            let [b0, b1, b2, a1, a2] = coeff.0;
            let z = &mut state[channel];
            let out = b0 * value + z[0];
            z[0] = b1 * value - a1 * out + z[1];
            z[1] = b2 * value - a2 * out;
            for v in z {
                if v.abs() < 1e-300 {
                    *v = 0.0;
                }
            }
            value = out;
        }
        // Explicit endpoints preserve exact unity/bypass, even if wet output
        // would overflow f32. Ordinary PCM has ample f64 processing headroom.
        if self.wet == 0.0 {
            return input;
        }
        (f64::from(input) * (1.0 - self.wet) + value * self.wet) as f32
    }
}

/// One independent PCM stream. Returned AudioData is interleaved F32; input
/// ownership, timestamps and channel count are preserved. Seek/gap/format changes
/// reset history automatically. Use reset for an explicit new segment.
#[derive(Clone)]
pub struct AudioEqualizer {
    pub settings: Settings,
    processor: Processor,
    next_timestamp: Option<i128>,
}
impl AudioEqualizer {
    pub fn new(bands: usize) -> Result<Self> {
        Ok(Self {
            settings: Settings::new(bands)?,
            processor: Processor::default(),
            next_timestamp: None,
        })
    }
    pub fn reset(&mut self) {
        self.processor.reset();
        self.next_timestamp = None;
    }
    pub fn process(&mut self, input: &AudioData) -> Result<AudioData> {
        let d = input.descriptor();
        let options = AudioCopyOptions {
            format: Some(AudioSampleFormat::F32),
            ..Default::default()
        };
        let size = input.allocation_size(options)? as usize;
        if size > 64 * 1024 * 1024 {
            return Err(Error::invalid("equalizer block exceeds 64 MiB"));
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(size)
            .map_err(|_| Error::exhausted())?;
        bytes.resize(size, 0);
        input.copy_to(&mut bytes, options)?;
        if bytes
            .as_chunks::<4>()
            .0
            .iter()
            .any(|b| !f32::from_ne_bytes(*b).is_finite())
        {
            return Err(Error::invalid("equalizer input contains non-finite PCM"));
        }
        // Failed validation/allocation/output never advances the stream state.
        let mut processor = self.processor.clone();
        if self
            .next_timestamp
            .is_some_and(|t| (t - i128::from(d.timestamp)).abs() > 2)
        {
            processor.reset();
        }
        let channels = d.number_of_channels as usize;
        processor.prepare(self.settings, f64::from(d.sample_rate), channels)?;
        for frame in bytes.chunks_exact_mut(channels * 4) {
            processor.begin_frame();
            for (channel, sample) in frame.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let value = processor.sample(channel, f32::from_ne_bytes(*sample));
                if !value.is_finite() {
                    return Err(Error::invalid("equalizer output exceeds finite F32 range"));
                }
                *sample = value.to_ne_bytes();
            }
        }
        let output = AudioData::new(
            AudioDescriptor {
                format: AudioSampleFormat::F32,
                ..d
            },
            &bytes,
        )?;
        self.processor = processor;
        self.next_timestamp = Some(i128::from(d.timestamp) + i128::from(input.duration()));
        Ok(output)
    }
}
