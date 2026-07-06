use std::collections::VecDeque;
use std::fs::File;
use std::os::fd::AsFd;

use anyhow::{anyhow, Context, Result};
use memmap2::MmapMut;
use tempfile::tempfile;
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_output, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::QueueHandle;
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

use crate::engine::CapturePolicy;
use crate::geometry::SurfaceSize;
use crate::App;

#[derive(Default)]
pub(crate) struct Overlay {
    surface: Option<wl_surface::WlSurface>,
    layer_surface: Option<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    buffers: VecDeque<BufferBacking>,
    width: u32,
    height: u32,
}

struct BufferBacking {
    _file: File,
    _mmap: MmapMut,
    _pool: wl_shm_pool::WlShmPool,
    buffer: wl_buffer::WlBuffer,
    released: bool,
}

impl Overlay {
    pub(crate) fn reset(&mut self) {
        if let Some(layer_surface) = self.layer_surface.take() {
            layer_surface.destroy();
        }
        if let Some(surface) = self.surface.take() {
            surface.destroy();
        }
        self.buffers.clear();
        self.width = 0;
        self.height = 0;
    }

    pub(crate) fn is_initialized(&self) -> bool {
        self.surface.is_some() && self.layer_surface.is_some()
    }

    pub(crate) fn init(
        &mut self,
        compositor: &wl_compositor::WlCompositor,
        layer_shell: &zwlr_layer_shell_v1::ZwlrLayerShellV1,
        output: Option<&wl_output::WlOutput>,
        qh: &QueueHandle<App>,
        namespace: &str,
    ) {
        let surface = compositor.create_surface(qh, ());
        let layer_surface = layer_shell.get_layer_surface(
            &surface,
            output,
            zwlr_layer_shell_v1::Layer::Overlay,
            String::from(namespace),
            qh,
            (),
        );

        layer_surface.set_anchor(
            zwlr_layer_surface_v1::Anchor::Top
                | zwlr_layer_surface_v1::Anchor::Bottom
                | zwlr_layer_surface_v1::Anchor::Left
                | zwlr_layer_surface_v1::Anchor::Right,
        );
        layer_surface.set_size(0, 0);
        layer_surface.set_exclusive_zone(-1);
        layer_surface
            .set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);

        surface.commit();

        self.surface = Some(surface);
        self.layer_surface = Some(layer_surface);
    }

    pub(crate) fn surface_size(&self) -> SurfaceSize {
        SurfaceSize {
            width: self.width.max(1),
            height: self.height.max(1),
        }
    }

    pub(crate) fn is_configured(&self) -> bool {
        self.width != 0 && self.height != 0
    }

    pub(crate) fn dimensions(&self) -> Option<(u32, u32)> {
        self.is_configured().then_some((self.width, self.height))
    }

    pub(crate) fn ack_configure(
        &mut self,
        layer_surface: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        serial: u32,
        width: u32,
        height: u32,
    ) {
        layer_surface.ack_configure(serial);
        self.width = width;
        self.height = height;
    }

    pub(crate) fn attach_buffer(
        &mut self,
        shm: &wl_shm::WlShm,
        qh: &QueueHandle<App>,
        width: u32,
        height: u32,
        render: impl FnOnce(&mut [u8], u32, u32),
    ) -> Result<()> {
        let width = width.max(1);
        let height = height.max(1);
        let stride = width
            .checked_mul(4)
            .ok_or_else(|| anyhow!("invalid buffer stride"))?;
        let size = stride
            .checked_mul(height)
            .ok_or_else(|| anyhow!("invalid buffer size"))?;

        let file = tempfile().context("create shm backing file")?;
        file.set_len(u64::from(size))
            .context("resize shm backing file")?;

        let mut mmap = unsafe { MmapMut::map_mut(&file).context("map shm backing file")? };
        render(&mut mmap, width, height);

        let surface = self
            .surface
            .as_ref()
            .ok_or_else(|| anyhow!("overlay surface is not initialized"))?;

        let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            wl_shm::Format::Argb8888,
            qh,
            (),
        );

        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, width as i32, height as i32);
        surface.commit();

        self.buffers.retain(|backing| !backing.released);
        self.buffers.push_back(BufferBacking {
            _file: file,
            _mmap: mmap,
            _pool: pool,
            buffer,
            released: false,
        });

        Ok(())
    }

    pub(crate) fn apply_input_region(
        &self,
        compositor: &wl_compositor::WlCompositor,
        qh: &QueueHandle<App>,
        policy: &CapturePolicy,
    ) -> Result<()> {
        let surface = self
            .surface
            .as_ref()
            .ok_or_else(|| anyhow!("overlay surface is not initialized"))?;
        let size = self.surface_size();

        match policy {
            CapturePolicy::Fullscreen => {
                surface.set_input_region(None);
            }
            CapturePolicy::Zones(rects) => {
                let region = compositor.create_region(qh, ());
                for rect in rects {
                    let rect = rect.to_px(size);
                    if rect.w > 0 && rect.h > 0 {
                        region.add(rect.x, rect.y, rect.w, rect.h);
                    }
                }
                surface.set_input_region(Some(&region));
                region.destroy();
            }
            CapturePolicy::None => {
                let region = compositor.create_region(qh, ());
                surface.set_input_region(Some(&region));
                region.destroy();
            }
        }

        surface.commit();
        Ok(())
    }

    pub(crate) fn mark_buffer_released(&mut self, proxy: &wl_buffer::WlBuffer) {
        for backing in &mut self.buffers {
            if backing.buffer == proxy.clone() {
                backing.released = true;
                break;
            }
        }
        self.buffers.retain(|backing| !backing.released);
    }
}
