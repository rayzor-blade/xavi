// ABI integration tests are kept beside the generated model so they can also
// exercise malformed record carriers and stale handles.
use super::*;

fn live() -> usize {
    with_media(|m| m.live_resources().unwrap())
}
fn audio(bytes: &[u8]) -> Box<AudioData> {
    AudioData::create(&AudioDataInit::new(
        AudioSampleFormat::U8.into(),
        48_000.0,
        bytes.len() as i64,
        1,
        -9,
        Buffer::new(bytes),
    ))
}
fn rgba() -> Box<VideoFrame> {
    VideoFrame::create(
        Buffer::new(&[10, 20, 30, 255, 40, 50, 60, 255]),
        &VideoFrameBufferInit::new(VideoPixelFormat::RGBA.into(), 2, 1, -42),
    )
}
fn no_errors() {
    assert!(host::raised().is_empty());
}

#[test]
fn generated_audio_copies_bytes_and_clones_close_independently() {
    let source = Buffer::new(&[128, 192, 255]);
    let init = AudioDataInit::new(AudioSampleFormat::U8.into(), 48_000.0, 3, 1, -9, source);
    let original = AudioData::create(&init);
    // Mutate the constructor's source after it has returned.
    unsafe {
        *source.writable().as_mut_ptr() = 0;
    }
    let clone = AudioData::clone(&original);
    AudioData::close(&original);
    AudioData::close(&original);
    assert_eq!(AudioData::timestamp(&clone), -9);
    assert_eq!(AudioData::numberOfFrames(&clone), 3);
    assert_eq!(AudioData::duration(&clone), 62);
    let mut options = AudioDataCopyToOptions::new(0);
    AudioDataCopyToOptions::format(&mut options, AudioSampleFormat::F32Planar.into());
    assert_eq!(AudioData::allocationSize(&clone, &options), 12);
    let destination = Buffer::new(&[0; 12]);
    AudioData::copyTo(&clone, destination.writable(), &options);
    let samples: Vec<_> = unsafe { destination.as_slice() }
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| f32::from_ne_bytes(*bytes))
        .collect();
    assert_eq!(samples, [0.0, 0.5, 127.0 / 128.0]);
    AudioData::close(&clone);
    no_errors();
    assert_eq!(live(), 0);
    assert_eq!(AudioData::allocationSize(&original, &options), 0);
    assert_eq!(host::raised().len(), 1);
}

#[test]
fn malformed_record_values_raise_without_allocating_or_writing() {
    let mut init = AudioDataInit::new(
        AudioSampleFormat::U8.into(),
        48_000.0,
        1,
        1,
        0,
        Buffer::new(&[128]),
    );
    for frames in [-1, i64::from(u32::MAX) + 1] {
        init.numberOfFrames = frames;
        assert_eq!(AudioData::create(&init).handle, 0);
    }
    init.numberOfFrames = 1;
    init.format = 999;
    assert_eq!(AudioData::create(&init).handle, 0);
    init.format = AudioSampleFormat::U8.native();
    init.sampleRate = 1e-13;
    assert_eq!(AudioData::create(&init).handle, 0);
    assert_eq!(host::raised().len(), 4);
    assert_eq!(live(), 0);
    let sample = audio(&[128]);
    let mut options = AudioDataCopyToOptions::new(0);
    AudioDataCopyToOptions::frameOffset(&mut options, -1);
    let destination = Buffer::new(&[91]);
    AudioData::copyTo(&sample, destination.writable(), &options);
    assert_eq!(unsafe { destination.as_slice() }, [91]);
    assert_eq!(host::raised().len(), 1);
    AudioData::close(&sample);
    no_errors();
    assert_eq!(live(), 0);
}

