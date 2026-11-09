//! Bus access commands (M4).
//!
//! The interface is `--interface /dev/hidrawN`, a KNXnet/IP tunnelling
//! server (`--interface 192.168.77.1` or `192.168.77.1:3671`), or the only
//! `hidraw` device whose report descriptor looks like a KNX USB Interface
//! Device (`knx_transport::hidraw::is_knx_descriptor`).

use anyhow::{bail, Context, Result};
use knx_core::IndividualAddress;
use knx_prod::tables::Got7Entry;
use knx_transport::go_diag::{self, GoConfig};
use knx_transport::hidraw::{self, Hidraw};
use knx_transport::ip::IpTransport;
use knx_transport::mgmt::{self, Connection};
use knx_transport::usb::UsbTransport;
use knx_transport::Transport;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    let mut c = Connection::open(&mut *t, ia.to_u16())?;
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
    let mut c = Connection::open(&mut *t, ia.to_u16())?;
    let data = mgmt::memory_read(&mut c, address, count)?;
    c.close()?;
    for (i, line) in data.chunks(16).enumerate() {
        let hex: Vec<String> = line.iter().map(|b| format!("{b:02x}")).collect();
        println!("{:04x}: {}", address as usize + i * 16, hex.join(" "));
    }
    Ok(())
}

/// `knxcfg go-read <ia> <object>`: value, status and config of a group
/// object through Group Object Diagnostics (`03_05_01 §4.8.1`). The Group
/// Object Table object is found by its `PID_OBJECT_TYPE`.
pub fn cmd_go_read(interface: Option<&Path>, ia: &str, object: u16) -> Result<()> {
    let ia = parse_ia(ia)?;
    let mut t = open(interface)?;
    let mut c = Connection::open(&mut *t, ia.to_u16())?;
    let idx = mgmt::find_object_index(&mut c, go_diag::OT_GROUP_OBJECT_TABLE)
        .context("looking for the Group Object Table object")?
        .ok_or_else(|| anyhow::anyhow!("{ia} has no Group Object Table interface object (System B and later only)"))?;
    let value = go_diag::get_value(&mut c, idx, object).with_context(|| format!("{ia}: Get local GO value"))?;
    let config = go_diag::get_config(&mut c, idx, object);
    c.close()?;

    let bits = config.as_ref().ok().and_then(|cfg| go_diag::value_field_size(cfg.value_field_type).1);
    println!("{ia} group object {object}: {}", format_value(&value.value, bits));
    println!("  status: {}", format_status(value.status));
    match config {
        Ok(cfg) => {
            let (octets, bits) = go_diag::value_field_size(cfg.value_field_type);
            let size = match bits {
                Some(b) => format!("{b} bit"),
                None => format!("{octets} octet{}", if octets == 1 { "" } else { "s" }),
            };
            let dpt = match cfg.dpt {
                Some((main, sub)) => format!("DPT {main}.{sub:03}"),
                None => "DPT unknown".to_string(),
            };
            println!("  config: {}  {size}, {dpt}", format_config(&cfg));
        }
        Err(e) => println!("  config: {e:#}"),
    }
    Ok(())
}

/// What the knxprod says about a group object, for `go-mem-read`.
pub struct ObjectInfo {
    pub label: String,
    pub datapoint_type: String,
    pub application: String,
}

