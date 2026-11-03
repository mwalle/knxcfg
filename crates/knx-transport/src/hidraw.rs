//! Linux `hidraw` access for KNX USB Interface Devices.
//!
//! The kernel's generic HID driver exposes every HID interface as
//! `/dev/hidrawN`; a `write` sends one output report and a `read` returns one
//! input report, both starting with the report ID. Device metadata comes from
//! sysfs (`/sys/class/hidraw/hidrawN/device/{uevent,report_descriptor}`).
//!
//! There is no common USB vendor ID for KNX interfaces (`09_03 §3.5.2.2`), so
//! KNX devices are recognised by their report descriptor: report ID 01h with
//! 63-octet input and output reports in a vendor-defined usage page
//! (`09_03 §3.4.1`). Nothing is sent to a device before it matches.

use super::usb::{HidDevice, REPORT_LEN};
use anyhow::{bail, Context, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

const SYSFS_HIDRAW: &str = "/sys/class/hidraw";

/// A `hidraw` device as seen in sysfs.
#[derive(Debug, Clone)]
pub struct HidrawInfo {
    /// `/dev/hidrawN`
    pub path: PathBuf,
    pub vendor: u16,
    pub product: u16,
    pub name: String,
    /// The report descriptor matches a KNX USB Interface Device.
    pub knx: bool,
}

/// List all `hidraw` devices, sorted by device number.
pub fn enumerate() -> Result<Vec<HidrawInfo>> {
    let entries = match fs::read_dir(SYSFS_HIDRAW) {
        Ok(e) => e,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).context(SYSFS_HIDRAW),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        let node = entry.file_name().to_string_lossy().into_owned();
        let dev = entry.path().join("device");
        let uevent = fs::read_to_string(dev.join("uevent")).unwrap_or_default();
        let (mut vendor, mut product, mut name) = (0, 0, String::new());
        for line in uevent.lines() {
            if let Some(id) = line.strip_prefix("HID_ID=") {
                // bus:vendor:product, each in hex (`0003:0000046D:0000C52B`).
                let f: Vec<&str> = id.split(':').collect();
                if f.len() == 3 {
                    vendor = u32::from_str_radix(f[1], 16).unwrap_or(0) as u16;
                    product = u32::from_str_radix(f[2], 16).unwrap_or(0) as u16;
                }
            } else if let Some(n) = line.strip_prefix("HID_NAME=") {
                name = n.to_string();
            }
        }
        let knx = fs::read(dev.join("report_descriptor")).is_ok_and(|d| is_knx_descriptor(&d));
        out.push(HidrawInfo { path: Path::new("/dev").join(&node), vendor, product, name, knx });
    }
    out.sort_by_key(|i| {
        let s = i.path.to_string_lossy();
        s.trim_start_matches("/dev/hidraw").parse::<u32>().unwrap_or(u32::MAX)
    });
    Ok(out)
}

/// Does the HID report descriptor declare the KNX data reports: report ID
/// 01h with 63 octets (8-bit × 63) of both input and output in a
/// vendor-defined usage page (FF00h..FFFFh)?
///
/// Only the global items that matter are tracked (`Usage Page`,
/// `Report Size`, `Report Count`, `Report ID`, `Push`/`Pop`), per the
/// short-item encoding of the USB HID 1.11 specification §6.2.2.
pub fn is_knx_descriptor(desc: &[u8]) -> bool {
    #[derive(Clone, Copy, Default)]
    struct Globals {
        page: u32,
        size: u32,
        count: u32,
        id: u32,
    }
    let mut g = Globals::default();
    let mut stack = Vec::new();
    let (mut input, mut output) = (false, false);
    let mut i = 0;
    while i < desc.len() {
        let prefix = desc[i];
        if prefix == 0xFE {
            // Long item: skip it.
            let len = desc.get(i + 1).copied().unwrap_or(0) as usize;
            i += 3 + len;
            continue;
        }
        let len = match prefix & 0x03 {
            3 => 4,
            n => n as usize,
        };
        if i + 1 + len > desc.len() {
            break;
        }
        let value = desc[i + 1..i + 1 + len].iter().rev().fold(0u32, |v, &b| (v << 8) | b as u32);
        match prefix & 0xFC {
            0x04 => g.page = value,
            0x74 => g.size = value,
            0x94 => g.count = value,
            0x84 => g.id = value,
            0xA4 => stack.push(g),
            0xB4 => g = stack.pop().unwrap_or_default(),
            tag @ (0x80 | 0x90) => {
                let knx = g.page >= 0xFF00 && g.id == 1 && g.size * g.count == 8 * (REPORT_LEN as u32 - 1);
                if knx {
                    if tag == 0x80 {
                        input = true;
                    } else {
                        output = true;
                    }
                }
            }
            _ => {}
        }
        i += 1 + len;
    }
    input && output
}

