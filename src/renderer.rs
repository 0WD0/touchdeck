use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache};

use crate::geometry::RectPx;

pub(crate) struct TextRenderer {
    font_system: FontSystem,
    swash_cache: SwashCache,
}

#[derive(Clone, Copy)]
pub(crate) struct CanvasSize {
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Clone, Copy)]
pub(crate) struct TextStyle {
    pub(crate) font_size: f32,
    pub(crate) color: [u8; 4],
}

#[derive(Clone, Copy, Default)]
pub(crate) struct KeycapLabels<'a> {
    pub(crate) tap: Option<&'a str>,
    pub(crate) hold: Option<&'a str>,
    pub(crate) up: Option<&'a str>,
    pub(crate) down: Option<&'a str>,
    pub(crate) left: Option<&'a str>,
    pub(crate) right: Option<&'a str>,
}

#[derive(Clone, Copy)]
struct GlyphPlacement {
    x: i32,
    y: i32,
    scale: i32,
}

impl TextRenderer {
    pub(crate) fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
        }
    }

    pub(crate) fn draw_text(
        &mut self,
        buf: &mut [u8],
        size: CanvasSize,
        rect: RectPx,
        text: &str,
        style: TextStyle,
    ) {
        if text.trim().is_empty() || rect.w <= 0 || rect.h <= 0 {
            return;
        }

        let font_size = style.font_size;
        let metrics = Metrics::new(font_size.max(1.0), (font_size * 1.25).max(1.0));
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(Some(rect.w.max(1) as f32), Some(rect.h.max(1) as f32));
        let attrs = Attrs::new().family(Family::Name("Noto Sans CJK SC"));
        buffer.set_text(text, &attrs, Shaping::Advanced, None);

        let color = style.color;
        let text_color = Color::rgba(color[0], color[1], color[2], color[3]);
        buffer.draw(
            &mut self.font_system,
            &mut self.swash_cache,
            text_color,
            |x, y, w, h, color| {
                blend_text_rect(
                    buf,
                    size.width,
                    size.height,
                    RectPx {
                        x: rect.x + x,
                        y: rect.y + y,
                        w: w as i32,
                        h: h as i32,
                    },
                    color,
                );
            },
        );
    }
}

pub(crate) fn draw_rect_frame(
    buf: &mut [u8],
    width: u32,
    height: u32,
    rect: RectPx,
    color: [u8; 4],
) {
    let thickness = 2.max((rect.w.min(rect.h) / 36).max(1));
    fill_rect(
        buf,
        width,
        height,
        RectPx {
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: thickness,
        },
        color,
    );
    fill_rect(
        buf,
        width,
        height,
        RectPx {
            x: rect.x,
            y: rect.y + rect.h - thickness,
            w: rect.w,
            h: thickness,
        },
        color,
    );
    fill_rect(
        buf,
        width,
        height,
        RectPx {
            x: rect.x,
            y: rect.y,
            w: thickness,
            h: rect.h,
        },
        color,
    );
    fill_rect(
        buf,
        width,
        height,
        RectPx {
            x: rect.x + rect.w - thickness,
            y: rect.y,
            w: thickness,
            h: rect.h,
        },
        color,
    );
}

