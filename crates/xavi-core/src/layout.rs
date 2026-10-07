use crate::{Error, Result, VideoPixelFormat, byte_len};

/// Integral pixel coordinates after validation of an IDL DOMRectInit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn from_f64(x: f64, y: f64, width: f64, height: f64) -> Result<Self> {
        fn component(v: f64) -> Result<u32> {
            if !v.is_finite() || v < 0.0 || v > f64::from(u32::MAX) || v.fract() != 0.0 {
                return Err(Error::invalid(
                    "rectangle coordinates must be nonnegative integral u32 values",
                ));
            }
            Ok(v as u32)
        }
        let rect = Self {
            x: component(x)?,
            y: component(y)?,
            width: component(width)?,
            height: component(height)?,
        };
        if rect.width == 0 || rect.height == 0 {
            return Err(Error::invalid("rectangle must be nonempty"));
        }
        Ok(rect)
    }

    pub(crate) fn validate(self, width: u32, height: u32, format: VideoPixelFormat) -> Result<()> {
        if self.width == 0
            || self.height == 0
            || u64::from(self.x) + u64::from(self.width) > u64::from(width)
            || u64::from(self.y) + u64::from(self.height) > u64::from(height)
        {
            return Err(Error::invalid(
                "rectangle is empty or outside the coded dimensions",
            ));
        }
        for &(sx, sy, _) in format.planes() {
            if !self.x.is_multiple_of(sx) || !self.y.is_multiple_of(sy) {
                return Err(Error::invalid(
                    "rectangle origin is not aligned to chroma samples",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaneLayout {
    pub offset: u32,
    pub stride: u32,
}

/// Validated geometry of a plane. Row padding is not part of the sample data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plane {
    pub layout: PlaneLayout,
    pub row_bytes: u32,
    pub rows: u32,
}

impl Plane {
    fn end(self) -> u64 {
        u64::from(self.layout.offset)
            + u64::from(self.layout.stride) * u64::from(self.rows - 1)
            + u64::from(self.row_bytes)
    }
}

/// A checked layout for all planes. Odd chroma dimensions are rounded up.
#[derive(Clone, Debug)]
pub struct FrameLayout {
    planes: Vec<Plane>,
    byte_len: u32,
}

impl FrameLayout {
    pub fn new(
        format: VideoPixelFormat,
        width: u32,
        height: u32,
        layout: Option<&[PlaneLayout]>,
    ) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(Error::invalid("coded dimensions must be positive"));
        }
        if layout.is_some_and(|p| p.len() != format.planes().len()) {
            return Err(Error::invalid(
                "layout must contain exactly one entry per plane",
            ));
        }
        let mut planes = Vec::with_capacity(format.planes().len());
        let mut end = 0;
        for (index, &(sx, sy, bytes)) in format.planes().iter().enumerate() {
            let row_bytes = byte_len(u64::from(width.div_ceil(sx)) * u64::from(bytes))?;
            let rows = height.div_ceil(sy);
            let layout = layout.map_or(
                PlaneLayout {
                    offset: end,
                    stride: row_bytes,
                },
                |p| p[index],
            );
            if layout.stride < row_bytes {
                return Err(Error::invalid("plane stride is smaller than a sample row"));
            }
            let plane = Plane {
                layout,
                row_bytes,
                rows,
            };
            let plane_end = byte_len(plane.end())?;
            if planes.iter().any(|other: &Plane| {
                u64::from(plane.layout.offset) < other.end()
                    && u64::from(other.layout.offset) < plane.end()
            }) {
                return Err(Error::invalid("plane memory ranges overlap"));
            }
            end = end.max(plane_end);
            planes.push(plane);
        }
        Ok(Self {
            planes,
            byte_len: end,
        })
    }

    pub fn planes(&self) -> &[Plane] {
        &self.planes
    }

    /// Smallest buffer length containing the last sample, without tail padding.
    pub fn byte_len(&self) -> u32 {
        self.byte_len
    }

    pub fn layouts(&self) -> Vec<PlaneLayout> {
        self.planes.iter().map(|p| p.layout).collect()
    }
}
