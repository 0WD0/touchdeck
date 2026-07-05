use anyhow::{Context, Result};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    self, AtomEnum, ConnectionExt as XprotoConnectionExt, Window,
};
use x11rb::rust_connection::RustConnection;

#[derive(Clone, Copy, Debug)]
pub struct X11WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub root_w: i32,
    pub root_h: i32,
}

pub struct X11GeometryProbe {
    conn: RustConnection,
    root: Window,
    active_window_atom: xproto::Atom,
}

impl X11GeometryProbe {
    pub fn connect() -> Result<Self> {
        let (conn, screen_num) =
            RustConnection::connect(None).context("connect to X display for geometry")?;
        let root = conn.setup().roots[screen_num].root;
        let active_window_atom = conn
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")
            .context("intern _NET_ACTIVE_WINDOW")?
            .reply()
            .context("read _NET_ACTIVE_WINDOW atom")?
            .atom;

        Ok(Self {
            conn,
            root,
            active_window_atom,
        })
    }

    pub fn active_window_geometry(&self) -> Result<Option<X11WindowGeometry>> {
        let active = self
            .conn
            .get_property(
                false,
                self.root,
                self.active_window_atom,
                AtomEnum::WINDOW,
                0,
                1,
            )
            .context("query _NET_ACTIVE_WINDOW")?
            .reply()
            .context("read _NET_ACTIVE_WINDOW")?
            .value32()
            .and_then(|mut values| values.next())
            .unwrap_or(x11rb::NONE);

        if active == x11rb::NONE {
            return Ok(None);
        }

        let geometry = self
            .conn
            .get_geometry(active)
            .context("query active X11 window geometry")?
            .reply()
            .context("read active X11 window geometry")?;
        let root_geometry = self
            .conn
            .get_geometry(self.root)
            .context("query X11 root geometry")?
            .reply()
            .context("read X11 root geometry")?;
        let translated = self
            .conn
            .translate_coordinates(active, self.root, 0, 0)
            .context("translate active X11 window to root")?
            .reply()
            .context("read active X11 window root position")?;

        Ok(Some(X11WindowGeometry {
            x: i32::from(translated.dst_x),
            y: i32::from(translated.dst_y),
            w: i32::from(geometry.width),
            h: i32::from(geometry.height),
            root_w: i32::from(root_geometry.width),
            root_h: i32::from(root_geometry.height),
        }))
    }
}