pub(crate) fn draw_keycap_labels(
    buf: &mut [u8],
    width: u32,
    height: u32,
    rect: RectPx,
    labels: KeycapLabels<'_>,
) {
    let hint_color = [0xa8, 0xff, 0xd8, 0xc8];
    let hold_color = [0xff, 0xe0, 0x90, 0xc8];
    let center_color = [0xff, 0xff, 0xff, 0xf0];
    let margin = (rect.w.min(rect.h) / 12).clamp(2, 10);
    let hint_h = (rect.h / 4).max(10);
    let side_w = (rect.w / 3).max(12);

    if let Some(label) = labels.up {
        draw_label_in_rect_limited(
            buf,
            width,
            height,
            RectPx {
                x: rect.x + margin,
                y: rect.y + margin,
                w: rect.w - margin * 2,
                h: hint_h,
            },
            label,
            hint_color,
            3,
        );
    }

    if let Some(label) = labels.down {
        draw_label_in_rect_limited(
            buf,
            width,
            height,
            RectPx {
                x: rect.x + margin,
                y: rect.y + rect.h - hint_h - margin,
                w: rect.w - margin * 2,
                h: hint_h,
            },
            label,
            hint_color,
            3,
        );
    }

    if let Some(label) = labels.left {
        draw_label_in_rect_limited(
            buf,
            width,
            height,
            RectPx {
                x: rect.x + margin,
                y: rect.y + rect.h / 3,
                w: side_w,
                h: rect.h / 3,
            },
            label,
            hint_color,
            3,
        );
    }

    if let Some(label) = labels.right {
        draw_label_in_rect_limited(
            buf,
            width,
            height,
            RectPx {
                x: rect.x + rect.w - side_w - margin,
                y: rect.y + rect.h / 3,
                w: side_w,
                h: rect.h / 3,
            },
            label,
            hint_color,
            3,
        );
    }

    if labels.tap.is_some() {
        if let Some(label) = labels.hold {
            draw_label_in_rect_limited(
                buf,
                width,
                height,
                RectPx {
                    x: rect.x + margin,
                    y: rect.y + rect.h - hint_h - margin,
                    w: side_w,
                    h: hint_h,
                },
                label,
                hold_color,
                2,
            );
        }
    }

    let center_label = labels.tap.or(labels.hold);
    let center_color = if labels.tap.is_some() {
        center_color
    } else {
        hold_color
    };

    if let Some(label) = center_label {
        draw_label_in_rect_limited(
            buf,
            width,
            height,
            RectPx {
                x: rect.x + rect.w / 4,
                y: rect.y + rect.h / 3,
                w: rect.w / 2,
                h: rect.h / 3,
            },
            label,
            center_color,
            8,
        );
    }
}

pub(crate) fn draw_label_in_rect(
    buf: &mut [u8],
    width: u32,
    height: u32,
    rect: RectPx,
    label: &str,
    color: [u8; 4],
) {
    draw_label_in_rect_limited(buf, width, height, rect, label, color, 8);
}

pub(crate) fn draw_label_in_rect_limited(
    buf: &mut [u8],
    width: u32,
    height: u32,
    rect: RectPx,
    label: &str,
    color: [u8; 4],
    max_scale: i32,
) {
    let text = label
        .chars()
        .filter(|ch| ch.is_ascii_graphic() || *ch == ' ')
        .take(8)
        .map(|ch| ch.to_ascii_uppercase())
        .collect::<Vec<_>>();
    if text.is_empty() {
        return;
    }

    let glyph_w = 3;
    let glyph_h = 5;
    let spacing = 1;
    let total_units = text.len() as i32 * glyph_w + (text.len() as i32 - 1) * spacing;
    let scale_x = (rect.w / (total_units + 2)).max(1);
    let scale_y = (rect.h / (glyph_h + 2)).max(1);
    let scale = scale_x.min(scale_y).clamp(1, max_scale.max(1));
    let total_w = total_units * scale;
    let total_h = glyph_h * scale;
    let mut x = rect.x + (rect.w - total_w) / 2;
    let y = rect.y + (rect.h - total_h) / 2;

    for ch in text {
        draw_glyph(
            buf,
            width,
            height,
            GlyphPlacement { x, y, scale },
            ch,
            color,
        );
        x += (glyph_w + spacing) * scale;
    }
}

