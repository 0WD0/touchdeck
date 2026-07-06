use touchdeck::niri;
use touchdeck::protocol::{ImeCandidate, ImeCursorRect, ImeStatus};

use crate::geometry::RectPx;
use crate::mode::Mode;
use crate::renderer::{draw_rect_frame, fill_rect, CanvasSize, TextRenderer, TextStyle};

pub(crate) fn should_render_ime_status(status: &ImeStatus, mode: Mode) -> bool {
    if status.preedit.is_empty() && status.commit_preview.is_empty() && status.candidates.is_empty()
    {
        return false;
    }

    (mode == Mode::Text && status.ui_owner == "touchdeck-overlay")
        || should_render_fcitx_server_popup(status)
}

pub(crate) fn render_ime_status(
    renderer: &mut TextRenderer,
    mmap: &mut [u8],
    width: u32,
    height: u32,
    status: &ImeStatus,
) {
    if status.preedit.is_empty() && status.commit_preview.is_empty() && status.candidates.is_empty()
    {
        return;
    }

    if should_render_fcitx_server_popup(status) {
        if let Some(cursor_rect) = status.cursor_rect.clone() {
            render_physical_ime_status(renderer, mmap, width, height, status, cursor_rect);
            return;
        }
    }

    if status.ui_owner != "touchdeck-overlay" {
        return;
    }

    render_touch_ime_status(renderer, mmap, width, height, status);
}

fn should_render_fcitx_server_popup(status: &ImeStatus) -> bool {
    status.ui_owner == "touchdeck-server-popup" && status.cursor_rect.is_some()
}

fn render_touch_ime_status(
    renderer: &mut TextRenderer,
    mmap: &mut [u8],
    width: u32,
    height: u32,
    status: &ImeStatus,
) {
    let panel_x = ((width as i32 * 3) / 100).max(4);
    let panel_w = (width as i32 - panel_x * 2).max(80);
    let panel_h = ((height as i32 * 78) / 1000).clamp(54, 118);
    let panel_y = ((height as i32 * 430) / 1000)
        .max(8)
        .min((height as i32 - panel_h - 8).max(0));
    let panel = RectPx {
        x: panel_x,
        y: panel_y,
        w: panel_w,
        h: panel_h,
    };

    fill_rect(mmap, width, height, panel, [0x08, 0x14, 0x18, 0xd8]);
    draw_rect_frame(mmap, width, height, panel, [0x40, 0xff, 0xd0, 0xc8]);

    let header_h = (panel.h * 42 / 100).max(22);
    let header = ime_header_text(status);
    renderer.draw_text(
        mmap,
        CanvasSize { width, height },
        RectPx {
            x: panel.x + 8,
            y: panel.y + 4,
            w: panel.w - 16,
            h: header_h - 6,
        },
        &header,
        TextStyle {
            font_size: (header_h as f32 * 0.55).clamp(14.0, 34.0),
            color: [0xff, 0xff, 0xff, 0xe8],
        },
    );

    let row_y = panel.y + header_h;
    let row_h = (panel.h - header_h - 8).max(18);
    let visible = status.candidates.iter().take(5).collect::<Vec<_>>();
    if visible.is_empty() {
        return;
    }

    let gap = 6;
    let box_w = ((panel.w - 16 - gap * (visible.len() as i32 - 1)) / visible.len() as i32).max(24);
    for (index, candidate) in visible.into_iter().enumerate() {
        let rect = RectPx {
            x: panel.x + 8 + index as i32 * (box_w + gap),
            y: row_y,
            w: box_w,
            h: row_h,
        };
        let highlighted = status.highlighted_candidate_index == Some(index);
        let fill = if highlighted {
            [0x10, 0xff, 0xb0, 0xa0]
        } else {
            [0x18, 0x38, 0x34, 0xb8]
        };
        fill_rect(mmap, width, height, rect, fill);
        draw_rect_frame(mmap, width, height, rect, [0xb0, 0xff, 0xe0, 0xb0]);

        let label = ime_candidate_label(index, candidate);
        renderer.draw_text(
            mmap,
            CanvasSize { width, height },
            rect,
            &label,
            TextStyle {
                font_size: (rect.h as f32 * 0.45).clamp(13.0, 28.0),
                color: [0xff, 0xff, 0xff, 0xf0],
            },
        );
    }
}

