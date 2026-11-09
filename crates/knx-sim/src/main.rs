//! `knx-sim`: run the KNXnet/IP tunnelling server with simulated devices.

use anyhow::{bail, Context, Result};
use clap::Parser;
use knx_sim::bus::{Bus, Log};
use knx_sim::device::Device;
use knx_sim::knxip::Description;
use knx_sim::parse_ia;
use knx_sim::server::{Input, Server};
use std::io::BufRead;
use std::net::Ipv4Addr;
use std::sync::mpsc;

#[derive(Parser)]
#[command(about = "KNXnet/IP tunnelling interface with simulated KNX devices")]
struct Args {
    /// Local IPv4 address to serve on (UDP 3671, multicast search on its interface).
    #[arg(long, required_unless_present = "replay")]
    listen: Option<Ipv4Addr>,
    /// Individual address of the interface.
    #[arg(long, default_value = "1.1.250")]
    address: String,
    /// Individual address of the tunnelling connection.
    #[arg(long, default_value = "1.1.251")]
    tunnel: String,
    /// Simulated device: <ia>:<mask>[:<manufacturer>], mask and manufacturer in hex
    /// (e.g. 1.1.1:0705:0083).
    #[arg(long = "device", value_name = "SPEC")]
    devices: Vec<String>,
    /// Simulated device built from a knxprod: <ia>:<knxprod>[:<Hardware2ProgramRefId>].
    #[arg(long = "product", value_name = "SPEC")]
    products: Vec<String>,
    /// Do not serve: send the requests in this file to the devices (lines
    /// `> <cEMI hex>` or `[nnn] <cEMI hex>` as printed by `knxcfg download
    /// --frames`), then print the device dumps.
    #[arg(long, value_name = "FILE")]
    replay: Option<std::path::PathBuf>,
    /// Also write the frame log to this file.
    #[arg(long)]
    log: Option<std::path::PathBuf>,
}

fn device(spec: &str) -> Result<Device> {
    let parts: Vec<&str> = spec.split(':').collect();
    let (addr, mask, manufacturer) = match parts.as_slice() {
        [a, m] => (*a, *m, "0000"),
        [a, m, f] => (*a, *m, *f),
        _ => bail!("device spec is <ia>:<mask>[:<manufacturer>]: {spec}"),
    };
    let addr = parse_ia(addr)?;
    let mask = u16::from_str_radix(mask.trim_end_matches('h'), 16).with_context(|| format!("mask {mask}"))?;
    let manufacturer =
        u16::from_str_radix(manufacturer.trim_end_matches('h'), 16).with_context(|| format!("manufacturer {manufacturer}"))?;
    // Serial number: manufacturer, then a fixed marker and the address.
    let [m0, m1] = manufacturer.to_be_bytes();
    let [a0, a1] = addr.to_be_bytes();
    Ok(Device::new(addr, mask, manufacturer, [m0, m1, 0x51, 0x4D, a0, a1]))
}

fn product(spec: &str) -> Result<Device> {
    let (addr, rest) = spec.split_once(':').with_context(|| format!("product spec is <ia>:<knxprod>[:<h2p>]: {spec}"))?;
    // The application reference starts with the manufacturer (M-xxxx_H-…).
    let (path, h2p) = match rest.rsplit_once(':') {
        Some((p, h)) if h.starts_with("M-") => (p, h),
        _ => (rest, ""),
    };
    knx_sim::profile::device(std::path::Path::new(path), h2p, parse_ia(addr)?)
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut devices = args.devices.iter().map(|s| device(s)).collect::<Result<Vec<_>>>()?;
    devices.extend(args.products.iter().map(|s| product(s)).collect::<Result<Vec<_>>>()?);
    let file = match &args.log {
        Some(p) => Some(std::fs::File::create(p).with_context(|| format!("create {}", p.display()))?),
        None => None,
    };
    if let Some(path) = &args.replay {
        let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let mut bus = Bus::new(devices, Log::to_file(file));
        knx_sim::replay(&mut bus, &text, parse_ia(&args.tunnel)?)?;
        for d in &bus.devices {
            print!("{}", d.dump());
        }
        return Ok(());
    }
    let address = parse_ia(&args.address)?;
    let desc = Description {
        individual_address: address,
        serial: [0x00, 0x00, 0x51, 0x4D, (address >> 8) as u8, address as u8],
        mac: [0x02, 0x00, 0x51, 0x4D, 0x00, 0x01],
        name: "knx-sim".into(),
    };
    let bus = Bus::new(devices, Log::new(file));
    let ip = args.listen.expect("required without --replay");
    let listen = std::net::SocketAddrV4::new(ip, knx_sim::knxip::PORT);
    let mut server = Server::new(listen, desc, parse_ia(&args.tunnel)?, bus)?;

    let (tx, rx) = mpsc::channel();
    Server::spawn_multicast(ip, tx.clone())?;
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if tx.send(Input::Command(line)).is_err() {
                break;
            }
        }
        // Without stdin (e.g. in the background) the server keeps running.
    });
    server.run(rx)
}
