use touchdeck::niri;

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
            pub(crate) x: x0,
            pub(crate) y: y0,
            pub(crate) w: (x1 - x0).max(0),
            pub(crate) h: (y1 - y0).max(0),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct RectPx {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) w: i32,
    pub(crate) h: i32,
}

#[derive(Clone, Copy)]
pub(crate) struct SurfaceSize {
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Clone, Debug)]

pub(crate) fn transformed_source_size(output: niri::FocusedOutputLayout) -> (f64, f64) {
    match output.transform {
        niri::OutputTransform::_90
        | niri::OutputTransform::_270
        | niri::OutputTransform::Flipped90
        | niri::OutputTransform::Flipped270 => (f64::from(output.height), f64::from(output.width)),
        niri::OutputTransform::Normal
        | niri::OutputTransform::_180
        | niri::OutputTransform::Flipped
        | niri::OutputTransform::Flipped180 => (f64::from(output.width), f64::from(output.height)),
    }
}

pub(crate) fn transform_rect_to_overlay(
    transform: niri::OutputTransform,
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) w: f64,
    pub(crate) h: f64,
    source_w: f64,
    source_h: f64,
) -> (f64, f64, f64, f64) {
    let points = [
        transform_point_to_overlay(transform, x, y, source_w, source_h),
        transform_point_to_overlay(transform, x + w, y, source_w, source_h),
        transform_point_to_overlay(transform, x, y + h, source_w, source_h),
        transform_point_to_overlay(transform, x + w, y + h, source_w, source_h),
    ];
    let min_x = points
        .iter()
        .map(|point| point.0)
        .fold(f64::INFINITY, f64::min);
    let min_y = points
        .iter()
        .map(|point| point.1)
        .fold(f64::INFINITY, f64::min);
    let max_x = points
        .iter()
        .map(|point| point.0)
        .fold(f64::NEG_INFINITY, f64::max);
    let max_y = points
        .iter()
        .map(|point| point.1)
        .fold(f64::NEG_INFINITY, f64::max);

    (min_x, min_y, max_x - min_x, max_y - min_y)
}

pub(crate) fn transform_point_to_overlay(
    transform: niri::OutputTransform,
    pub(crate) x: f64,
    pub(crate) y: f64,
    source_w: f64,
    source_h: f64,
) -> (f64, f64) {
    match transform {
        niri::OutputTransform::Normal => (x, y),
        niri::OutputTransform::_90 => (y, source_w - x),
        niri::OutputTransform::_180 => (source_w - x, source_h - y),
        niri::OutputTransform::_270 => (source_h - y, x),
        niri::OutputTransform::Flipped => (source_w - x, y),
        niri::OutputTransform::Flipped90 => (y, x),
        niri::OutputTransform::Flipped180 => (x, source_h - y),
        niri::OutputTransform::Flipped270 => (source_h - y, source_w - x),
    }
}