fn render_physical_ime_status(
    renderer: &mut TextRenderer,
    mmap: &mut [u8],
    width: u32,
    height: u32,
    status: &ImeStatus,
    cursor_rect: ImeCursorRect,
) {
    let screen_w = width as i32;
    let screen_h = height as i32;
    if screen_w <= 0 || screen_h <= 0 {
        return;
    }

    let candidate_count = status.candidates.iter().take(6).count();
    let panel_w = 560.min(screen_w - 16).max(220);
    let panel_h = if candidate_count == 0 { 48 } else { 88 }
        .min(screen_h - 16)
        .max(44);

    let Some((cursor_x, cursor_y, cursor_h)) = physical_ime_anchor(cursor_rect, screen_w, screen_h)
    else {
        return;
    };

    let panel_x = cursor_x.clamp(0, (screen_w - panel_w).max(0));
    let below_y = cursor_y;
    let above_y = cursor_y - panel_h;
    let panel_y = if below_y + panel_h <= screen_h {
        below_y
    } else {
        above_y.max(0)
    };

    let panel = RectPx {
        x: panel_x,
        y: panel_y,
        w: panel_w,
        h: panel_h,
    };

    if log_geometry() {
        eprintln!(
            "touchdeck: ime geometry panel anchor=({}, {} h={}) panel=({}, {} {}x{}) screen={}x{}",
            cursor_x, cursor_y, cursor_h, panel.x, panel.y, panel.w, panel.h, screen_w, screen_h
        );
    }

    fill_rect(mmap, width, height, panel, [0x1a, 0x22, 0x26, 0xe6]);
    draw_rect_frame(mmap, width, height, panel, [0x79, 0x8b, 0x86, 0x96]);
    if log_geometry() {
        fill_rect(
            mmap,
            width,
            height,
            RectPx {
                x: cursor_x - 14,
                y: cursor_y - 1,
                w: 29,
                h: 3,
            },
            [0x20, 0x40, 0xff, 0xf0],
        );
        fill_rect(
            mmap,
            width,
            height,
            RectPx {
                x: cursor_x - 1,
                y: cursor_y - 14,
                w: 3,
                h: 29,
            },
            [0x20, 0x40, 0xff, 0xf0],
        );
        draw_rect_frame(
            mmap,
            width,
            height,
            RectPx {
                x: cursor_x - 6,
                y: cursor_y - 6,
                w: 13,
                h: 13,
            },
            [0xff, 0xff, 0xff, 0xf0],
        );
    }

    let header_h = if candidate_count == 0 {
        panel.h
    } else {
        (panel.h * 40 / 100).max(30)
    };
    let header = ime_header_text(status);
    renderer.draw_text(
        mmap,
        CanvasSize { width, height },
        RectPx {
            x: panel.x + 12,
            y: panel.y + 4,
            w: panel.w - 24,
            h: header_h - 6,
        },
        &header,
        TextStyle {
            font_size: (header_h as f32 * 0.52).clamp(15.0, 30.0),
            color: [0xd8, 0xde, 0xe8, 0xee],
        },
    );

    let visible = status.candidates.iter().take(6).collect::<Vec<_>>();
    if visible.is_empty() {
        return;
    }

    fill_rect(
        mmap,
        width,
        height,
        RectPx {
            x: panel.x + 10,
            y: panel.y + header_h,
            w: panel.w - 20,
            h: 1,
        },
        [0x6c, 0x78, 0x72, 0x70],
    );

    let row_y = panel.y + header_h + 7;
    let row_h = panel.h - header_h - 14;
    let gap = 6;
    let mut x = panel.x + 10;
    let right = panel.x + panel.w - 10;
    for (index, candidate) in visible.into_iter().enumerate() {
        let label = ime_candidate_label(index, candidate);
        let text_units = label
            .chars()
            .map(|ch| if ch.is_ascii() { 1 } else { 2 })
            .sum::<i32>();
        let rect_w = (text_units * 8 + 26).clamp(48, 154);
        if x + rect_w > right {
            break;
        }

        let rect = RectPx {
            x,
            y: row_y,
            w: rect_w,
            h: row_h,
        };
        let highlighted = status.highlighted_candidate_index == Some(index);
        let fill = if highlighted {
            [0x3b, 0x86, 0xf2, 0xdc]
        } else if index == 0 {
            [0x2e, 0x3d, 0x44, 0x70]
        } else {
            [0x00, 0x00, 0x00, 0x00]
        };
        fill_rect(mmap, width, height, rect, fill);
        renderer.draw_text(
            mmap,
            CanvasSize { width, height },
            RectPx {
                x: rect.x + 8,
                y: rect.y + 3,
                w: rect.w - 16,
                h: rect.h - 6,
            },
            &label,
            TextStyle {
                font_size: (rect.h as f32 * 0.48).clamp(14.0, 24.0),
                color: [0xff, 0xff, 0xff, 0xf0],
            },
        );

        x += rect.w + gap;
    }
}

