use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::mem::{size_of, MaybeUninit};
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::config::InputConfig;
use crate::geometry::SurfaceSize;

const EV_SYN: u16 = 0x00;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const DEFAULT_SUNSHINE_TOUCH_NAME: &str = "Touch passthrough";

const IOC_NRBITS: u32 = 8;
const IOC_TYPEBITS: u32 = 8;
const IOC_SIZEBITS: u32 = 14;
const IOC_NRSHIFT: u32 = 0;
const IOC_TYPESHIFT: u32 = IOC_NRSHIFT + IOC_NRBITS;
const IOC_SIZESHIFT: u32 = IOC_TYPESHIFT + IOC_TYPEBITS;
const IOC_DIRSHIFT: u32 = IOC_SIZESHIFT + IOC_SIZEBITS;
const IOC_READ: u32 = 2;
const IOC_WRITE: u32 = 1;

#[derive(Clone, Debug)]
pub(crate) enum RawTouchEvent {
    Down {
        id: i32,
        time: u32,
        x: f64,
        y: f64,
    },
    Motion {
        id: i32,
        time: u32,
        x: f64,
        y: f64,
    },
    Up {
        id: i32,
        time: u32,
    },
}

#[derive(Debug)]
pub(crate) struct EvdevTouchBackend {
    file: File,
    x_axis: AbsAxis,
    y_axis: AbsAxis,
    slots: Vec<MtSlot>,
    current_slot: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct InputEvent {
    time: libc::timeval,
    type_: u16,
    code: u16,
    value: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InputAbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[derive(Clone, Copy, Debug)]
struct AbsAxis {
    min: i32,
    max: i32,
}

#[derive(Clone, Debug, Default)]
struct MtSlot {
    active: bool,
    pending_down: bool,
    pending_up: bool,
    dirty: bool,
    x: Option<i32>,
    y: Option<i32>,
}

impl EvdevTouchBackend {
    pub(crate) fn open(config: &InputConfig) -> Result<Self> {
        let path = resolve_touch_device(config)?;
        let file = OpenOptions::new()
            .read(true)
            .open(&path)
            .with_context(|| format!("open touch device {}", path.display()))?;
        set_nonblocking(file.as_raw_fd()).context("set touch device nonblocking")?;

        let x_axis = query_abs_axis(file.as_raw_fd(), ABS_MT_POSITION_X, &path)
            .context("query ABS_MT_POSITION_X")?;
        let y_axis = query_abs_axis(file.as_raw_fd(), ABS_MT_POSITION_Y, &path)
            .context("query ABS_MT_POSITION_Y")?;
        let slot_axis =
            query_abs_axis(file.as_raw_fd(), ABS_MT_SLOT, &path).context("query ABS_MT_SLOT")?;
        let slot_count = slot_axis
            .max
            .checked_sub(slot_axis.min)
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| anyhow!("invalid ABS_MT_SLOT range"))?
            .clamp(1, 64) as usize;

        if config.evdev_grab {
            grab_device(file.as_raw_fd(), true)
                .with_context(|| format!("grab touch device {}", path.display()))?;
        }

        eprintln!(
            "touchdeck: evdev touch initialized path={} slots={} x={}..{} y={}..{} grab={}",
            path.display(),
            slot_count,
            x_axis.min,
            x_axis.max,
            y_axis.min,
            y_axis.max,
            config.evdev_grab
        );

        Ok(Self {
            file,
            x_axis,
            y_axis,
            slots: vec![MtSlot::default(); slot_count],
            current_slot: 0,
        })
    }

    pub(crate) fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    pub(crate) fn drain_events(&mut self, size: SurfaceSize) -> Result<Vec<RawTouchEvent>> {
        let mut output = Vec::new();
        let event_size = size_of::<InputEvent>();
        let mut buf = vec![0_u8; event_size * 64];

        loop {
            match self.file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    for chunk in buf[..n - (n % event_size)].chunks_exact(event_size) {
                        let event = read_input_event(chunk);
                        self.handle_event(event, size, &mut output);
                    }
                    if n < buf.len() {
                        break;
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err).context("read touch device"),
            }
        }

        Ok(output)
    }

