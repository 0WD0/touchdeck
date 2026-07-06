use crate::config::Config;
use crate::geometry::SurfaceSize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GestureKind {
    Tap,
    SwipeLeft,
    SwipeRight,
    SwipeUp,
    SwipeDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SwipeDirection {
    Left,
    Right,
    Up,
    Down,
}

impl SwipeDirection {
    pub(crate) fn as_gesture_kind(self) -> GestureKind {
        match self {
            Self::Left => GestureKind::SwipeLeft,
            Self::Right => GestureKind::SwipeRight,
            Self::Up => GestureKind::SwipeUp,
            Self::Down => GestureKind::SwipeDown,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Contact {
    pub(crate) id: i32,
    pub(crate) start_x: f64,
    pub(crate) start_y: f64,
    pub(crate) last_x: f64,
    pub(crate) last_y: f64,
    pub(crate) start_time: u32,
    pub(crate) last_time: u32,
}

#[derive(Debug, Default)]
pub(crate) struct Gesture {
    pub(crate) finished: Vec<Contact>,
    pub(crate) max_active: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TapRecord {
    pub(crate) t_ms: u64,
    pub(crate) x: f64,
    pub(crate) y: f64,
}

pub(crate) fn recognize_gesture_kind(
    gesture: &Gesture,
    config: &Config,
    size: SurfaceSize,
) -> Option<GestureKind> {
    if gesture.max_active == 0 || gesture.finished.is_empty() {
        return None;
    }

    if is_tap_like(gesture, config.tap_radius, config.two_finger_tap_ms) {
        return Some(GestureKind::Tap);
    }

    let min_dim = f64::from(size.width.min(size.height).max(1));
    let swipe_threshold_min = config.swipe_threshold_min.min(config.swipe_threshold_max);
    let swipe_threshold_max = config.swipe_threshold_min.max(config.swipe_threshold_max);
    let swipe_threshold =
        (min_dim * config.swipe_threshold_ratio).clamp(swipe_threshold_min, swipe_threshold_max);
    let contact_count = gesture.finished.len() as f64;
    let dx = gesture
        .finished
        .iter()
        .map(|contact| contact.last_x - contact.start_x)
        .sum::<f64>()
        / contact_count;
    let dy = gesture
        .finished
        .iter()
        .map(|contact| contact.last_y - contact.start_y)
        .sum::<f64>()
        / contact_count;
    let abs_dx = dx.abs();
    let abs_dy = dy.abs();

    if abs_dx.max(abs_dy) < swipe_threshold {
        return None;
    }

    if abs_dx >= abs_dy * 1.25 {
        if dx < 0.0 {
            Some(GestureKind::SwipeLeft)
        } else {
            Some(GestureKind::SwipeRight)
        }
    } else if abs_dy >= abs_dx * 1.25 {
        if dy < 0.0 {
            Some(GestureKind::SwipeUp)
        } else {
            Some(GestureKind::SwipeDown)
        }
    } else {
        None
    }
}

pub(crate) fn is_tap_like(gesture: &Gesture, radius: f64, max_ms: u32) -> bool {
    let start = gesture
        .finished
        .iter()
        .map(|contact| contact.start_time)
        .min()
        .unwrap_or(0);
    let end = gesture
        .finished
        .iter()
        .map(|contact| contact.last_time)
        .max()
        .unwrap_or(start);

    if end.saturating_sub(start) > max_ms {
        return false;
    }

    gesture
        .finished
        .iter()
        .all(|contact| contact_movement(contact) <= radius)
}

pub(crate) fn contact_movement(contact: &Contact) -> f64 {
    let dx = contact.last_x - contact.start_x;
    let dy = contact.last_y - contact.start_y;
    dx.hypot(dy)
}
