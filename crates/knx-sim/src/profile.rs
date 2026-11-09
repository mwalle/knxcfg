//! Simulated devices built from a knxprod: identity from the application
//! program, loadable parts and memory mapped load states from the mask in
//! `knx_master.xml`.

use crate::device::{pid, Device};
use anyhow::{anyhow, bail, Context, Result};
use knx_prod::{KnxProd, LoadStep};
use std::path::Path;

/// The loadable parts of a mask with property mapped load state machines
/// (System 7, mask 0705h): object index, object type and the resource
/// holding the memory mapped load status.
const LOADABLE_PARTS: [(usize, u16, &str); 4] = [
    (1, 1, "GroupAddressTableLoadStatus"),
    (2, 2, "GroupAssociationTableLoadStatus"),
    (3, 3, "ApplicationLoadStatus"),
    (4, 4, "PeiprogLoadStatus"),
];

/// A device for the product `h2p` (a `Hardware2ProgramRefId`) of `knxprod`
/// at `address`. With `h2p` empty the knxprod must hold one application.
pub fn device(knxprod: &Path, h2p: &str, address: u16) -> Result<Device> {
    let kp = KnxProd::open(knxprod)?;
    let h2p = if h2p.is_empty() {
        let mut refs: Vec<&str> = kp.products.iter().filter_map(|p| p.hardware2program_ref_id.as_deref()).collect();
        refs.sort();
        refs.dedup();
        match refs.as_slice() {
            [one] => one.to_string(),
            _ => bail!("{} holds several applications, name one of: {}", knxprod.display(), refs.join(", ")),
        }
    } else {
        h2p.to_string()
    };
    let ap = kp.application_program(&h2p)?;
    let master = kp.master_data()?;
    let mask = master
        .mask_version(&ap.mask_version)
        .ok_or_else(|| anyhow!("mask {} not in knx_master.xml", ap.mask_version))?;
    let mask_number = ap
        .mask_version
        .strip_prefix("MV-")
        .and_then(|m| u16::from_str_radix(m, 16).ok())
        .ok_or_else(|| anyhow!("bad mask version {:?}", ap.mask_version))?;
    if !mask.has_resource("GroupAddressTableLoadStatus") {
        bail!("mask {} is not simulated yet (only System 7 masks like 0705h)", ap.mask_version);
    }
    let app_id = knx_prod::device_image::application_id(&ap)?;
    let manufacturer = u16::from_be_bytes([app_id[0], app_id[1]]);
    // Serial number: manufacturer, a fixed marker and the address.
    let [a0, a1] = address.to_be_bytes();
    let mut dev = Device::new(address, mask_number, manufacturer, [app_id[0], app_id[1], 0x51, 0x4D, a0, a1]);

    // The hardware type the load procedure checks (`LdCtrlCompareProp` on
    // PID_HARDWARE_TYPE). The BE-02001.02's procedure has 10 octets of
    // inline data; the device answers the first six (ETS5.log, 2026-11-03).
    let steps = ap.static_.iter().flat_map(|s| s.load_procedures.iter()).flat_map(|l| l.items.iter()).flat_map(|p| p.steps.iter());
    for step in steps {
        if let LoadStep::CompareProp(c) = step {
            if c.obj_idx.trim() == "0" && c.prop_id.trim() == pid::HARDWARE_TYPE.to_string() {
                let data = parse_hex(&c.inline_data).with_context(|| format!("InlineData {:?}", c.inline_data))?;
                let mut hw = [0u8; 6];
                for (h, d) in hw.iter_mut().zip(&data) {
                    *h = *d;
                }
                dev.set_property(0, pid::HARDWARE_TYPE, &hw);
            }
        }
    }

    for (index, kind, status) in LOADABLE_PARTS {
        let status_address = match mask.resource_location(status) {
            Some(("StandardMemory", a)) => Some(a as u16),
            _ => None,
        };
        dev.add_loadable(index, kind, status_address);
    }
    Ok(dev)
}

fn parse_hex(s: &str) -> Result<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        bail!("odd number of hex digits");
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(Into::into)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    pub(crate) fn be_02001() -> Option<PathBuf> {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../refs/knxprod/MDT_KP_BE_02_Universal_Interface_V32a.knxprod");
        p.is_file().then_some(p)
    }

    #[test]
    fn be_02001_profile() {
        let Some(path) = be_02001() else { return };
        let d = device(&path, "M-0083_H-182-0_HP-00B6-32-9642", 0x1103).unwrap();
        assert_eq!(d.mask, 0x0705);
        let dump = d.dump();
        assert!(dump.contains("pid 78: 00 00 00 00 02 AC"), "{dump}");
        assert!(dump.contains("object 3 type 3\n  load state 0 Unloaded\n  run state halted"), "{dump}");
        // Load status of the application mirrored at B6ECh.
        assert_eq!(d.memory[0xB6EC], 0);
        assert!(device(&path, "", 0x1103).unwrap_err().to_string().contains("several applications"));
    }
}