/// `knxcfg go-mem-read`: read a System 7 group object's value and its
/// RAM-flags octet at the addresses of its GoT descriptor.
pub fn go_mem_read(interface: Option<&Path>, ia: IndividualAddress, object: u16, e: &Got7Entry, info: &ObjectInfo) -> Result<()> {
    let (octets, bits) = go_diag::value_field_size(e.value_type);
    let mut t = open(interface)?;
    let mut c = Connection::open(&mut *t, ia.to_u16())?;
    let value = mgmt::memory_read(&mut c, e.data_pointer, octets)
        .with_context(|| format!("reading the value at {:04X}h", e.data_pointer))?;
    let flags = mgmt::memory_read(&mut c, e.ram_flags, 1)
        .with_context(|| format!("reading the RAM flags at {:04X}h", e.ram_flags))?[0];
    c.close()?;

    let nibble = if e.ram_flags_high { flags >> 4 } else { flags & 0x0F };
    let size = match bits {
        Some(b) => format!("{b} bit"),
        None => format!("{octets} octet{}", if octets == 1 { "" } else { "s" }),
    };
    let label = if info.label.is_empty() { String::new() } else { format!(" ({})", info.label) };
    println!("{ia} group object {object}{label}: {}", format_value(&value, bits));
    println!("  status: {}", format_status(nibble));
    println!(
        "  memory: value at {:04X}h ({size}{}), flags at {:04X}h {} nibble",
        e.data_pointer,
        if info.datapoint_type.is_empty() { String::new() } else { format!(", {}", info.datapoint_type) },
        e.ram_flags,
        if e.ram_flags_high { "high" } else { "low" },
    );
    println!("  addresses from {}: valid if the device runs this application", info.application);
    Ok(())
}

/// A value as hex octets; values shorter than one octet also in decimal,
/// with the unused leading bits masked.
fn format_value(value: &[u8], bits: Option<u8>) -> String {
    let hex: Vec<String> = value.iter().map(|b| format!("{b:02x}")).collect();
    match (bits, value) {
        (Some(b), [v]) => format!("{} ({})", v & ((1u16 << b) - 1) as u8, hex[0]),
        _ => hex.join(" "),
    }
}

/// The communication flags of a group object (`03_05_01 §4.18.3.1.1.1`,
/// GO Status of `§4.8.1.1.6`): update (3), data request (2), transmission
/// status (1..0).
fn format_status(flags: u8) -> String {
    let mut out = vec![if flags & 0x08 != 0 { "updated" } else { "not updated" }];
    if flags & 0x04 != 0 {
        out.push("data request");
    }
    out.push(match flags & 0x03 {
        0 => "idle/OK",
        1 => "idle/error",
        2 => "transmitting",
        _ => "transmit request",
    });
    out.join(", ")
}

/// The flags of a GO Config: `C R W T U I` (`-` when off), priority, link
/// and security.
fn format_config(cfg: &GoConfig) -> String {
    let flag = |f: u16, c: char| if cfg.has(f) { c } else { '-' };
    let flags: String = [
        flag(GoConfig::COMMUNICATION, 'C'),
        flag(GoConfig::READ, 'R'),
        flag(GoConfig::WRITE, 'W'),
        flag(GoConfig::TRANSMIT, 'T'),
        flag(GoConfig::UPDATE, 'U'),
        flag(GoConfig::READ_ON_INIT, 'I'),
    ]
    .iter()
    .collect();
    let prio = ["system", "normal", "urgent", "low"][cfg.priority() as usize];
    let mut out = format!("{flags}, priority {prio}, {}", if cfg.has(GoConfig::LINKED) { "linked" } else { "not linked" });
    if cfg.has(GoConfig::AUTH) || cfg.has(GoConfig::CONF) {
        out.push_str(match (cfg.has(GoConfig::AUTH), cfg.has(GoConfig::CONF)) {
            (true, true) => ", secure (auth+conf)",
            (true, false) => ", secure (auth)",
            _ => ", secure (conf)",
        });
    }
    out
}