fn blend_text_rect(buf: &mut [u8], width: u32, height: u32, rect: RectPx, color: Color) {
    let x0 = rect.x.max(0).min(width as i32) as u32;
    let y0 = rect.y.max(0).min(height as i32) as u32;
    let x1 = (rect.x + rect.w).max(0).min(width as i32) as u32;
    let y1 = (rect.y + rect.h).max(0).min(height as i32) as u32;
    if x0 >= x1 || y0 >= y1 {
        return;
    }

    let [src_r, src_g, src_b, src_a] = color.as_rgba();
    if src_a == 0 {
        return;
    }

    for y in y0..y1 {
        for x in x0..x1 {
            let index = ((y * width + x) * 4) as usize;
            let dst_b = buf[index];
            let dst_g = buf[index + 1];
            let dst_r = buf[index + 2];
            let dst_a = buf[index + 3];
            let out_a = src_a as u16 + ((dst_a as u16 * (255 - src_a as u16)) / 255);

            buf[index] = alpha_over(src_b, src_a, dst_b);
            buf[index + 1] = alpha_over(src_g, src_a, dst_g);
            buf[index + 2] = alpha_over(src_r, src_a, dst_r);
            buf[index + 3] = out_a.min(255) as u8;
        }
    }
}

fn alpha_over(src: u8, src_a: u8, dst: u8) -> u8 {
    let src = src as u16;
    let src_a = src_a as u16;
    let dst = dst as u16;
    ((src * src_a + dst * (255 - src_a)) / 255).min(255) as u8
}

fn draw_glyph(
    buf: &mut [u8],
    width: u32,
    height: u32,
    placement: GlyphPlacement,
    ch: char,
    color: [u8; 4],
) {
    let pattern = glyph_3x5(ch);
    for (row, bits) in pattern.iter().enumerate() {
        for col in 0usize..3 {
            if *bits & (1u8 << (2 - col)) == 0 {
                continue;
            }

            fill_rect(
                buf,
                width,
                height,
                RectPx {
                    x: placement.x + col as i32 * placement.scale,
                    y: placement.y + row as i32 * placement.scale,
                    w: placement.scale,
                    h: placement.scale,
                },
                color,
            );
        }
    }
}

