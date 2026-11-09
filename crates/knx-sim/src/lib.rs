//! knx-sim: a KNXnet/IP tunnelling interface with simulated devices behind
//! it, so ETS and `knxcfg` can be run against the same simulation
//! (`docs/sim-plan.md`).
//!
//! - [`knxip`] — KNXnet/IP frames (from `knx-transport`)
//! - [`server`] — the tunnelling server
//! - [`bus`] — the simulated line and the frame log
//! - [`device`] — simulated devices (management server)
//! - [`profile`] — devices built from a knxprod

use anyhow::{bail, Context, Result};
use knx_transport::CemiFrame;

pub mod bus;
pub mod device;
pub mod profile;

pub use knx_transport::knxip;
pub mod server;

/// Parse an Individual Address `a.l.d`.
pub fn parse_ia(s: &str) -> Result<u16> {
    let parts: Vec<&str> = s.split('.').collect();
    let [a, l, d] = parts.as_slice() else { bail!("not an individual address: {s}") };
    let (a, l, d): (u16, u16, u16) = (a.parse()?, l.parse()?, d.parse()?);
    if a > 15 || l > 15 || d > 255 {
        bail!("individual address out of range: {s}");
    }
    Ok(a << 12 | l << 8 | d)
}

/// Send the requests in `text` to the bus, as if they came through the
/// tunnel with address `source`: lines `> <cEMI hex>` (captures, see
/// `refs/ets5/`) or `[nnn] <cEMI hex>` (`knxcfg download --frames`); other
/// lines are skipped. Returns the number of requests.
pub fn replay(bus: &mut bus::Bus, text: &str, source: u16) -> Result<usize> {
    let mut n = 0;
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        let hex = if let Some(h) = line.strip_prefix("> ") {
            h
        } else if let Some((_, h)) = line.strip_prefix('[').and_then(|l| l.split_once("] ")) {
            h
        } else {
            continue;
        };
        let bytes = hex
            .split_whitespace()
            .map(|x| u8::from_str_radix(x, 16))
            .collect::<std::result::Result<Vec<u8>, _>>()
            .with_context(|| format!("line {}: bad hex", i + 1))?;
        let mut frame = CemiFrame::decode(&bytes).with_context(|| format!("line {}", i + 1))?;
        if frame.body.len() >= 4 && frame.body[2..4] == [0, 0] {
            frame.body[2..4].copy_from_slice(&source.to_be_bytes());
        }
        bus.send(&frame, source);
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    #[test]
    fn parse_ia() {
        assert_eq!(super::parse_ia("1.1.250").unwrap(), 0x11FA);
        assert!(super::parse_ia("16.1.1").is_err());
        assert!(super::parse_ia("1.1").is_err());
    }
}
