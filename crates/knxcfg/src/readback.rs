//! Reading a device's configuration over the bus (`knxcfg read`, M5): the
//! memory a download wrote, decoded by `knx_prod::device_image::decode_device`.
//!
//! - The application program must be loaded and be the project's
//!   (`PID_PROGRAM_VERSION` of the application object, `03_05_01 §4.2.6`).
//! - Absolute segments (System 7, couplers) are read at their knxprod
//!   address, relative segments (System B) at the address in
//!   `PID_TABLE_REFERENCE` of their load state machine.
//! - System B tables are read from the objects of types 1, 2 and 9: the
//!   count first, then the entries. A coupler's filter table is not read: the
//!   project's links decide it.

use anyhow::{bail, Context, Result};
use knx_prod::device_image::{DeviceImage, DeviceMemory};
use knx_prod::tables::TableFormat;
use knx_prod::Object;
use knx_transport::apdu::apci;
use knx_transport::mgmt::{self, Connection};
use knx_transport::Transport;

use crate::download::{hex, object_index, property_read, LOADED, PID_TABLE_REFERENCE};

/// `PID_PROGRAM_VERSION` (`03_05_01 §4.2.6`): manufacturer, application
/// number and version.
const PID_PROGRAM_VERSION: u8 = 13;
const OBJECT_ADDRESS_TABLE: u16 = 1;
const OBJECT_ASSOCIATION_TABLE: u16 = 2;
const OBJECT_APPLICATION: u16 = 3;
const OBJECT_GROUP_OBJECT_TABLE: u16 = 9;

/// Read what `template` (the image for the device's product) says is in
/// the device's memory. `app_id` is the application the project expects;
/// `written` are the addresses of the absolute segments the download plan
/// writes in full. Of the other absolute segments only those holding a
/// table are read; the rest are only allocated (RAM).
#[allow(clippy::too_many_arguments)]
pub fn read_memory(
    t: &mut dyn Transport,
    dst: u16,
    mask: u16,
    format: TableFormat,
    app_id: &[u8; 5],
    template: &DeviceImage,
    written: &[u32],
    log: &mut dyn FnMut(&str),
) -> Result<DeviceMemory> {
    let mut c = Connection::open(t, dst)?;
    let found = mgmt::device_descriptor_read(&mut c).context("reading the device descriptor")?;
    if found != mask {
        bail!("the device has mask {found:04X}h, the project's application needs {mask:04X}h");
    }

    if format != TableFormat::Coupler {
        let app = object_index(&mut c, &Object::Type(OBJECT_APPLICATION))?;
        let state = property_read(&mut c, app, apci::PID_LOAD_STATE_CONTROL)?;
        if state.first() != Some(&LOADED) {
            bail!("the application program is not loaded (load state {})", hex(&state));
        }
        let version = property_read(&mut c, app, PID_PROGRAM_VERSION)?;
        if version != app_id {
            bail!(
                "the device runs application {}, the project has {}",
                hex(&version),
                hex(app_id)
            );
        }
    }

    let mut memory = DeviceMemory::default();
    let tables = [&template.address_table, &template.association_table, &template.group_object_table];
    for seg in &template.segments {
        if seg.memory_type == "RAM" {
            continue;
        }
        let (address, full) = if !seg.address.is_empty() {
            let a = seg.address.trim().parse::<u16>().with_context(|| format!("segment {} address {:?}", seg.id, seg.address))?;
            (a, written.contains(&(a as u32)))
        } else if let Some(lsm) = seg.load_state_machine {
            (table_address(&mut c, lsm)?, true)
        } else {
            continue;
        };
        let has_table = tables.iter().any(|t| t.placement.as_ref().is_some_and(|p| p.segment_id == seg.id));
        if !full && !has_table {
            continue;
        }
        log(&format!("segment {} at {address:04X}h, {} octets{}", seg.id, seg.size, if full { "" } else { " (tables)" }));
        let mut read = seg.clone();
        read.data = mgmt::memory_read(&mut c, address, seg.size)?;
        if full {
            memory.segments.push(read);
        } else {
            memory.table_segments.push(read);
        }
    }

    if format == TableFormat::SystemB {
        // [count:2] then entries of 2 (addresses, objects) or 4 octets
        // (associations, [TSAP:2][ASAP:2] as knxcfg and ETS write them).
        for (kind, width, out) in [
            (OBJECT_ADDRESS_TABLE, 2, &mut memory.address_table),
            (OBJECT_ASSOCIATION_TABLE, 4, &mut memory.association_table),
            (OBJECT_GROUP_OBJECT_TABLE, 2, &mut memory.group_object_table),
        ] {
            let idx = object_index(&mut c, &Object::Type(kind))?;
            let address = table_address(&mut c, idx)?;
            let head = mgmt::memory_read(&mut c, address, 2)?;
            let count = u16::from_be_bytes([head[0], head[1]]) as usize;
            log(&format!("object {idx} (type {kind}) table at {address:04X}h, {count} entries"));
            let entries = mgmt::memory_read(&mut c, address.wrapping_add(2), count * width)?;
            out.extend_from_slice(&head);
            out.extend_from_slice(&entries);
        }
    }
    c.close()?;
    Ok(memory)
}

/// The memory address of an object's table or segment.
fn table_address(c: &mut Connection, obj: u8) -> Result<u16> {
    let v = property_read(c, obj, PID_TABLE_REFERENCE)?;
    let a = v.iter().fold(0u32, |a, b| a << 8 | *b as u32);
    u16::try_from(a).with_context(|| format!("object {obj}: table reference {a:#x} above 64 KiB"))
}
