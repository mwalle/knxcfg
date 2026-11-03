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

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, Result};

use crate::memory::{build_parameter_image, SegmentImage};
use crate::tables::{self, GroupObject};
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

/// The project-relevant configuration decoded back out of a device image.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceConfiguration {
    /// Parameter id (or `module-instance-id/param-id`) → value string.
    pub parameters: HashMap<String, String>,
    /// Links as `(ASAP, raw 16-bit group address)`, sorted by ASAP.
    pub links: Vec<(u16, u16)>,
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
    // pointer octet is baked into the knxprod template, not computed). The
    // template's own placeholder octet supplies the priority bits; hidden
    // objects keep the template flags with communication cleared.
    let mut group_object_table = None;
    if let Some(t) = &static_.com_object_table {
        let offset = t.offset.parse::<usize>().unwrap_or(0);
        let segment = image
            .segments
            .iter()
            .find(|s| s.id == t.code_segment)
            .ok_or_else(|| anyhow!("GoT segment {} not found", t.code_segment))?;
        let linked: HashSet<u16> = active_links.iter().map(|(asap, _)| *asap).collect();
        let flags_by_asap: HashMap<u16, u8> = active
            .iter()
            .map(|(asap, obj)| {
                let slot_flags = segment
                    .data
                    .get(offset + (*asap as usize + 1) * 4 + 1)
                    .copied()
                    .unwrap_or(0);
                (
                    *asap,
                    tables::visible_group_object_flags(
                        obj.flags,
                        slot_flags,
                        linked.contains(asap),
                    ),
                )
            })
            .collect();

        let segment = image
            .segments
            .iter_mut()
            .find(|s| s.id == t.code_segment)
            .ok_or_else(|| anyhow!("GoT segment {} not found", t.code_segment))?;
        let overlaid = tables::overlay_group_object_table_at(&segment.data, offset, &flags_by_asap);
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

/// Decode a built device image back into project-relevant configuration.
///
/// This is the read-back half of M5: parameters are read from their `<Memory>`
/// placements, and group-object links are decoded from the GrAT/GrOAT. It is
/// the inverse of [`build_device_image`] and is what the verify/download
/// read-back step will compare against the original project values.
pub fn read_back_device(
    ap: &ApplicationProgram,
    image: &DeviceImage,
) -> Result<DeviceConfiguration> {
    let parameters = crate::memory::decode_parameter_image(ap, &image.segments)?;

    let mut links = Vec::new();
    if let (Some(at), Some(oat)) = (&image.address_table, &image.association_table) {
        let address_bytes = segment_slice(&image.segments, &at.segment_id, at.offset);
        let association_bytes = segment_slice(&image.segments, &oat.segment_id, oat.offset);
        links = tables::read_links(address_bytes, association_bytes)?;
    }

    Ok(DeviceConfiguration { parameters, links })
}

fn segment_slice<'a>(segments: &'a [SegmentImage], id: &str, offset: usize) -> &'a [u8] {
    segments
        .iter()
        .find(|s| s.id == id)
        .and_then(|s| s.data.get(offset..))
        .unwrap_or_default()
}

/// One concrete step of the download sequence, ready to be encoded as cEMI
/// management messages by the transport layer (M4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadStep {
    Connect,
    Disconnect,
    Restart,
    CompareProp {
        obj_idx: u8,
        prop_id: u8,
        inline_data: Vec<u8>,
    },
    Unload {
        lsm_idx: u8,
    },
    Load {
        lsm_idx: u8,
    },
    AbsSegment {
        lsm_idx: u8,
        seg_type: u8,
        address: u32,
        size: usize,
        access: u8,
        mem_type: u8,
        seg_flags: u8,
        data: Vec<u8>,
    },
    TaskSegment {
        lsm_idx: u8,
        address: u32,
    },
    LoadCompleted {
        lsm_idx: u8,
    },
}

