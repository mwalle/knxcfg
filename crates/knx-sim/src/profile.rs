//! Simulated devices built from a knxprod: identity from the application
//! program, loadable parts (interface objects), memory mapped load states
//! and the memory for relative segments from the mask in `knx_master.xml`.

use crate::device::{pdt, pid, Device, Property};
use anyhow::{anyhow, bail, Result};
use knx_prod::KnxProd;
use std::path::Path;

/// The resource holding the memory mapped load status of a loadable part,
/// by object type (System 7, mask 0705h).
const LOAD_STATUS: [(u16, &str); 4] = [
    (1, "GroupAddressTableLoadStatus"),
    (2, "GroupAssociationTableLoadStatus"),
    (3, "ApplicationLoadStatus"),
    (4, "PeiprogLoadStatus"),
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
    let objects = mask.interface_objects();
    let filter = match mask.resource_location("GroupFilterTable") {
        Some(("LcFilterMemory", start)) => Some((start as u16, mask.resource_length("GroupFilterTable").unwrap_or(0) as usize)),
        _ => None,
    };
    if filter.is_none() && objects.iter().all(|o| o.index == 0) {
        bail!("mask {} lists no loadable interface objects", ap.mask_version);
    }
    let app_id = knx_prod::device_image::application_id(&ap)?;
    let manufacturer = u16::from_be_bytes([app_id[0], app_id[1]]);
    // Serial number: manufacturer, a fixed marker and the address.
    let [a0, a1] = address.to_be_bytes();
    let mut dev = Device::new(address, mask_number, manufacturer, [app_id[0], app_id[1], 0x51, 0x4D, a0, a1]);

    // The hardware type the download checks (`CompareProp` on
    // PID_HARDWARE_TYPE in knx-prod's plan for the default configuration:
    // from `LdCtrlCompareProp` or a hardware property parameter). The
    // BE-02001.02's procedure has 10 octets of inline data for the 6-octet
    // property; the first six are taken, as in knx-core's emulation of the
    // device. What MDT hardware answers is not known.
    let values = std::collections::HashMap::new();
    let ev = knx_prod::Evaluation::new(&ap, &values);
    let format = kp.table_format(&ap)?;
    let image = if format == knx_prod::tables::TableFormat::Coupler {
        knx_prod::build_coupler_image(&ev, &Default::default())?
    } else {
        knx_prod::device_image::build_device_image_eval(&ev, format, &[])?
    };
    let merged = ap.load_procedure_style == "MergedProcedure";
    let plan = knx_prod::download_plan(&ev, &image, merged.then_some(&master), knx_prod::DownloadKind::Full)?;
    for step in &plan {
        if let knx_prod::DownloadStep::CompareProp { obj_idx: 0, prop_id: pid::HARDWARE_TYPE, inline_data } = step {
            let mut hw = [0u8; 6];
            for (h, d) in hw.iter_mut().zip(inline_data) {
                *h = *d;
            }
            dev.set_property(0, pid::HARDWARE_TYPE, &hw);
        }
    }

    // A coupler (0912h/091Ah): the mask names the router object by type
    // only; the simulation puts it at index 1.
    if let Some((start, len)) = filter {
        dev.add_router(1, start, len);
    }
    let system7 = mask.has_resource("GroupAddressTableLoadStatus");
    for o in objects.iter().filter(|o| o.index > 0) {
        let status_address = LOAD_STATUS
            .iter()
            .find(|(t, _)| *t == o.object_type)
            .and_then(|(_, r)| match mask.resource_location(r) {
                Some(("StandardMemory", a)) => Some(a as u16),
                _ => None,
            });
        let table_reference = if o.properties.iter().any(|(id, pdt)| *id == pid::TABLE_REFERENCE && pdt == "PDT_UNSIGNED_LONG") {
            Some(4)
        } else if system7 && matches!(o.object_type, 1 | 2) {
            // `GroupAssociationTablePtr`, 2 octets.
            Some(2)
        } else {
            None
        };
        dev.add_loadable(o.index as usize, o.object_type, status_address, table_reference);
        if o.object_type == 2 && table_reference == Some(4) {
            // ETS 5.7 reads the description of the association table's
            // PID_TABLE before a System B download, to learn the entry format
            // (`03_05_01 §4.3.23.3`: PDT_GENERIC_02 or _04). The simulation
            // answers PDT_GENERIC_04 ([TSAP:2][ASAP:2], as knx-prod writes);
            // what MDT's devices answer is not known. The maximum number of
            // entries (255) is chosen, not taken from a device.
            dev.objects[o.index as usize].properties.push(Property {
                id: pid::TABLE,
                pdt: pdt::GENERIC_04,
                writable: true,
                size: 4,
                max_elements: 255,
                elements: Vec::new(),
            });
        }
    }
    dev.allocation = mask
        .memory_segments()
        .iter()
        .find(|m| m.address_space == "StandardMemory" && !m.optional)
        .map(|m| (m.start, m.start + m.length));
    Ok(dev)
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

    /// System B (mask 07B0h): five loadable parts, the group object table as
    /// object 3, 4-octet table references, relative segments in the first
    /// EEPROM segment.
    #[test]
    fn aku_02up_profile() {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../refs/knxprod/MDT_KP_AKU_03_Universal_Actuator_V41a.knxprod");
        if !p.is_file() {
            return;
        }
        let d = device(&p, "M-0083_H-287-1_HP-00C5-41-204E", 0x1102).unwrap();
        assert_eq!(d.mask, 0x07B0);
        let kinds: Vec<u16> = d.objects.iter().map(|o| o.kind).collect();
        assert_eq!(kinds, [0, 1, 2, 9, 3, 4]);
        assert_eq!(d.allocation, Some((0x4000, 0x6000)));
        let dump = d.dump();
        assert!(dump.contains("object 3 type 9\n  load state 0 Unloaded\n  pid 7: 00 00 00 00\n"), "{dump}");
        assert!(dump.contains("pid 78: 00 00 00 00 02 57"), "{dump}");
    }
}