    fn handle_event(
        &mut self,
        event: InputEvent,
        size: SurfaceSize,
        output: &mut Vec<RawTouchEvent>,
    ) {
        match (event.type_, event.code) {
            (EV_ABS, ABS_MT_SLOT) => {
                if event.value >= 0 {
                    let slot = event.value as usize;
                    if slot < self.slots.len() {
                        self.current_slot = slot;
                    }
                }
            }
            (EV_ABS, ABS_MT_TRACKING_ID) => {
                let slot = &mut self.slots[self.current_slot];
                if event.value >= 0 {
                    slot.active = true;
                    slot.pending_down = true;
                    slot.pending_up = false;
                    slot.dirty = false;
                } else if slot.active || slot.pending_down {
                    slot.pending_up = true;
                    slot.pending_down = false;
                    slot.dirty = false;
                }
            }
            (EV_ABS, ABS_MT_POSITION_X) => {
                let slot = &mut self.slots[self.current_slot];
                slot.x = Some(event.value);
                if slot.active && !slot.pending_down {
                    slot.dirty = true;
                }
            }
            (EV_ABS, ABS_MT_POSITION_Y) => {
                let slot = &mut self.slots[self.current_slot];
                slot.y = Some(event.value);
                if slot.active && !slot.pending_down {
                    slot.dirty = true;
                }
            }
            (EV_SYN, SYN_REPORT) => {
                self.flush_frame(event_time_ms(event), size, output);
            }
            _ => {}
        }
    }

    fn flush_frame(&mut self, time: u32, size: SurfaceSize, output: &mut Vec<RawTouchEvent>) {
        for (slot_id, slot) in self.slots.iter_mut().enumerate() {
            if slot.pending_up {
                output.push(RawTouchEvent::Up {
                    id: slot_id as i32,
                    time,
                });
                *slot = MtSlot::default();
                continue;
            }

            let Some(raw_x) = slot.x else {
                continue;
            };
            let Some(raw_y) = slot.y else {
                continue;
            };
            let x = self.x_axis.scale(raw_x, f64::from(size.width));
            let y = self.y_axis.scale(raw_y, f64::from(size.height));

            if slot.pending_down {
                output.push(RawTouchEvent::Down {
                    id: slot_id as i32,
                    time,
                    x,
                    y,
                });
                slot.pending_down = false;
                slot.dirty = false;
            } else if slot.active && slot.dirty {
                output.push(RawTouchEvent::Motion {
                    id: slot_id as i32,
                    time,
                    x,
                    y,
                });
                slot.dirty = false;
            }
        }
    }
}

fn resolve_touch_device(config: &InputConfig) -> Result<PathBuf> {
    if let Some(path) = &config.evdev_touch_device {
        return Ok(path.clone());
    }

    let matches = discover_touch_devices(config)?;
    match matches.as_slice() {
        [] => Err(anyhow!(
            "no matching evdev touchscreen found; set [input].touch_device or use Sunshine's native_pen_touch device named {DEFAULT_SUNSHINE_TOUCH_NAME:?}"
        )),
        [device] => Ok(device.path.clone()),
        _ => {
            let candidates = matches
                .iter()
                .map(|device| format!("{} ({})", device.path.display(), device.name))
                .collect::<Vec<_>>()
                .join(", ");
            Err(anyhow!(
                "multiple matching evdev touchscreens found: {candidates}; set [input].touch_device or [input].sunshine_output"
            ))
        }
    }
}

#[derive(Debug)]
struct CandidateDevice {
    path: PathBuf,
    name: String,
}