fn glyph_3x5(ch: char) -> [u8; 5] {
    match ch {
        'A' => [0b010, 0b101, 0b111, 0b101, 0b101],
        'B' => [0b110, 0b101, 0b110, 0b101, 0b110],
        'C' => [0b111, 0b100, 0b100, 0b100, 0b111],
        'D' => [0b110, 0b101, 0b101, 0b101, 0b110],
        'E' => [0b111, 0b100, 0b110, 0b100, 0b111],
        'F' => [0b111, 0b100, 0b110, 0b100, 0b100],
        'G' => [0b111, 0b100, 0b101, 0b101, 0b111],
        'H' => [0b101, 0b101, 0b111, 0b101, 0b101],
        'I' => [0b111, 0b010, 0b010, 0b010, 0b111],
        'J' => [0b001, 0b001, 0b001, 0b101, 0b111],
        'K' => [0b101, 0b101, 0b110, 0b101, 0b101],
        'L' => [0b100, 0b100, 0b100, 0b100, 0b111],
        'M' => [0b101, 0b111, 0b111, 0b101, 0b101],
        'N' => [0b101, 0b111, 0b111, 0b111, 0b101],
        'O' => [0b111, 0b101, 0b101, 0b101, 0b111],
        'P' => [0b111, 0b101, 0b111, 0b100, 0b100],
        'Q' => [0b111, 0b101, 0b101, 0b111, 0b001],
        'R' => [0b110, 0b101, 0b110, 0b101, 0b101],
        'S' => [0b111, 0b100, 0b111, 0b001, 0b111],
        'T' => [0b111, 0b010, 0b010, 0b010, 0b010],
        'U' => [0b101, 0b101, 0b101, 0b101, 0b111],
        'V' => [0b101, 0b101, 0b101, 0b101, 0b010],
        'W' => [0b101, 0b101, 0b111, 0b111, 0b101],
        'X' => [0b101, 0b101, 0b010, 0b101, 0b101],
        'Y' => [0b101, 0b101, 0b010, 0b010, 0b010],
        'Z' => [0b111, 0b001, 0b010, 0b100, 0b111],
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b010, 0b010, 0b010],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        '-' => [0b000, 0b000, 0b111, 0b000, 0b000],
        '_' => [0b000, 0b000, 0b000, 0b000, 0b111],
        '+' => [0b000, 0b010, 0b111, 0b010, 0b000],
        '=' => [0b000, 0b111, 0b000, 0b111, 0b000],
        '!' => [0b010, 0b010, 0b010, 0b000, 0b010],
        '?' => [0b111, 0b001, 0b010, 0b000, 0b010],
        '@' => [0b111, 0b101, 0b111, 0b100, 0b111],
        '#' => [0b101, 0b111, 0b101, 0b111, 0b101],
        '$' => [0b011, 0b110, 0b010, 0b011, 0b110],
        '%' => [0b101, 0b001, 0b010, 0b100, 0b101],
        '^' => [0b010, 0b101, 0b000, 0b000, 0b000],
        '&' => [0b010, 0b101, 0b010, 0b101, 0b011],
        '*' => [0b101, 0b010, 0b111, 0b010, 0b101],
        '(' => [0b001, 0b010, 0b010, 0b010, 0b001],
        ')' => [0b100, 0b010, 0b010, 0b010, 0b100],
        '[' => [0b111, 0b100, 0b100, 0b100, 0b111],
        ']' => [0b111, 0b001, 0b001, 0b001, 0b111],
        '{' => [0b011, 0b010, 0b110, 0b010, 0b011],
        '}' => [0b110, 0b010, 0b011, 0b010, 0b110],
        ';' => [0b000, 0b010, 0b000, 0b010, 0b100],
        ':' => [0b000, 0b010, 0b000, 0b010, 0b000],
        '\'' => [0b010, 0b010, 0b000, 0b000, 0b000],
        '"' => [0b101, 0b101, 0b000, 0b000, 0b000],
        '`' => [0b100, 0b010, 0b000, 0b000, 0b000],
        '~' => [0b000, 0b011, 0b110, 0b000, 0b000],
        '\\' => [0b100, 0b100, 0b010, 0b001, 0b001],
        '|' => [0b010, 0b010, 0b010, 0b010, 0b010],
        ',' => [0b000, 0b000, 0b000, 0b010, 0b100],
        '.' => [0b000, 0b000, 0b000, 0b000, 0b010],
        '/' => [0b001, 0b001, 0b010, 0b100, 0b100],
        '<' => [0b001, 0b010, 0b100, 0b010, 0b001],
        '>' => [0b100, 0b010, 0b001, 0b010, 0b100],
        ' ' => [0b000, 0b000, 0b000, 0b000, 0b000],
        _ => [0b111, 0b001, 0b010, 0b000, 0b010],
    }
}

pub(crate) fn fill_rect(buf: &mut [u8], width: u32, height: u32, rect: RectPx, color: [u8; 4]) {
    let x0 = rect.x.max(0).min(width as i32) as u32;
    let y0 = rect.y.max(0).min(height as i32) as u32;
    let x1 = (rect.x + rect.w).max(0).min(width as i32) as u32;
    let y1 = (rect.y + rect.h).max(0).min(height as i32) as u32;

    for y in y0..y1 {
        for x in x0..x1 {
            let index = ((y * width + x) * 4) as usize;
            buf[index..index + 4].copy_from_slice(&color);
        }
    }
}

pub(crate) fn draw_circle(
    buf: &mut [u8],
    width: u32,
    height: u32,
    cx: i32,
    cy: i32,
    radius: i32,
    color: [u8; 4],
) {
    let r2 = radius * radius;
    let x0 = (cx - radius).max(0);
    let y0 = (cy - radius).max(0);
    let x1 = (cx + radius).min(width as i32 - 1);
    let y1 = (cy + radius).min(height as i32 - 1);

    for y in y0..=y1 {
        for x in x0..=x1 {
            let dx = x - cx;
            let dy = y - cy;
            if dx * dx + dy * dy <= r2 {
                let index = (((y as u32) * width + x as u32) * 4) as usize;
                buf[index..index + 4].copy_from_slice(&color);
            }
        }
    }
}