#[test]
fn video_future_contains_actual_layouts_after_destination_writes() {
    let frame = rgba();
    let mut options = VideoFrameCopyToOptions::new();
    VideoFrameCopyToOptions::addLayout(&mut options, &PlaneLayout::new(2, 12));
    assert_eq!(VideoFrame::allocationSize(&frame, &options), 10);
    let destination = Buffer::new(&[99; 14]);
    let future = VideoFrame::copyTo(&frame, destination.writable(), &options);
    let layouts = future
        .take()
        .expect("CPU copy must settle before returning")
        .unwrap();
    assert_eq!(
        unsafe { destination.as_slice() },
        [99, 99, 10, 20, 30, 255, 40, 50, 60, 255, 99, 99, 99, 99]
    );
    assert_eq!(PlaneLayouts::count(&layouts), 1);
    assert_eq!(PlaneLayouts::offset(&layouts, 0), 2);
    assert_eq!(PlaneLayouts::stride(&layouts, 0), 12);
    assert_eq!(PlaneLayouts::offset(&layouts, -1), 0);
    assert_eq!(PlaneLayouts::stride(&layouts, 1), 0);
    assert_eq!(host::raised().len(), 2);
    VideoFrame::close(&frame);
    assert_eq!(PlaneLayouts::count(&layouts), 1);
    PlaneLayouts::close(&layouts);
    PlaneLayouts::close(&layouts);
    no_errors();
    assert_eq!(live(), 0);
}

#[test]
fn invalid_video_copies_reject_without_partial_writes_or_layout_leaks() {
    let frame = rgba();
    let mut options = VideoFrameCopyToOptions::new();
    let destination = Buffer::new(&[91; 4]);
    let error = VideoFrame::copyTo(&frame, destination.writable(), &options)
        .take()
        .unwrap()
        .err()
        .unwrap();
    assert!(error.contains("too small"));
    assert_eq!(unsafe { destination.as_slice() }, [91; 4]);
    assert_eq!(live(), 1);
    VideoFrameCopyToOptions::format(&mut options, VideoPixelFormat::NV12.into());
    assert!(
        VideoFrame::copyTo(&frame, destination.writable(), &options)
            .take()
            .unwrap()
            .is_err()
    );
    VideoFrame::close(&frame);
    assert!(
        VideoFrame::copyTo(&frame, destination.writable(), &options)
            .take()
            .unwrap()
            .is_err()
    );
    no_errors(); // Async failures reject; they do not raise synchronous errors.
    assert_eq!(live(), 0);
}

#[test]
fn optional_duration_and_color_metadata_survive_generated_variant_getters() {
    let bytes = Buffer::new(&[0; 4]);
    let mut init = VideoFrameBufferInit::new(VideoPixelFormat::RGBA.into(), 1, 1, i64::MIN);
    let unknown = VideoFrame::create(bytes, &init);
    assert_eq!(VideoFrame::durationVariant(&unknown), 0);
    assert_eq!(VideoFrame::colorSpaceVariant(&unknown), 0);
    assert_eq!(VideoFrame::colorSpaceValuePrimariesVariant(), 0);
    assert_eq!(VideoFrame::colorSpaceValueFullRangeVariant(), 0);
    VideoFrameBufferInit::duration(&mut init, 0);
    let mut color = VideoColorSpaceInit::new();
    VideoColorSpaceInit::primaries(&mut color, VideoColorPrimaries::Bt2020.into());
    VideoColorSpaceInit::transfer(&mut color, VideoTransferCharacteristics::Hlg.into());
    VideoColorSpaceInit::fullRange(&mut color, false);
    VideoFrameBufferInit::colorSpace(&mut init, &color);
    let known = VideoFrame::create(bytes, &init);
    assert_eq!(VideoFrame::timestamp(&known), i64::MIN);
    assert_eq!(VideoFrame::durationVariant(&known), 1);
    assert_eq!(VideoFrame::optionalDurationValueMicroseconds(), 0);
    VideoFrame::colorSpaceVariant(&known);
    assert_eq!(
        VideoFrame::colorSpaceValuePrimariesValueValue().get(),
        VideoColorPrimaries::Bt2020
    );
    assert_eq!(
        VideoFrame::colorSpaceValueTransferValueValue().get(),
        VideoTransferCharacteristics::Hlg
    );
    assert_eq!(VideoFrame::colorSpaceValueMatrixVariant(), 0);
    assert_eq!(VideoFrame::colorSpaceValueFullRangeVariant(), 1);
    assert!(!VideoFrame::colorSpaceValueFullRangeValueValue());
    VideoFrame::visibleRectVariant(&known);
    assert_eq!(VideoFrame::pixelRectValueWidth(), 1);
    VideoFrame::close(&known);
    VideoFrame::close(&unknown);
    no_errors();
    assert_eq!(live(), 0);
}