fn physical_ime_anchor(
    cursor_rect: ImeCursorRect,
    screen_w: i32,
    screen_h: i32,
) -> Option<(i32, i32, i32)> {
    if cursor_rect.space == "x11-root" {
        return x11_ime_anchor(cursor_rect, screen_w, screen_h);
    }

    let scale = if cursor_rect.scale.is_finite() && cursor_rect.scale > 0.0 {
        cursor_rect.scale
    } else {
        1.0
    };
    Some((
        ((cursor_rect.x as f64) / scale)
            .round()
            .clamp(0.0, screen_w.saturating_sub(1) as f64) as i32,
        ((cursor_rect.y as f64) / scale)
            .round()
            .clamp(0.0, screen_h.saturating_sub(1) as f64) as i32,
        ((cursor_rect.h.max(0) as f64) / scale).round() as i32,
    ))
}

fn x11_ime_anchor(
    cursor_rect: ImeCursorRect,
    screen_w: i32,
    screen_h: i32,
) -> Option<(i32, i32, i32)> {
    let (Some(window_x), Some(window_y), Some(window_w), Some(window_h)) = (
        cursor_rect.window_x,
        cursor_rect.window_y,
        cursor_rect.window_w,
        cursor_rect.window_h,
    ) else {
        if log_geometry() {
            eprintln!(
                "touchdeck: ime geometry x11-root missing window geometry raw=({}, {} {}x{}) root={:?}x{:?}",
                cursor_rect.x,
                cursor_rect.y,
                cursor_rect.w,
                cursor_rect.h,
                cursor_rect.root_w,
                cursor_rect.root_h
            );
        }
        return None;
    };
    if window_w <= 0 || window_h <= 0 {
        if log_geometry() {
            eprintln!(
                "touchdeck: ime geometry x11-root invalid window geometry window=({}, {} {}x{}) raw=({}, {} {}x{})",
                window_x,
                window_y,
                window_w,
                window_h,
                cursor_rect.x,
                cursor_rect.y,
                cursor_rect.w,
                cursor_rect.h
            );
        }
        return None;
    }

    let layout = match niri::focused_window_layout() {
        Ok(Some(layout)) => layout,
        Ok(None) => {
            if log_geometry() {
                eprintln!(
                    "touchdeck: ime geometry x11-root no focused niri window window=({}, {} {}x{}) raw=({}, {} {}x{})",
                    window_x,
                    window_y,
                    window_w,
                    window_h,
                    cursor_rect.x,
                    cursor_rect.y,
                    cursor_rect.w,
                    cursor_rect.h
                );
            }
            return None;
        }
        Err(err) => {
            eprintln!(
                "touchdeck: failed to query niri focused window for xwayland IME popup: {err:?}"
            );
            return None;
        }
    };

    let (window_output_x, window_output_y, output_window_w, output_window_h) =
        layout.window_rect_in_output;
    let output_layout = niri::focused_output_layout().ok().flatten();
    let origin_x = window_output_x;
    let origin_y = window_output_y;
    let window_size_w = output_window_w as f64;
    let window_size_h = output_window_h as f64;
    if window_size_w <= 0.0 || window_size_h <= 0.0 {
        return None;
    }

    let scale_x = window_w as f64 / window_size_w;
    let scale_y = window_h as f64 / window_size_h;
    if !scale_x.is_finite() || !scale_y.is_finite() || scale_x <= 0.0 || scale_y <= 0.0 {
        if log_geometry() {
            eprintln!(
                "touchdeck: ime geometry x11-root invalid scale raw=({}, {} {}x{}) x11_window=({}, {} {}x{}) niri_output={:?} niri_window_rect=({:.2}, {:.2} {}x{}) overlay_origin=({:.2}, {:.2}) scale=({:.4}, {:.4})",
                cursor_rect.x,
                cursor_rect.y,
                cursor_rect.w,
                cursor_rect.h,
                window_x,
                window_y,
                window_w,
                window_h,
                output_layout,
                window_output_x,
                window_output_y,
                output_window_w,
                output_window_h,
                origin_x,
                origin_y,
                scale_x,
                scale_y
            );
        }
        return None;
    }

    let local_x = (cursor_rect.x - window_x) as f64;
    let local_y = (cursor_rect.y - window_y) as f64;
    let cursor_x = (origin_x + local_x / scale_x)
        .round()
        .clamp(0.0, screen_w.saturating_sub(1) as f64) as i32;
    let cursor_y = (origin_y + local_y / scale_y)
        .round()
        .clamp(0.0, screen_h.saturating_sub(1) as f64) as i32;
    let cursor_h = ((cursor_rect.h.max(0) as f64) / scale_y).round() as i32;

    if log_geometry() {
        eprintln!(
            "touchdeck: ime geometry x11-root raw=({}, {} {}x{}) root={:?}x{:?} x11_window=({}, {} {}x{}) niri_output={:?} niri_window_rect=({:.2}, {:.2} {}x{}) overlay_origin=({:.2}, {:.2}) scale=({:.4}, {:.4}) local=({:.2}, {:.2}) anchor=({}, {} h={}) screen={}x{}",
            cursor_rect.x,
            cursor_rect.y,
            cursor_rect.w,
            cursor_rect.h,
            cursor_rect.root_w,
            cursor_rect.root_h,
            window_x,
            window_y,
            window_w,
            window_h,
            output_layout,
            window_output_x,
            window_output_y,
            output_window_w,
            output_window_h,
            origin_x,
            origin_y,
            scale_x,
            scale_y,
            local_x,
            local_y,
            cursor_x,
            cursor_y,
            cursor_h,
            screen_w,
            screen_h
        );
    }

    Some((cursor_x, cursor_y, cursor_h))
}

fn ime_header_text(status: &ImeStatus) -> String {
    let mut header = String::new();
    if !status.preedit.is_empty() {
        header.push_str(&status.preedit);
    }
    if !status.commit_preview.is_empty() {
        if !header.is_empty() {
            header.push_str(" > ");
        }
        header.push_str(&status.commit_preview);
    }
    if header.is_empty() {
        header.push_str("IME");
    }
    header
}

fn ime_candidate_label(index: usize, candidate: &ImeCandidate) -> String {
    let mut label = candidate.label.clone();
    if !candidate.text.is_empty() {
        label.push(' ');
        label.push_str(&candidate.text);
    }
    if label.trim().is_empty() {
        label = format!("{}", index + 1);
    }
    label
}

fn log_geometry() -> bool {
    std::env::var_os("TOUCHDECK_LOG_IME_GEOMETRY").is_some()
}
