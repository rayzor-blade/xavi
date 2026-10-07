use xavi_core::*;

#[test]
fn odd_dimensions_round_up_chroma_planes_in_every_format() {
    use VideoPixelFormat::*;
    for (format, bytes) in [
        (I420, 17),
        (I420A, 26),
        (I422, 21),
        (I444, 27),
        (Nv12, 17),
        (Rgba, 36),
        (Rgbx, 36),
        (Bgra, 36),
        (Bgrx, 36),
    ] {
        let layout = FrameLayout::new(format, 3, 3, None).unwrap();
        assert_eq!(layout.byte_len(), bytes);
        let source: Vec<_> = (0..bytes).map(|n| n as u8).collect();
        let frame = VideoFrame::new(VideoDescriptor::new(format, 3, 3, -5), &source, None).unwrap();
        let mut out = vec![0; bytes as usize];
        assert_eq!(
            frame.allocation_size(&VideoCopyOptions::default()).unwrap(),
            bytes
        );
        frame
            .copy_to(&mut out, &VideoCopyOptions::default())
            .unwrap();
        assert_eq!(out, source);
    }
}

#[test]
fn cropped_nv12_copy_honors_source_and_destination_padding() {
    let layouts = [
        PlaneLayout {
            offset: 2,
            stride: 6,
        },
        PlaneLayout {
            offset: 26,
            stride: 6,
        },
    ];
    let mut bytes = [0xe0; 36];
    for row in 0..4 {
        for col in 0..4 {
            bytes[2 + row * 6 + col] = (row * 4 + col) as u8;
        }
    }
    bytes[26..30].copy_from_slice(&[100, 101, 102, 103]);
    bytes[32..36].copy_from_slice(&[104, 105, 106, 107]);
    let mut d = VideoDescriptor::new(VideoPixelFormat::Nv12, 4, 4, -42);
    d.visible_rect = Some(Rect {
        x: 2,
        y: 2,
        width: 2,
        height: 2,
    });
    d.duration = Some(0);
    d.color_space.full_range = Some(false);
    let frame = VideoFrame::new(d, &bytes, Some(&layouts)).unwrap();
    bytes.fill(0);
    let cloned = frame.clone();
    assert_eq!(frame.bytes().as_ptr(), cloned.bytes().as_ptr());
    assert_eq!(frame.info().display_width, 2);
    assert_eq!(frame.info().duration, Some(0));
    assert_eq!(frame.info().color_space.full_range, Some(false));
    let mut packed = [0; 6];
    frame
        .copy_to(&mut packed, &VideoCopyOptions::default())
        .unwrap();
    assert_eq!(packed, [10, 11, 14, 15, 106, 107]);

    let options = VideoCopyOptions {
        layout: Some(vec![
            PlaneLayout {
                offset: 1,
                stride: 4,
            },
            PlaneLayout {
                offset: 10,
                stride: 2,
            },
        ]),
        ..Default::default()
    };
    assert_eq!(frame.allocation_size(&options).unwrap(), 12);
    let mut padded = [0xaa; 14];
    let copied = frame.copy_to(&mut padded, &options).unwrap();
    assert_eq!(copied, options.layout.unwrap());
    assert_eq!(
        padded,
        [
            0xaa, 10, 11, 0xaa, 0xaa, 14, 15, 0xaa, 0xaa, 0xaa, 106, 107, 0xaa, 0xaa
        ]
    );
}

#[test]
fn invalid_layouts_and_buffer_sizes_are_rejected_without_writes() {
    let frame = VideoFrame::new(
        VideoDescriptor::new(VideoPixelFormat::I420, 4, 4, 0),
        &[0; 24],
        None,
    )
    .unwrap();
    let invalid = [
        VideoCopyOptions {
            rect: Some(Rect {
                x: 1,
                y: 0,
                width: 2,
                height: 2,
            }),
            ..Default::default()
        },
        VideoCopyOptions {
            rect: Some(Rect {
                x: u32::MAX,
                y: 0,
                width: 2,
                height: 2,
            }),
            ..Default::default()
        },
        VideoCopyOptions {
            layout: Some(vec![]),
            ..Default::default()
        },
        VideoCopyOptions {
            layout: Some(vec![
                PlaneLayout {
                    offset: 0,
                    stride: 4
                };
                3
            ]),
            ..Default::default()
        },
        VideoCopyOptions {
            format: Some(VideoPixelFormat::Rgba),
            ..Default::default()
        },
    ];
    for options in invalid {
        let mut out = [37; 24];
        assert!(frame.allocation_size(&options).is_err());
        assert!(frame.copy_to(&mut out, &options).is_err());
        assert_eq!(out, [37; 24]);
    }
    let mut short = [37; 23];
    assert!(
        frame
            .copy_to(&mut short, &VideoCopyOptions::default())
            .is_err()
    );
    assert_eq!(short, [37; 23]);
    assert!(
        VideoFrame::new(
            VideoDescriptor::new(VideoPixelFormat::I420, 4, 4, 0),
            &short,
            None
        )
        .is_err()
    );
}

#[test]
fn oversized_dimensions_and_plane_spans_cannot_overflow() {
    assert!(FrameLayout::new(VideoPixelFormat::Rgba, u32::MAX, 1, None).is_err());
    assert!(FrameLayout::new(VideoPixelFormat::I420, u32::MAX, u32::MAX, None).is_err());
    assert!(
        FrameLayout::new(
            VideoPixelFormat::Rgba,
            1,
            1,
            Some(&[PlaneLayout {
                offset: u32::MAX,
                stride: 4
            }])
        )
        .is_err()
    );
    assert!(
        FrameLayout::new(
            VideoPixelFormat::Rgba,
            1,
            2,
            Some(&[PlaneLayout {
                offset: 0,
                stride: 3
            }])
        )
        .is_err()
    );
    assert!(FrameLayout::new(VideoPixelFormat::Rgba, 0, 1, None).is_err());
}

#[test]
fn rectangle_and_display_metadata_are_validated() {
    for x in [
        -1.0,
        0.5,
        f64::NAN,
        f64::INFINITY,
        f64::from(u32::MAX) + 1.0,
    ] {
        assert!(Rect::from_f64(x, 0.0, 1.0, 1.0).is_err());
    }
    assert!(Rect::from_f64(0.0, 0.0, 0.0, 1.0).is_err());
    let mut d = VideoDescriptor::new(VideoPixelFormat::Rgba, 1, 1, 0);
    d.display_width = Some(2);
    assert!(VideoFrame::new(d, &[0; 4], None).is_err());
    d.display_height = Some(3);
    let frame = VideoFrame::new(d, &[0; 4], None).unwrap();
    assert_eq!(
        (frame.info().display_width, frame.info().display_height),
        (2, 3)
    );
}
