use std::sync::Arc;

use crate::{Error, FrameLayout, PlaneLayout, Rect, Result, VideoPixelFormat, snapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoColorPrimaries {
    Bt709,
    Bt470bg,
    Smpte170m,
    Bt2020,
    Smpte432,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoTransferCharacteristics {
    Bt709,
    Smpte170m,
    Iec61966_2_1,
    Linear,
    Pq,
    Hlg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoMatrixCoefficients {
    Rgb,
    Bt709,
    Bt470bg,
    Smpte170m,
    Bt2020Ncl,
}

/// Unknown metadata stays unknown; pixel format alone does not identify gamut.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VideoColorSpace {
    pub primaries: Option<VideoColorPrimaries>,
    pub transfer: Option<VideoTransferCharacteristics>,
    pub matrix: Option<VideoMatrixCoefficients>,
    pub full_range: Option<bool>,
}

#[derive(Clone, Copy, Debug)]
pub struct VideoDescriptor {
    pub format: VideoPixelFormat,
    pub coded_width: u32,
    pub coded_height: u32,
    pub timestamp: i64,
    pub duration: Option<u64>,
    pub visible_rect: Option<Rect>,
    pub display_width: Option<u32>,
    pub display_height: Option<u32>,
    pub color_space: VideoColorSpace,
}

impl VideoDescriptor {
    pub fn new(
        format: VideoPixelFormat,
        coded_width: u32,
        coded_height: u32,
        timestamp: i64,
    ) -> Self {
        Self {
            format,
            coded_width,
            coded_height,
            timestamp,
            duration: None,
            visible_rect: None,
            display_width: None,
            display_height: None,
            color_space: VideoColorSpace::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoInfo {
    pub format: VideoPixelFormat,
    pub coded_width: u32,
    pub coded_height: u32,
    pub timestamp: i64,
    pub duration: Option<u64>,
    pub visible_rect: Rect,
    pub display_width: u32,
    pub display_height: u32,
    pub color_space: VideoColorSpace,
}

#[derive(Clone, Debug, Default)]
pub struct VideoCopyOptions {
    pub rect: Option<Rect>,
    pub layout: Option<Vec<PlaneLayout>>,
    pub format: Option<VideoPixelFormat>,
}

/// A CPU frame. GPU-backed frames will use a separate storage implementation.
#[derive(Clone, Debug)]
pub struct VideoFrame {
    info: VideoInfo,
    layout: FrameLayout,
    data: Arc<[u8]>,
}

impl VideoFrame {
    pub fn new(
        descriptor: VideoDescriptor,
        data: &[u8],
        layout: Option<&[PlaneLayout]>,
    ) -> Result<Self> {
        let d = descriptor;
        let layout = FrameLayout::new(d.format, d.coded_width, d.coded_height, layout)?;
        let visible_rect = d.visible_rect.unwrap_or(Rect {
            x: 0,
            y: 0,
            width: d.coded_width,
            height: d.coded_height,
        });
        visible_rect.validate(d.coded_width, d.coded_height, d.format)?;
        let (display_width, display_height) = match (d.display_width, d.display_height) {
            (None, None) => (visible_rect.width, visible_rect.height),
            (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
            _ => {
                return Err(Error::invalid(
                    "display dimensions must both be positive or both omitted",
                ));
            }
        };
        if data.len() < layout.byte_len() as usize {
            return Err(Error::invalid(
                "video input buffer is too small for its planes",
            ));
        }
        let data = snapshot(&data[..layout.byte_len() as usize])?;
        let info = VideoInfo {
            format: d.format,
            coded_width: d.coded_width,
            coded_height: d.coded_height,
            timestamp: d.timestamp,
            duration: d.duration,
            visible_rect,
            display_width,
            display_height,
            color_space: d.color_space,
        };
        Ok(Self { info, layout, data })
    }

    pub fn info(&self) -> VideoInfo {
        self.info
    }
    pub fn layout(&self) -> &FrameLayout {
        &self.layout
    }
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    pub fn allocation_size(&self, options: &VideoCopyOptions) -> Result<u32> {
        Ok(self.plan(options)?.1.byte_len())
    }

    /// Copies sample rows, leaving destination padding and unrelated bytes alone.
    /// The source format is preserved; conversion requests return NotSupported.
    /// Native adapters can resolve VideoFrame.copyTo's future after this returns.
    pub fn copy_to(
        &self,
        destination: &mut [u8],
        options: &VideoCopyOptions,
    ) -> Result<Vec<PlaneLayout>> {
        let (rect, output) = self.plan(options)?;
        if destination.len() < output.byte_len() as usize {
            return Err(Error::invalid("video destination buffer is too small"));
        }
        for (index, &(sx, sy, sample_bytes)) in self.info.format.planes().iter().enumerate() {
            let source = self.layout.planes()[index];
            let target = output.planes()[index];
            let x = (rect.x / sx) as usize * sample_bytes as usize;
            let y = (rect.y / sy) as usize;
            for row in 0..target.rows as usize {
                let src =
                    source.layout.offset as usize + (y + row) * source.layout.stride as usize + x;
                let dst = target.layout.offset as usize + row * target.layout.stride as usize;
                let count = target.row_bytes as usize;
                destination[dst..dst + count].copy_from_slice(&self.data[src..src + count]);
            }
        }
        Ok(output.layouts())
    }

    fn plan(&self, options: &VideoCopyOptions) -> Result<(Rect, FrameLayout)> {
        if options
            .format
            .is_some_and(|format| format != self.info.format)
        {
            return Err(Error::unsupported(
                "CPU video copies currently preserve pixel format",
            ));
        }
        let rect = options.rect.unwrap_or(self.info.visible_rect);
        rect.validate(
            self.info.coded_width,
            self.info.coded_height,
            self.info.format,
        )?;
        let layout = FrameLayout::new(
            self.info.format,
            rect.width,
            rect.height,
            options.layout.as_deref(),
        )?;
        Ok((rect, layout))
    }
}
