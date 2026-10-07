/// PCM samples use native byte order, matching typed buffers on the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioSampleFormat {
    U8,
    S16,
    S32,
    F32,
    U8Planar,
    S16Planar,
    S32Planar,
    F32Planar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SampleType {
    U8,
    S16,
    S32,
    F32,
}

impl AudioSampleFormat {
    pub fn bytes_per_sample(self) -> u32 {
        match self.sample_type() {
            SampleType::U8 => 1,
            SampleType::S16 => 2,
            SampleType::S32 | SampleType::F32 => 4,
        }
    }

    pub fn is_planar(self) -> bool {
        matches!(
            self,
            Self::U8Planar | Self::S16Planar | Self::S32Planar | Self::F32Planar
        )
    }

    pub(crate) fn sample_type(self) -> SampleType {
        match self {
            Self::U8 | Self::U8Planar => SampleType::U8,
            Self::S16 | Self::S16Planar => SampleType::S16,
            Self::S32 | Self::S32Planar => SampleType::S32,
            Self::F32 | Self::F32Planar => SampleType::F32,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoPixelFormat {
    I420,
    I420A,
    I422,
    I444,
    Nv12,
    Rgba,
    Rgbx,
    Bgra,
    Bgrx,
}

impl VideoPixelFormat {
    /// Each plane's horizontal/vertical subsampling and bytes per sample.
    pub(crate) fn planes(self) -> &'static [(u32, u32, u32)] {
        match self {
            Self::I420 => &[(1, 1, 1), (2, 2, 1), (2, 2, 1)],
            Self::I420A => &[(1, 1, 1), (2, 2, 1), (2, 2, 1), (1, 1, 1)],
            Self::I422 => &[(1, 1, 1), (2, 1, 1), (2, 1, 1)],
            Self::I444 => &[(1, 1, 1), (1, 1, 1), (1, 1, 1)],
            Self::Nv12 => &[(1, 1, 1), (2, 2, 2)],
            Self::Rgba | Self::Rgbx | Self::Bgra | Self::Bgrx => &[(1, 1, 4)],
        }
    }
}
