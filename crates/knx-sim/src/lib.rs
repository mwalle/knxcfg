//! knx-sim: a KNXnet/IP tunnelling interface with simulated devices behind
//! it, so ETS and `knxcfg` can be run against the same simulation
//! (`docs/sim-plan.md`).
//!
//! - [`knxip`] — KNXnet/IP frames
//! - [`server`] — the tunnelling server
//! - [`bus`] — the simulated line and the frame log
//! - [`device`] — simulated devices (management server)

use anyhow::{bail, Result};

pub mod bus;
pub mod device;
pub mod knxip;
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

#[cfg(test)]
mod tests {
    #[test]
    fn parse_ia() {
        assert_eq!(super::parse_ia("1.1.250").unwrap(), 0x11FA);
        assert!(super::parse_ia("16.1.1").is_err());
        assert!(super::parse_ia("1.1").is_err());
    }
}