/// Build the download sequence from the knxprod `<LoadProcedure>` and the
/// assembled [`DeviceImage`].
///
/// The knxprod load procedure is the authoritative download sequence — the
/// same sequence ETS follows (`docs/ets5-findings.md`). Absolute segments are
/// matched to the image buffers by their device address.
pub fn download_plan(ap: &ApplicationProgram, image: &DeviceImage) -> Result<Vec<DownloadStep>> {
    let static_ = ap
        .static_
        .as_ref()
        .ok_or_else(|| anyhow!("application program has no <Static>"))?;
    let procedure = static_
        .load_procedures
        .as_ref()
        .and_then(|p| p.items.first())
        .ok_or_else(|| anyhow!("application program has no <LoadProcedure>"))?;

    let segments_by_address: HashMap<u32, &SegmentImage> = image
        .segments
        .iter()
        .filter_map(|s| {
            let address = s.address.parse::<u32>().ok()?;
            Some((address, s))
        })
        .collect();

    let mut out = Vec::new();
    for step in &procedure.steps {
        match step {
            crate::LoadStep::Connect => out.push(DownloadStep::Connect),
            crate::LoadStep::Disconnect => out.push(DownloadStep::Disconnect),
            crate::LoadStep::Restart => out.push(DownloadStep::Restart),
            crate::LoadStep::Unload(s) => out.push(DownloadStep::Unload {
                lsm_idx: s.lsm_idx.parse().unwrap_or(0),
            }),
            crate::LoadStep::Load(s) => out.push(DownloadStep::Load {
                lsm_idx: s.lsm_idx.parse().unwrap_or(0),
            }),
            crate::LoadStep::LoadCompleted(s) => out.push(DownloadStep::LoadCompleted {
                lsm_idx: s.lsm_idx.parse().unwrap_or(0),
            }),
            crate::LoadStep::TaskSegment(s) => out.push(DownloadStep::TaskSegment {
                lsm_idx: s.lsm_idx.parse().unwrap_or(0),
                address: s.address.parse().unwrap_or(0),
            }),
            crate::LoadStep::CompareProp(s) => out.push(DownloadStep::CompareProp {
                obj_idx: s.obj_idx.parse().unwrap_or(0),
                prop_id: s.prop_id.parse().unwrap_or(0),
                inline_data: parse_hex_bytes(&s.inline_data),
            }),
            crate::LoadStep::AbsSegment(s) => {
                let address = s.address.parse::<u32>().unwrap_or(0);
                let size = s.size.parse::<usize>().unwrap_or(0);
                let segment = segments_by_address.get(&address).ok_or_else(|| {
                    anyhow!(
                        "load procedure references address {} but no image segment has that address",
                        address
                    )
                })?;
                let mut data = segment.data.clone();
                if data.len() != size {
                    data.resize(size, 0);
                }
                out.push(DownloadStep::AbsSegment {
                    lsm_idx: s.lsm_idx.parse().unwrap_or(0),
                    seg_type: s.seg_type.parse().unwrap_or(0),
                    address,
                    size,
                    access: s.access.parse().unwrap_or(0),
                    mem_type: s.mem_type.parse().unwrap_or(0),
                    seg_flags: s.seg_flags.parse().unwrap_or(0),
                    data,
                });
            }
            // Not planned yet (RelSegment, WriteProp, choose, ...).
            _ => {}
        }
    }

    Ok(out)
}

fn parse_hex_bytes(s: &str) -> Vec<u8> {
    let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.len() % 2 != 0 {
        return Vec::new();
    }
    compact
        .as_bytes()
        .chunks(2)
        .filter_map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap_or(""), 16).ok())
        .collect()
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
                        // header (entry count 1) + one GO slot (flags 0xdf).
                        data: Some("AQcABxrfAAc=".into()),
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
        // GoT: header [entry-count][07][00][07] unchanged; ASAP 0 gets
        // flags 0x07 (comm + low priority from the template slot), linked.
        assert_eq!(&seg("AS-4400").data[0..4], &[0x01, 0x07, 0x00, 0x07]);
        assert_eq!(&seg("AS-4400").data[4..8], &[0x1a, 0x07, 0x00, 0x07]);

        let t = image.address_table.as_ref().unwrap();
        assert_eq!((t.segment_id.as_str(), t.offset, t.len), ("AS-4000", 0, 5));
        let t = image.association_table.as_ref().unwrap();
        assert_eq!((t.segment_id.as_str(), t.offset, t.len), ("AS-4201", 0, 3));
        let t = image.group_object_table.as_ref().unwrap();
        assert_eq!((t.segment_id.as_str(), t.offset, t.len), ("AS-4400", 0, 8));

        let config = read_back_device(&ap, &image).unwrap();
        assert_eq!(config.links, links);
        assert!(config.parameters.is_empty());
    }

    #[test]
    fn download_plan_follows_knxprod_load_procedure() {
        let static_ = crate::Static {
            code: Some(crate::Code {
                absolute: vec![crate::Segment {
                    id: "AS-4000".into(),
                    size: "4".into(),
                    address: "16384".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            load_procedures: Some(crate::LoadProcedures {
                items: vec![crate::LoadProcedure {
                    merge_id: String::new(),
                    steps: vec![
                        crate::LoadStep::Connect,
                        crate::LoadStep::Unload(crate::LdCtrlUnload { lsm_idx: "1".into() }),
                        crate::LoadStep::Load(crate::LdCtrlLoad { lsm_idx: "1".into() }),
                        crate::LoadStep::AbsSegment(crate::LdCtrlAbsSegment {
                            lsm_idx: "1".into(),
                            seg_type: "0".into(),
                            address: "16384".into(),
                            size: "4".into(),
                            access: "255".into(),
                            mem_type: "3".into(),
                            seg_flags: "128".into(),
                        }),
                        crate::LoadStep::TaskSegment(crate::LdCtrlTaskSegment {
                            lsm_idx: "1".into(),
                            address: "16384".into(),
                        }),
                        crate::LoadStep::LoadCompleted(crate::LdCtrlLoadCompleted {
                            lsm_idx: "1".into(),
                        }),
                        crate::LoadStep::Restart,
                        crate::LoadStep::Disconnect,
                    ],
                }],
            }),
            ..Default::default()
        };
        let ap = crate::ApplicationProgram {
            static_: Some(static_),
            ..Default::default()
        };
        let image = build_device_image(&ap, &HashMap::new(), &[]).unwrap();
        let plan = download_plan(&ap, &image).unwrap();

        assert_eq!(plan.len(), 8);
        assert_eq!(plan[0], DownloadStep::Connect);
        assert_eq!(plan[1], DownloadStep::Unload { lsm_idx: 1 });
        assert_eq!(
            plan[3],
            DownloadStep::AbsSegment {
                lsm_idx: 1,
                seg_type: 0,
                address: 16384,
                size: 4,
                access: 255,
                mem_type: 3,
                seg_flags: 128,
                data: vec![0, 0, 0, 0],
            }
        );
        assert_eq!(plan[7], DownloadStep::Disconnect);
    }
}
