#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RectNorm {
    pub(crate) x0: f64,
    pub(crate) y0: f64,
    pub(crate) x1: f64,
    pub(crate) y1: f64,
}

impl RectNorm {
    pub(crate) fn contains_px(self, size: SurfaceSize, x: f64, y: f64) -> bool {
        let width = f64::from(size.width.max(1));
        let height = f64::from(size.height.max(1));
        x >= width * self.x0
            && x <= width * self.x1
            && y >= height * self.y0
            && y <= height * self.y1
    }

    pub(crate) fn to_px(self, size: SurfaceSize) -> RectPx {
        let width = f64::from(size.width.max(1));
        let height = f64::from(size.height.max(1));
        let x0 = (width * self.x0).floor().max(0.0) as i32;
        let y0 = (height * self.y0).floor().max(0.0) as i32;
        let x1 = (width * self.x1).ceil().max(0.0) as i32;
        let y1 = (height * self.y1).ceil().max(0.0) as i32;

        RectPx {
            x: x0,
            y: y0,
            w: (x1 - x0).max(0),
            h: (y1 - y0).max(0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RectPx {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) w: i32,
    pub(crate) h: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SurfaceSize {
    pub(crate) width: u32,
    pub(crate) height: u32,
}
