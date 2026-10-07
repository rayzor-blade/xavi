use xavi_core::{AudioData, AudioDescriptor, AudioSampleFormat, equalizer::AudioEqualizer};

fn audio(samples: &[f32], channels: u32, timestamp: i64) -> AudioData {
    let bytes: Vec<_> = samples.iter().flat_map(|x| x.to_ne_bytes()).collect();
    AudioData::new(
        AudioDescriptor {
            format: AudioSampleFormat::F32,
            sample_rate: 48000.0,
            number_of_channels: channels,
            number_of_frames: samples.len() as u32 / channels,
            timestamp,
        },
        &bytes,
    )
    .unwrap()
}
fn samples(data: &AudioData) -> Vec<f32> {
    data.bytes()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_ne_bytes(*b))
        .collect()
}
fn tone(frequency: f64, frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|n| (0.1 * (std::f64::consts::TAU * frequency * n as f64 / 48000.0).sin()) as f32)
        .collect()
}
fn power(data: &[f32]) -> f64 {
    data.iter().map(|&x| f64::from(x).powi(2)).sum::<f64>() / data.len() as f64
}

#[test]
fn bands_boost_and_cut_the_selected_frequency() {
    for gain in [-12.0, 6.0, 24.0] {
        for frequency in [100.0, 1000.0, 10000.0] {
            let input = tone(frequency, 24000);
            let mut eq = AudioEqualizer::new(1).unwrap();
            eq.settings.set_band(0, 1000.0, gain, 2.0).unwrap();
            let output = samples(&eq.process(&audio(&input, 1, 0)).unwrap());
            let db = 10.0 * (power(&output[12000..]) / power(&input[12000..])).log10();
            if frequency == 1000.0 {
                assert!((db - gain).abs() < 0.01, "{db} vs {gain}");
            } else {
                assert!(db.abs() < 0.5, "out-of-band gain {db}");
            }
        }
    }
}

#[test]
fn chunk_boundaries_do_not_change_output_or_mix_channels() {
    let mut input = vec![0.0; 16000];
    input[0] = 0.5;
    let mut whole = AudioEqualizer::new(2).unwrap();
    whole.settings.set_band(0, 400.0, 12.0, 3.0).unwrap();
    whole.settings.set_band(1, 8000.0, -8.0, 0.7).unwrap();
    let mut incremental = whole.clone();
    let expected = samples(&whole.process(&audio(&input, 2, -100)).unwrap());
    let mut got = Vec::new();
    for (index, block) in input.chunks(254).enumerate() {
        let pts = -100 + (index * 127 * 1_000_000 / 48000) as i64;
        got.extend(samples(
            &incremental.process(&audio(block, 2, pts)).unwrap(),
        ));
    }
    assert_eq!(got, expected);
    assert!(got.iter().skip(2).step_by(2).any(|x| x.abs() > 1e-6));
    assert!(got.iter().skip(1).step_by(2).all(|x| *x == 0.0));
}

#[test]
fn unity_bypass_preamp_and_source_ownership() {
    let input = audio(&[0.0, 0.25, -0.5, 2.0], 1, i64::MAX - 1);
    let mut eq = AudioEqualizer::new(3).unwrap();
    assert_eq!(eq.process(&input).unwrap().bytes(), input.bytes());
    eq.reset();
    eq.settings.set_preamp(-6.020599913279624).unwrap();
    assert_eq!(
        samples(&eq.process(&input).unwrap()),
        [0.0, 0.125, -0.25, 1.0]
    );
    eq.settings.set_band(1, 1000.0, 24.0, 1.0).unwrap();
    eq.settings.set_bypass(true);
    eq.reset();
    let out = eq.process(&input).unwrap();
    assert_eq!(out.bytes(), input.bytes());
    assert_eq!(out.descriptor(), input.descriptor());
    assert_eq!(samples(&input), [0.0, 0.25, -0.5, 2.0]);
}

#[test]
fn invalid_input_and_settings_leave_history_unchanged() {
    let mut eq = AudioEqualizer::new(1).unwrap();
    eq.settings.set_band(0, 500.0, 6.0, 2.0).unwrap();
    eq.process(&audio(&[1.0, 0.0], 1, 0)).unwrap();
    let mut reference = eq.clone();
    for params in [
        (0.0, 0.0, 1.0),
        (100.0, f64::NAN, 1.0),
        (100.0, 25.0, 1.0),
        (100.0, 1.0, 0.0),
    ] {
        assert!(
            eq.settings
                .set_band(0, params.0, params.1, params.2)
                .is_err()
        );
    }
    assert!(eq.settings.set_band(1, 100.0, 1.0, 1.0).is_err());
    assert!(eq.settings.set_preamp(1.0).is_err());
    assert!(eq.process(&audio(&[f32::NAN], 1, 41)).is_err());
    assert!(eq.process(&audio(&[f32::INFINITY], 1, 41)).is_err());
    let next = audio(&[0.0; 200], 1, 41);
    assert_eq!(
        eq.process(&next).unwrap().bytes(),
        reference.process(&next).unwrap().bytes()
    );
    assert!(AudioEqualizer::new(0).is_err());
    assert!(AudioEqualizer::new(17).is_err());
}

#[test]
fn seeks_and_explicit_resets_drop_previous_history() {
    let mut eq = AudioEqualizer::new(1).unwrap();
    eq.settings.set_band(0, 100.0, 24.0, 10.0).unwrap();
    let impulse = audio(&[0.5, 0.0, 0.0], 1, 0);
    let first = eq.process(&impulse).unwrap();
    assert_eq!(eq.process(&impulse).unwrap().bytes(), first.bytes()); // seek backwards
    eq.reset();
    assert_eq!(eq.process(&impulse).unwrap().bytes(), first.bytes());
    assert!(
        samples(&eq.process(&audio(&[0.0; 20], 1, 1_000_000)).unwrap())
            .iter()
            .all(|x| *x == 0.0)
    );
}

#[test]
fn planar_input_and_bands_above_nyquist_are_supported() {
    let data = AudioData::new(
        AudioDescriptor {
            format: AudioSampleFormat::S16Planar,
            sample_rate: 8000.0,
            number_of_channels: 2,
            number_of_frames: 2,
            timestamp: 9,
        },
        &[16384i16, -16384, 8192, -8192]
            .into_iter()
            .flat_map(i16::to_ne_bytes)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let mut eq = AudioEqualizer::new(1).unwrap();
    eq.settings.set_band(0, 8000.0, 24.0, 0.1).unwrap();
    let out = eq.process(&data).unwrap();
    assert_eq!(samples(&out), [0.5, 0.25, -0.5, -0.25]);
    assert_eq!(out.descriptor().format, AudioSampleFormat::F32);
    assert_eq!(out.descriptor().timestamp, 9);
}

#[test]
fn bypass_changes_are_smoothed_and_reach_exact_identity() {
    let mut eq = AudioEqualizer::new(1).unwrap();
    eq.settings.set_preamp(-60.0).unwrap();
    eq.process(&audio(&[1.0; 480], 1, 0)).unwrap();
    eq.settings.set_bypass(true);
    let out = samples(&eq.process(&audio(&[1.0; 960], 1, 10000)).unwrap());
    assert!(out[0] < 0.01);
    assert!(out.windows(2).all(|p| p[1] >= p[0] && p[1] - p[0] < 0.003));
    assert!(out[480..].iter().all(|x| *x == 1.0));
}
