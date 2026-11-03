//! Full device memory image for download (M5).
//!
//! Combines the parameter image (device + module parameters overlaid onto the
//! knxprod `<Code>` templates) with the three System B tables:
//!
//! - **Group Address Table** (`<AddressTable>`) — built from the project links
//! - **Association Table** (`<AssociationTable>`) — built from the project links
//! - **Group Object Table** (`<ComObjectTable>`) — the knxprod template with the
//!   flags octet of each active group object overwritten
//!
//! The table entries are overlaid at the offsets declared by those knxprod
//! headers. The result is one byte buffer per code segment, ready to be written
//! to the device with `A_Memory_Write` (and the load-state machine driven with
//! `A_PropertyValue_Write` PropertyId 5 — see `docs/ets5-findings.md`).

use std::collections::HashMap;

use anyhow::{anyhow, Result};

use crate::memory::{build_parameter_image, SegmentImage};
use crate::tables::{self, GroupObject, GroupObjectFlags};
use crate::ApplicationProgram;

/// A complete device memory image, ready for the download sequence.
#[derive(Debug, Clone)]
pub struct DeviceImage {
    /// One buffer per knxprod code segment, with parameters and tables
    /// already overlaid. Absolute segments carry their device address.
    pub segments: Vec<SegmentImage>,
    /// The active group objects in ASAP order (0-based ASAP, matching the GoT
    /// record index). This is the filtered, visible-mode enumeration.
    pub active_group_objects: Vec<(u16, GroupObject)>,
    /// Where the Group Address Table was written (if the device has one).
    pub address_table: Option<TablePlacement>,
    /// Where the Association Table was written (if the device has one).
    pub association_table: Option<TablePlacement>,
    /// Where the Group Object Table was overlaid (if the device has one).
    pub group_object_table: Option<TablePlacement>,
}

/// A table overlaid into a code segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TablePlacement {
    pub segment_id: String,
    /// Byte offset of the table within the segment.
    pub offset: usize,
    /// Number of table bytes written/overlaid.
    pub len: usize,
}

/// Build the full download image for a device.
///
/// `values` maps parameter id (or `module-instance-id/param-id`) to its value
/// string; missing entries fall back to knxprod defaults. `links` maps the
/// 0-based group object number (ASAP) to the raw 16-bit group address; links
/// to inactive group objects are ignored.
pub fn build_device_image(
    ap: &ApplicationProgram,
    values: &HashMap<String, String>,
    links: &[(u16, u16)],
) -> Result<DeviceImage> {
    let mut image = build_parameter_image(ap, values)?;

    let active = tables::active_group_objects(ap, values);

    // Only linked, *visible* group objects become table entries.
    let active_links: Vec<(u16, u16)> = links
        .iter()
        .filter(|(asap, _)| active.iter().any(|(a, _)| a == asap))
        .copied()
        .collect();
    let (address_bytes, association_bytes) =
        tables::build_address_and_association_tables(&active_links);

    let static_ = ap
        .static_
        .as_ref()
        .ok_or_else(|| anyhow!("application program has no <Static>"))?;

    // Group Address Table.
    let mut address_table = None;
    if let Some(t) = &static_.address_table {
        let offset = t.offset.parse::<usize>().unwrap_or(0);
        overlay_bytes(
            &mut image.segments,
            &t.code_segment,
            offset,
            &address_bytes,
        )?;
        address_table = Some(TablePlacement {
            segment_id: t.code_segment.clone(),
            offset,
            len: address_bytes.len(),
        });
    }

    // Association Table.
    let mut association_table = None;
    if let Some(t) = &static_.association_table {
        let offset = t.offset.parse::<usize>().unwrap_or(0);
        overlay_bytes(
            &mut image.segments,
            &t.code_segment,
            offset,
            &association_bytes,
        )?;
        association_table = Some(TablePlacement {
            segment_id: t.code_segment.clone(),
            offset,
            len: association_bytes.len(),
        });
    }

    // Group Object Table: overlay flags onto the shipped template (the data
    // pointer octet is baked into the knxprod template, not computed).
    let mut group_object_table = None;
    if let Some(t) = &static_.com_object_table {
        let offset = t.offset.parse::<usize>().unwrap_or(0);
        let flags_by_record: HashMap<usize, GroupObjectFlags> = active
            .iter()
            .map(|(asap, obj)| (*asap as usize, obj.flags))
            .collect();

        let segment = image
            .segments
            .iter_mut()
            .find(|s| s.id == t.code_segment)
            .ok_or_else(|| anyhow!("GoT segment {} not found", t.code_segment))?;
        let overlaid = tables::overlay_group_object_table_at(&segment.data, offset, &flags_by_record);
        let len = overlaid.len().saturating_sub(offset);
        segment.data = overlaid;
        group_object_table = Some(TablePlacement {
            segment_id: t.code_segment.clone(),
            offset,
            len,
        });
    }

    Ok(DeviceImage {
        segments: image.segments,
        active_group_objects: active,
        address_table,
        association_table,
        group_object_table,
    })
}