/// `knxcfg prog-ia <ia> [--serial <sn>]`: assign an Individual Address to
/// the device in programming mode (`NM_IndividualAddress_Write`) or to the
/// device with a KNX Serial Number (`NM_IndividualAddress_SerialNumber_Write`).
pub fn cmd_prog_ia(interface: Option<&Path>, ia: &str, serial: Option<&str>, wait: Duration) -> Result<()> {
    let ia = parse_ia(ia)?;
    if let Some(why) = ia.reserved_reason() {
        bail!("{ia}: {why}");
    }
    let serial = serial.map(parse_serial).transpose()?;
    let mut t = open(interface)?;
    let r = match &serial {
        Some(sn) => mgmt::individual_address_serial_write(&mut *t, sn, ia.to_u16())?,
        None => mgmt::individual_address_write(&mut *t, ia.to_u16(), wait, &mut |m| eprintln!("{m}"))?,
    };
    let previous = format_ia(r.previous);
    match (&serial, r.mask) {
        _ if r.previous == ia.to_u16() => println!("{ia}: already assigned"),
        (Some(sn), _) => println!("{ia}: assigned to {} (was {previous})", mgmt::serial_string(sn)),
        (None, Some(mask)) => println!("{ia}: assigned (was {previous}), mask version {mask:04X}h, restarted"),
        (None, None) => println!("{ia}: assigned (was {previous})"),
    }
    Ok(())
}

/// Open the bus interface: a KNXnet/IP tunnelling server if `interface` is an
/// IPv4 address (port 3671 unless given), else a KNX USB interface.
pub fn open(interface: Option<&Path>) -> Result<Box<dyn Transport>> {
    if let Some(s) = interface.and_then(Path::to_str) {
        let server = s
            .parse::<SocketAddrV4>()
            .ok()
            .or_else(|| s.parse::<Ipv4Addr>().ok().map(|ip| SocketAddrV4::new(ip, 3671)));
        if let Some(server) = server {
            let t = IpTransport::connect(server).with_context(|| format!("KNXnet/IP {server}"))?;
            return Ok(Box::new(t));
        }
    }
    Ok(Box::new(open_usb(interface)?))
}

fn open_usb(interface: Option<&Path>) -> Result<UsbTransport<Hidraw>> {
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

fn format_ia(a: u16) -> String {
    format!("{}.{}.{}", a >> 12, (a >> 8) & 0x0F, a & 0xFF)
}

/// KNX Serial Number: 12 hex digits, separators `:`, `-`, `.` or spaces
/// allowed (`0083:12345678`, as printed on the device label).
fn parse_serial(s: &str) -> Result<[u8; 6]> {
    let hex: String = s.chars().filter(|c| !matches!(c, ':' | '-' | '.' | ' ')).collect();
    if hex.len() != 12 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("invalid serial number {s} (expected 12 hex digits, e.g. 0083:12345678)");
    }
    let mut sn = [0u8; 6];
    for (i, b) in sn.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).expect("hex digits");
    }
    Ok(sn)
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

    #[test]
    fn value_and_flag_formats() {
        assert_eq!(format_value(&[0x01], Some(1)), "1 (01)");
        assert_eq!(format_value(&[0x8B], Some(4)), "11 (8b)", "unused bits masked");
        assert_eq!(format_value(&[0x0C, 0x1A], None), "0c 1a");
        assert_eq!(format_status(0x08), "updated, idle/OK");
        assert_eq!(format_status(0x07), "not updated, data request, transmit request");
        let cfg = GoConfig { config: 0x04DD, value_field_type: 0, dpt: None };
        assert_eq!(format_config(&cfg), "CRWTU-, priority normal, linked");
        let cfg = GoConfig { config: 0x0300 | 0x2B, value_field_type: 0, dpt: None };
        assert_eq!(format_config(&cfg), "-R---I, priority low, not linked, secure (auth+conf)");
    }

    #[test]
    fn serial_notations() {
        let sn = [0x00, 0x83, 0x12, 0x34, 0xAB, 0xCD];
        assert_eq!(parse_serial("0083:1234ABCD").unwrap(), sn);
        assert_eq!(parse_serial("00831234abcd").unwrap(), sn);
        assert_eq!(parse_serial("00-83-12-34-AB-CD").unwrap(), sn);
        assert!(parse_serial("0083:1234").is_err());
        assert!(parse_serial("0083:1234ABCG").is_err());
    }
}
