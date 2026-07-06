use std::collections::VecDeque;
use std::env;
use std::fs::File;
use std::os::fd::AsFd;

use anyhow::{anyhow, Context, Result};
use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache};
use memmap2::MmapMut;
use tempfile::tempfile;
use touchdeck::protocol::ImeStatus;
use wayland_client::protocol::{wl_buffer, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::QueueHandle;

use super::config::PopupConfig;
use super::ImeApp;

#[derive(Default)]
pub(super) struct PopupRenderer {
    buffers: VecDeque<PopupBuffer>,
    text: TextRenderer,
}

impl PopupRenderer {
    pub(super) fn hide_surface(&mut self, surface: &wl_surface::WlSurface) {
        surface.attach(None::<&wl_buffer::WlBuffer>, 0, 0);
        surface.commit();
    }

    pub(super) fn render_to_surface(
        &mut self,
        qh: &QueueHandle<ImeApp>,
        shm: &wl_shm::WlShm,
        surface: &wl_surface::WlSurface,
        status: &ImeStatus,
        config: &PopupConfig,
    ) -> Result<()> {
        let (backing, width, height) = self.create_buffer(qh, shm, status, config)?;
        surface.attach(Some(&backing.buffer), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.commit();

        self.buffers.push_back(backing);
        self.retain_live_buffers();
        while self.buffers.len() > 8 {
            self.buffers.pop_front();
        }

        Ok(())
    }

    pub(super) fn release_buffer(&mut self, buffer: &wl_buffer::WlBuffer) {
        for backing in &mut self.buffers {
            if backing.buffer == buffer.clone() {
                backing.released = true;
                break;
            }
        }
        self.retain_live_buffers();
    }

    fn retain_live_buffers(&mut self) {
        self.buffers.retain(|buffer| !buffer.released);
    }

    fn create_buffer(
        &mut self,
        qh: &QueueHandle<ImeApp>,
        shm: &wl_shm::WlShm,
        status: &ImeStatus,
        config: &PopupConfig,
    ) -> Result<(PopupBuffer, i32, i32)> {
        let (width, height) = popup_dimensions(status, config);
        let stride = width
            .checked_mul(4)
            .ok_or_else(|| anyhow!("invalid popup buffer stride"))?;
        let len = stride
            .checked_mul(height)
            .ok_or_else(|| anyhow!("invalid popup buffer size"))?;

        let file = tempfile().context("create popup shm backing file")?;
        file.set_len(u64::from(len))
            .context("resize popup shm backing file")?;
        let mut mmap = unsafe { MmapMut::map_mut(&file).context("map popup shm backing file")? };
        mmap.fill(0);
        draw_popup_status(&mut self.text, &mut mmap, width, height, status, config);
        mmap.flush().context("flush popup shm backing file")?;

        let pool = shm.create_pool(file.as_fd(), len as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            wl_shm::Format::Argb8888,
            qh,
            (),
        );

        Ok((
            PopupBuffer {
                _file: file,
                _mmap: mmap,
                _pool: pool,
                buffer,
                released: false,
            },
            width as i32,
            height as i32,
        ))
    }
}

struct PopupBuffer {
    _file: File,
    _mmap: MmapMut,
    _pool: wl_shm_pool::WlShmPool,
    buffer: wl_buffer::WlBuffer,
    released: bool,
}

#[derive(Clone, Copy, Debug)]
struct RectPx {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

#[derive(Clone, Copy, Debug)]
struct CanvasSize {
    width: u32,
    height: u32,
}

struct TextRenderer {
    font_system: FontSystem,
    swash_cache: SwashCache,
}

impl Default for TextRenderer {
    fn default() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
        }
    }
}

impl TextRenderer {
    fn draw_text(
        &mut self,
        buf: &mut [u8],
        size: CanvasSize,
        rect: RectPx,
        text: &str,
        font_size: f32,
        color: [u8; 4],
    ) {
        if text.trim().is_empty() || rect.w <= 0 || rect.h <= 0 {
            return;
        }

        let metrics = Metrics::new(font_size.max(1.0), (font_size * 1.25).max(1.0));
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(Some(rect.w.max(1) as f32), Some(rect.h.max(1) as f32));
        let attrs = Attrs::new().family(Family::Name("Noto Sans CJK SC"));
        buffer.set_text(text, &attrs, Shaping::Advanced, None);

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

fn popup_dimensions(status: &ImeStatus, popup: &PopupConfig) -> (u32, u32) {
    let width = env_u32("TOUCHDECK_IME_POPUP_WIDTH", popup.width).clamp(220, 1600);
    let candidate_count = status.candidates.iter().take(popup.max_candidates).count();
    let height = if candidate_count == 0 {
        popup.height_empty
    } else {
        popup.height_candidates
    };

    (width, height)
}

fn draw_popup_status(
    renderer: &mut TextRenderer,
    buf: &mut [u8],
    width: u32,
    height: u32,
    status: &ImeStatus,
    popup: &PopupConfig,
) {
    let size = CanvasSize { width, height };
    let panel = RectPx {
        x: 0,
        y: 0,
        w: width as i32,
        h: height as i32,
    };
    fill_rect(buf, width, height, panel, popup.background_color.bgra());
    draw_rect_frame(buf, width, height, panel, popup.border_color.bgra());

    let header_h = if status.candidates.is_empty() {
        height as i32
    } else {
        popup.header_height.min(height as i32)
    };
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

    renderer.draw_text(
        buf,
        size,
        RectPx {
            x: popup.padding_x + 2,
            y: popup.header_y,
            w: width as i32 - (popup.padding_x + 2) * 2,
            h: header_h - popup.header_y,
        },
        &header,
        popup.preedit_font_size,
        popup.preedit_color.rgba(),
    );

    let visible = status
        .candidates
        .iter()
        .take(popup.max_candidates)
        .collect::<Vec<_>>();
    if visible.is_empty() {
        return;
    }

    fill_rect(
        buf,
        width,
        height,
        RectPx {
            x: popup.padding_x,
            y: header_h,
            w: width as i32 - popup.padding_x * 2,
            h: 1,
        },
        popup.separator_color.bgra(),
    );

    let gap = popup.candidate_gap;
    let row_y = header_h + 7;
    let row_h = height as i32 - row_y - 8;
    let mut x = popup.padding_x;
    for (index, candidate) in visible.into_iter().enumerate() {
        let mut label = candidate.label.clone();
        if label.trim().is_empty() {
            label = format!("{}", index + 1);
        }
        if !candidate.text.is_empty() {
            label.push(' ');
            label.push_str(&candidate.text);
        }

        let text_units = label
            .chars()
            .map(|ch| if ch.is_ascii() { 1 } else { 2 })
            .sum::<i32>();
        let rect_w = (text_units * popup.candidate_unit_width + popup.candidate_extra_width)
            .clamp(popup.candidate_min_width, popup.candidate_max_width);
        if x + rect_w > width as i32 - popup.padding_x {
            break;
        }

        let rect = RectPx {
            x,
            y: row_y,
            w: rect_w,
            h: row_h,
        };
        let highlighted = status.highlighted_candidate_index == Some(index);
        if highlighted {
            fill_rect(
                buf,
                width,
                height,
                rect,
                popup.highlight_background_color.bgra(),
            );
        } else if index == 0 {
            fill_rect(
                buf,
                width,
                height,
                rect,
                popup.first_candidate_background_color.bgra(),
            );
        }

        renderer.draw_text(
            buf,
            size,
            RectPx {
                x: rect.x + 8,
                y: rect.y + 4,
                w: rect.w - 16,
                h: rect.h - 8,
            },
            &label,
            popup.candidate_font_size,
            popup.candidate_text_color.rgba(),
        );
        x += rect.w + gap;
    }
}

fn draw_rect_frame(buf: &mut [u8], width: u32, height: u32, rect: RectPx, color: [u8; 4]) {
    fill_rect(
        buf,
        width,
        height,
        RectPx {
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: 2,
        },
        color,
    );
    fill_rect(
        buf,
        width,
        height,
        RectPx {
            x: rect.x,
            y: rect.y + rect.h - 2,
            w: rect.w,
            h: 2,
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
            w: 2,
            h: rect.h,
        },
        color,
    );
    fill_rect(
        buf,
        width,
        height,
        RectPx {
            x: rect.x + rect.w - 2,
            y: rect.y,
            w: 2,
            h: rect.h,
        },
        color,
    );
}

fn fill_rect(buf: &mut [u8], width: u32, height: u32, rect: RectPx, color: [u8; 4]) {
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

fn env_u32(name: &str, default: u32) -> u32 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(default)
}
