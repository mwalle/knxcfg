//! Bus access commands over a KNX USB interface (M4).
//!
//! The interface is `--interface /dev/hidrawN`, or the only `hidraw` device
//! whose report descriptor looks like a KNX USB Interface Device
//! (`knx_transport::hidraw::is_knx_descriptor`).

use anyhow::{bail, Context, Result};
use knx_core::IndividualAddress;
use knx_transport::hidraw::{self, Hidraw};
use knx_transport::mgmt::{self, Connection};
use knx_transport::usb::UsbTransport;
use std::path::{Path, PathBuf};

/// `knxcfg interfaces`: list the `hidraw` devices, KNX candidates marked.
pub fn cmd_interfaces() -> Result<()> {
    let devices = hidraw::enumerate()?;
    if devices.is_empty() {
        println!("no hidraw devices");
        return Ok(());
    }
    for d in devices {
        println!(
            "{} {:04x}:{:04x} {}{}",
            d.path.display(),
            d.vendor,
            d.product,
            d.name,
            if d.knx { "  [KNX]" } else { "" }
        );
    }
    Ok(())
}

/// `knxcfg device-info <ia>`: bus status and the device descriptor
/// (mask version).
pub fn cmd_device_info(interface: Option<&Path>, ia: &str) -> Result<()> {
    let ia = parse_ia(ia)?;
    let mut t = open(interface)?;
    let mut c = Connection::open(&mut t, ia.to_u16())?;
    let mask = mgmt::device_descriptor_read(&mut c).context("reading the device descriptor")?;
    c.close()?;
    println!("{ia}: mask version {mask:04X}h");
    Ok(())
}

/// `knxcfg mem-read <ia> <address> <count>`: hex dump of device memory.
pub fn cmd_mem_read(interface: Option<&Path>, ia: &str, address: &str, count: usize) -> Result<()> {
    let ia = parse_ia(ia)?;
    let address = parse_u16(address)?;
    let mut t = open(interface)?;
    let mut c = Connection::open(&mut t, ia.to_u16())?;
    let data = mgmt::memory_read(&mut c, address, count)?;
    c.close()?;
    for (i, line) in data.chunks(16).enumerate() {
        let hex: Vec<String> = line.iter().map(|b| format!("{b:02x}")).collect();
        println!("{:04x}: {}", address as usize + i * 16, hex.join(" "));
    }
    Ok(())
}

fn open(interface: Option<&Path>) -> Result<UsbTransport<Hidraw>> {
    let path = match interface {
        Some(p) => p.to_path_buf(),
        None => find_interface()?,
    };
    let t = UsbTransport::new(Hidraw::open(&path)?).with_context(|| format!("{}", path.display()))?;
    if t.bus_connected() == Some(false) {
        bail!("{}: the USB interface has no bus connection", path.display());
    }
    Ok(t)
}

fn find_interface() -> Result<PathBuf> {
    let knx: Vec<PathBuf> = hidraw::enumerate()?.into_iter().filter(|d| d.knx).map(|d| d.path).collect();
    match knx.as_slice() {
        [one] => Ok(one.clone()),
        [] => bail!("no KNX USB interface found (see `knxcfg interfaces`; use --interface /dev/hidrawN)"),
        many => bail!(
            "several KNX USB interfaces found ({}); choose one with --interface",
            many.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
        ),
    }
}

fn parse_ia(s: &str) -> Result<IndividualAddress> {
    s.parse().with_context(|| format!("invalid IA {s}"))
}

/// `0x1234`, `1234h` (hex) or decimal.
fn parse_u16(s: &str) -> Result<u16> {
    let r = if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u16::from_str_radix(h, 16)
    } else if let Some(h) = s.strip_suffix('h').or_else(|| s.strip_suffix('H')) {
        u16::from_str_radix(h, 16)
    } else {
        s.parse()
    };
    r.with_context(|| format!("invalid address {s}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_notations() {
        assert_eq!(parse_u16("0x0100").unwrap(), 0x100);
        assert_eq!(parse_u16("4000h").unwrap(), 0x4000);
        assert_eq!(parse_u16("256").unwrap(), 256);
        assert!(parse_u16("0x10000").is_err());
    }
}