/// An opened `/dev/hidrawN`.
pub struct Hidraw {
    file: File,
}

impl Hidraw {
    pub fn open(path: &Path) -> Result<Self> {
        match OpenOptions::new().read(true).write(true).open(path) {
            Ok(file) => Ok(Self { file }),
            Err(e) if e.kind() == ErrorKind::PermissionDenied => bail!(
                "{}: permission denied; grant access with a udev rule, e.g. \
                 SUBSYSTEM==\"hidraw\", ATTRS{{idVendor}}==\"<vid>\", TAG+=\"uaccess\"",
                path.display()
            ),
            Err(e) => Err(e).with_context(|| path.display().to_string()),
        }
    }
}

impl HidDevice for Hidraw {
    fn write_report(&mut self, report: &[u8]) -> Result<()> {
        let n = self.file.write(report).context("writing HID report")?;
        if n != report.len() {
            bail!("short HID report write: {n} of {} octets", report.len());
        }
        Ok(())
    }

    fn read_report(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>> {
        let mut pfd = libc::pollfd { fd: self.file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        let ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
        // SAFETY: `pfd` is a valid pollfd for the duration of the call.
        let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                return Ok(None);
            }
            return Err(err).context("poll on HID device");
        }
        if rc == 0 {
            return Ok(None);
        }
        if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            bail!("the USB interface was disconnected");
        }
        let mut buf = [0u8; REPORT_LEN];
        let n = self.file.read(&mut buf).context("reading HID report")?;
        Ok(Some(buf[..n].to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Report descriptor in the layout KNX USB interfaces use: vendor usage
    /// page, report ID 1, 63 × 8 bit input and output (synthetic example).
    const KNX_DESC: &[u8] = &[
        0x06, 0xA0, 0xFF, // Usage Page (FFA0h)
        0x09, 0x01, // Usage (1)
        0xA1, 0x01, // Collection (Application)
        0x09, 0x01, //   Usage (1)
        0x15, 0x00, 0x26, 0xFF, 0x00, // Logical 0..255
        0x75, 0x08, 0x95, 0x3F, // Report Size 8, Count 63
        0x85, 0x01, // Report ID 1
        0x81, 0x02, //   Input
        0x09, 0x01, 0x91, 0x02, // Usage, Output
        0xC0, // End Collection
    ];

    /// `hidraw0` of a Logitech USB receiver: vendor page FF00h, but report
    /// IDs 10h/11h/20h/21h (HID++).
    const LOGITECH_DESC: &str = "0600ff0901a101851075089506150026ff000901810009019100c00600ff0902a1018511\
        75089513150026ff000902810009029100c00600ff0904a10185207508950e150026ff000941810009419100\
        8521951f150026ff000942810009429100c0";

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn knx_descriptor_matches() {
        assert!(is_knx_descriptor(KNX_DESC));
    }

    #[test]
    fn other_descriptors_do_not_match() {
        assert!(!is_knx_descriptor(&hex(LOGITECH_DESC)));
        // Same layout, but only an input report.
        let input_only = &KNX_DESC[..KNX_DESC.len() - 5];
        assert!(!is_knx_descriptor(input_only));
        // Same layout on a standard usage page (Generic Desktop).
        let mut desktop = KNX_DESC.to_vec();
        desktop[..3].copy_from_slice(&[0x05, 0x01, 0x00]);
        assert!(!is_knx_descriptor(&desktop));
    }
}