/// Overlay `bytes` into a segment at `offset`, returning an error if it does
/// not fit.
fn overlay_bytes(
    segments: &mut [SegmentImage],
    segment_id: &str,
    offset: usize,
    bytes: &[u8],
) -> Result<()> {
    let segment = segments
        .iter_mut()
        .find(|s| s.id == segment_id)
        .ok_or_else(|| anyhow!("segment {segment_id} not found"))?;
    let end = offset
        .checked_add(bytes.len())
        .ok_or_else(|| anyhow!("table offset overflow in segment {segment_id}"))?;
    if end > segment.data.len() {
        return Err(anyhow!(
            "table of {} bytes does not fit in segment {} ({} bytes) at offset {}",
            bytes.len(),
            segment_id,
            segment.data.len(),
            offset
        ));
    }
    segment.data[offset..end].copy_from_slice(bytes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_device_image_overlays_all_tables() {
        let static_ = crate::Static {
            code: Some(crate::Code {
                absolute: vec![
                    crate::Segment {
                        id: "AS-4000".into(),
                        size: "8".into(),
                        address: "16384".into(),
                        ..Default::default()
                    },
                    crate::Segment {
                        id: "AS-4201".into(),
                        size: "8".into(),
                        address: "16897".into(),
                        ..Default::default()
                    },
                    crate::Segment {
                        id: "AS-4400".into(),
                        size: "8".into(),
                        address: "17408".into(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }),
            address_table: Some(crate::Table {
                code_segment: "AS-4000".into(),
                offset: "0".into(),
                max_entries: "10".into(),
            }),
            association_table: Some(crate::Table {
                code_segment: "AS-4201".into(),
                offset: "0".into(),
                max_entries: "10".into(),
            }),
            com_object_table: Some(crate::ComObjectTable {
                code_segment: "AS-4400".into(),
                offset: "0".into(),
                items: vec![crate::ComObject {
                    number: "0".into(),
                    communication_flag: "Enabled".into(),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        };
        let ap = crate::ApplicationProgram {
            static_: Some(static_),
            ..Default::default()
        };

        let values = HashMap::new();
        let links = vec![(0u16, 0x000Au16)];
        let image = build_device_image(&ap, &values, &links).unwrap();

        let seg = |id| image.segments.iter().find(|s| s.id == id).unwrap();
        // GrAT: length 2 (IA + 1 GA), IA 0.0.0, GA 0x000A.
        assert_eq!(&seg("AS-4000").data[0..5], &[0x02, 0x00, 0x00, 0x00, 0x0A]);
        // GrOAT: length 1, entry TSAP 1 / ASAP 0.
        assert_eq!(&seg("AS-4201").data[0..3], &[0x01, 0x01, 0x00]);
        // GoT: data pointer 0 (template), flags 0x04 (comm), 0x00, vft 0.
        assert_eq!(&seg("AS-4400").data[0..4], &[0x00, 0x04, 0x00, 0x00]);

        let t = image.address_table.as_ref().unwrap();
        assert_eq!((t.segment_id.as_str(), t.offset, t.len), ("AS-4000", 0, 5));
        let t = image.association_table.as_ref().unwrap();
        assert_eq!((t.segment_id.as_str(), t.offset, t.len), ("AS-4201", 0, 3));
        let t = image.group_object_table.as_ref().unwrap();
        assert_eq!((t.segment_id.as_str(), t.offset, t.len), ("AS-4400", 0, 8));
    }
}