#[test]
fn encoded_chunks_snapshot_data_and_preserve_unknown_and_zero_duration() {
    let bytes = Buffer::new(&[1, 2, 3]);
    let audio = EncodedAudioChunk::create(&EncodedAudioChunkInit::new(
        EncodedChunkType::Key.into(),
        -8,
        bytes,
    ));
    let mut init = EncodedVideoChunkInit::new(EncodedChunkType::Delta.into(), 10, bytes);
    EncodedVideoChunkInit::duration(&mut init, 0);
    let video = EncodedVideoChunk::create(&init);
    unsafe {
        *bytes.writable().as_mut_ptr() = 9;
    }
    assert_eq!(EncodedAudioChunk::durationVariant(&audio), 0);
    assert_eq!(EncodedVideoChunk::durationVariant(&video), 1);
    assert_eq!(EncodedVideoChunk::optionalDurationValueMicroseconds(), 0);
    assert_eq!(EncodedAudioChunk::timestamp(&audio), -8);
    assert_eq!(
        EncodedVideoChunk::kind(&video).get(),
        EncodedChunkType::Delta
    );
    let destination = Buffer::new(&[0; 4]);
    EncodedVideoChunk::copyTo(&video, destination.writable());
    assert_eq!(unsafe { destination.as_slice() }, [1, 2, 3, 0]);
    EncodedAudioChunk::close(&audio);
    EncodedVideoChunk::close(&video);
    EncodedVideoChunkInit::duration(&mut init, -1);
    assert_eq!(EncodedVideoChunk::create(&init).handle, 0);
    assert_eq!(host::raised().len(), 1);
    let empty = EncodedAudioChunk::create(&EncodedAudioChunkInit::new(
        EncodedChunkType::Key.into(),
        0,
        Buffer::NULL,
    ));
    EncodedAudioChunk::copyTo(&empty, Buffer::NULL.writable());
    EncodedAudioChunk::close(&empty);
    no_errors();
    assert_eq!(live(), 0);
}

#[test]
fn streaming_republishes_a_retained_frame_after_guest_close() {
    use xavi_core::stream::{Limits, Read, channel};
    let source = audio(&[128, 192]);
    let held = with_media(|m| m.audio(source.handle).unwrap());
    let (mut tx, mut rx) = channel(Limits {
        max_items: 1,
        max_bytes: 2,
    })
    .unwrap();
    tx.try_send(held).unwrap();
    tx.finish().unwrap();
    AudioData::close(&source);
    let Read::Item(held) = rx.try_next().unwrap() else {
        panic!("missing queued audio")
    };
    let retained = held.clone();
    let received = AudioData::from_handle(with_media(|m| m.retain_audio(held).unwrap()));
    assert!(std::sync::Arc::ptr_eq(
        &retained,
        &with_media(|m| m.audio(received.handle).unwrap())
    ));
    assert_eq!(AudioData::numberOfFrames(&received), 2);
    AudioData::close(&received);
    assert!(matches!(rx.try_next().unwrap(), Read::End));
    no_errors();
    assert_eq!(live(), 0);
}

#[test]
fn wrong_type_close_and_stale_close_cannot_release_a_live_resource() {
    let frame = rgba();
    AudioData::close(&AudioData::from_handle(frame.handle));
    assert_eq!(host::raised().len(), 1);
    assert_eq!(VideoFrame::codedWidth(&frame), 2);
    VideoFrame::close(&frame);
    let next = rgba();
    VideoFrame::close(&frame);
    assert_eq!(VideoFrame::codedWidth(&next), 2);
    VideoFrame::close(&next);
    no_errors();
    assert_eq!(live(), 0);
}

#[test]
fn rayzor_symbols_have_matching_method_descriptors() {
    let symbols = xidl_runtime_symbols();
    assert_eq!(symbols.len(), XIDL_METHODS.len());
    for name in [
        "xavi_audio_data_create",
        "xavi_audio_data_copy_to",
        "xavi_video_frame_copy_to",
        "xavi_encoded_video_chunk_close",
    ] {
        assert!(symbols.iter().any(|(symbol, _)| *symbol == name));
    }
    assert!(symbols.iter().all(|(name, _)| name.starts_with("xavi_")));
}
