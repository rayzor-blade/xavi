use xavi_core::*;

fn descriptor(format: AudioSampleFormat, frames: u32, channels: u32) -> AudioDescriptor {
    AudioDescriptor {
        format,
        sample_rate: 48_000.0,
        number_of_frames: frames,
        number_of_channels: channels,
        timestamp: -123,
    }
}

#[test]
fn input_is_snapshotted_and_clones_share_storage() {
    let mut bytes = vec![3; 480];
    let data = AudioData::new(descriptor(AudioSampleFormat::U8, 480, 1), &bytes).unwrap();
    bytes.fill(9);
    let copy = data.clone();
    assert_eq!(data.bytes(), &[3; 480]);
    assert_eq!(data.bytes().as_ptr(), copy.bytes().as_ptr());
    assert_eq!(copy.duration(), 10_000);
    assert_eq!(copy.descriptor().timestamp, -123);
}

#[test]
fn all_sample_types_rearrange_channels_without_changing_bits() {
    use AudioSampleFormat::*;
    for (interleaved, planar) in [
        (U8, U8Planar),
        (S16, S16Planar),
        (S32, S32Planar),
        (F32, F32Planar),
    ] {
        let size = interleaved.bytes_per_sample() as usize;
        let bytes: Vec<_> = (0..4 * 3 * size).map(|n| n as u8).collect();
        let data = AudioData::new(descriptor(interleaved, 4, 3), &bytes).unwrap();
        let mut planes = Vec::new();
        for channel in 0..3 {
            let options = AudioCopyOptions {
                plane_index: channel,
                format: Some(planar),
                ..Default::default()
            };
            let mut out = vec![0; data.allocation_size(options).unwrap() as usize];
            data.copy_to(&mut out, options).unwrap();
            for frame in 0..4 {
                assert_eq!(
                    &out[frame * size..(frame + 1) * size],
                    &bytes[(frame * 3 + channel as usize) * size
                        ..(frame * 3 + channel as usize + 1) * size]
                );
            }
            planes.extend(out);
        }
        let data = AudioData::new(descriptor(planar, 4, 3), &planes).unwrap();
        let mut out = vec![0; bytes.len()];
        data.copy_to(
            &mut out,
            AudioCopyOptions {
                format: Some(interleaved),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(out, bytes);
    }
}

#[test]
fn subranges_use_the_output_plane_and_preserve_destination_tail() {
    let data = AudioData::new(
        descriptor(AudioSampleFormat::U8, 4, 2),
        &[1, 11, 2, 12, 3, 13, 4, 14],
    )
    .unwrap();
    let options = AudioCopyOptions {
        plane_index: 1,
        frame_offset: 1,
        frame_count: Some(2),
        format: Some(AudioSampleFormat::U8Planar),
    };
    assert_eq!(data.allocation_size(options).unwrap(), 2);
    let mut out = [99; 4];
    data.copy_to(&mut out, options).unwrap();
    assert_eq!(out, [12, 13, 99, 99]);
}

#[test]
fn pcm_conversion_normalizes_and_saturates() {
    let bytes: Vec<_> = [-32768_i16, 0, 32767]
        .into_iter()
        .flat_map(i16::to_ne_bytes)
        .collect();
    let data = AudioData::new(descriptor(AudioSampleFormat::S16, 3, 1), &bytes).unwrap();
    let mut out = [0; 12];
    data.copy_to(
        &mut out,
        AudioCopyOptions {
            format: Some(AudioSampleFormat::F32Planar),
            ..Default::default()
        },
    )
    .unwrap();
    let floats: Vec<_> = out
        .as_chunks::<4>()
        .0
        .iter()
        .map(|s| f32::from_ne_bytes(*s))
        .collect();
    assert_eq!(floats, [-1.0, 0.0, 32767.0 / 32768.0]);

    let bytes: Vec<_> = [f32::NEG_INFINITY, -1.0, 0.0, 1.0, f32::INFINITY, f32::NAN]
        .into_iter()
        .flat_map(f32::to_ne_bytes)
        .collect();
    let data = AudioData::new(descriptor(AudioSampleFormat::F32, 6, 1), &bytes).unwrap();
    let mut out = [0; 6];
    data.copy_to(
        &mut out,
        AudioCopyOptions {
            format: Some(AudioSampleFormat::U8),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(out, [0, 0, 128, 255, 255, 128]);
}

#[test]
fn invalid_copies_never_partially_write_the_destination() {
    let data = AudioData::new(descriptor(AudioSampleFormat::U8, 2, 1), &[1, 2]).unwrap();
    for options in [
        AudioCopyOptions {
            plane_index: 1,
            ..Default::default()
        },
        AudioCopyOptions {
            frame_offset: 2,
            ..Default::default()
        },
        AudioCopyOptions {
            frame_count: Some(3),
            ..Default::default()
        },
    ] {
        let mut out = [42; 2];
        assert!(data.copy_to(&mut out, options).is_err());
        assert!(data.allocation_size(options).is_err());
        assert_eq!(out, [42; 2]);
    }
    let mut out = [42; 1];
    assert!(data.copy_to(&mut out, AudioCopyOptions::default()).is_err());
    assert_eq!(out, [42]);
}

#[test]
fn invalid_descriptors_and_size_overflow_fail_before_allocation() {
    for rate in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::MIN_POSITIVE] {
        let mut d = descriptor(AudioSampleFormat::U8, 1, 1);
        d.sample_rate = rate;
        assert!(AudioData::new(d, &[0]).is_err());
    }
    assert!(AudioData::new(descriptor(AudioSampleFormat::F32, u32::MAX, u32::MAX), &[]).is_err());
    assert!(AudioData::new(descriptor(AudioSampleFormat::S16, 2, 2), &[0; 7]).is_err());
    assert!(AudioData::new(descriptor(AudioSampleFormat::U8, 0, 1), &[]).is_err());
    assert!(AudioData::new(descriptor(AudioSampleFormat::U8, 1, 0), &[]).is_err());
}