fn discover_touch_devices(config: &InputConfig) -> Result<Vec<CandidateDevice>> {
    let name_filter = config
        .evdev_device_name_contains
        .as_deref()
        .unwrap_or(DEFAULT_SUNSHINE_TOUCH_NAME);
    let output_tag = config
        .sunshine_output
        .as_ref()
        .map(|output| format!("[sunshine-output={output}]"));
    let mut matches = Vec::new();

    for entry in fs::read_dir("/dev/input").context("read /dev/input")? {
        let entry = entry.context("read /dev/input entry")?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        if !file_name.starts_with("event") {
            continue;
        }

        let path = entry.path();
        let Some(name) = input_device_name_from_event_path(&path) else {
            continue;
        };
        if !name.contains(name_filter) {
            continue;
        }
        if let Some(output_tag) = &output_tag {
            if !name.contains(output_tag) {
                continue;
            }
        }

        matches.push(CandidateDevice { path, name });
    }

    matches.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(matches)
}

fn input_device_name_from_event_path(path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_str()?;
    let name_path = Path::new("/sys/class/input")
        .join(file_name)
        .join("device/name");
    fs::read_to_string(name_path)
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

impl AbsAxis {
    fn scale(self, value: i32, output_max: f64) -> f64 {
        let span = (self.max - self.min).max(1) as f64;
        let normalized = f64::from(value - self.min) / span;
        normalized.clamp(0.0, 1.0) * output_max
    }
}

fn read_input_event(chunk: &[u8]) -> InputEvent {
    debug_assert_eq!(chunk.len(), size_of::<InputEvent>());
    let mut event = MaybeUninit::<InputEvent>::uninit();
    unsafe {
        std::ptr::copy_nonoverlapping(
            chunk.as_ptr(),
            event.as_mut_ptr().cast::<u8>(),
            size_of::<InputEvent>(),
        );
        event.assume_init()
    }
}

fn query_abs_axis(fd: RawFd, code: u16, path: &Path) -> Result<AbsAxis> {
    let mut info = InputAbsInfo::default();
    let rc = unsafe { libc::ioctl(fd, eviocgabs(code), &mut info) };
    if rc < 0 {
        return Err(io::Error::last_os_error()).with_context(|| {
            format!(
                "device {} does not expose abs code {code:#x}",
                path.display()
            )
        });
    }
    if info.maximum <= info.minimum {
        return Err(anyhow!(
            "device {} has invalid abs code {code:#x} range {}..{}",
            path.display(),
            info.minimum,
            info.maximum
        ));
    }
    Ok(AbsAxis {
        min: info.minimum,
        max: info.maximum,
    })
}

fn set_nonblocking(fd: RawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error()).context("fcntl F_GETFL");
    }
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if rc < 0 {
        return Err(io::Error::last_os_error()).context("fcntl F_SETFL");
    }
    Ok(())
}

fn grab_device(fd: RawFd, grab: bool) -> Result<()> {
    let value: libc::c_int = if grab { 1 } else { 0 };
    let rc = unsafe { libc::ioctl(fd, eviocgrab(), value) };
    if rc < 0 {
        return Err(io::Error::last_os_error()).context("EVIOCGRAB");
    }
    Ok(())
}

fn event_time_ms(event: InputEvent) -> u32 {
    let sec = event.time.tv_sec.max(0) as u64;
    let usec = event.time.tv_usec.max(0) as u64;
    sec.wrapping_mul(1000).wrapping_add(usec / 1000) as u32
}

fn eviocgabs(abs: u16) -> libc::c_ulong {
    ioc(
        IOC_READ,
        b'E',
        0x40 + u32::from(abs),
        size_of::<InputAbsInfo>() as u32,
    )
}

fn eviocgrab() -> libc::c_ulong {
    ioc(
        IOC_WRITE,
        b'E',
        0x90,
        size_of::<libc::c_int>() as u32,
    )
}

fn ioc(dir: u32, type_: u8, nr: u32, size: u32) -> libc::c_ulong {
    ((dir << IOC_DIRSHIFT)
        | (u32::from(type_) << IOC_TYPESHIFT)
        | (nr << IOC_NRSHIFT)
        | (size << IOC_SIZESHIFT)) as libc::c_ulong
}
