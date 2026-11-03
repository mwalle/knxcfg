//! Full device image and download plan (M5).
//!
//! [`build_device_image`] combines the parameter image (see [`crate::memory`])
//! with the three group communication tables (see [`crate::tables`]):
//!
//! - **System 7** (mask 0701h/0705h): the tables live in absolute segments
//!   named by the knxprod `<AddressTable>`/`<AssociationTable>`/
//!   `<ComObjectTable>` headers and are overlaid into the segment images.
//! - **System B** (mask 07B0h): the tables are separate byte images, loaded
//!   into the table interface objects 1/2/3 with `LdCtrlWriteRelMem`.
//!
//! [`download_plan`] turns the knxprod load procedure into concrete
//! [`DownloadStep`]s: the application's own procedure
//! (`LoadProcedureStyle="ProductProcedure"`), or the mask procedure from
//! `knx_master.xml` with the application's fragments merged in
//! (`MergedProcedure`). Every load control is either translated or rejected;
//! nothing is dropped silently.

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail, Context, Result};

use crate::eval::Evaluation;
use crate::memory::{build_parameter_image_eval, decode_parameter_image, SegmentImage};
use crate::tables::{self, GroupObject, Link, TableFormat, TableLimits};
use crate::{ApplicationProgram, LoadStep, MasterData};

/// A complete device image, ready for the download sequence.
#[derive(Debug, Clone)]
pub struct DeviceImage {
    /// One buffer per knxprod code segment, with parameters (and, for
    /// System 7, the tables) already overlaid.
    pub segments: Vec<SegmentImage>,
    pub format: TableFormat,
    /// The active group objects by ASAP.
    pub active_group_objects: Vec<(u16, GroupObject)>,
    /// The links that made it into the tables (links to inactive objects are
    /// dropped and reported in `warnings`).
    pub links: Vec<Link>,
    pub address_table: TableImage,
    pub association_table: TableImage,
    pub group_object_table: TableImage,
    /// Parameters whose memory was overwritten by a higher-priority
    /// alternative (see [`crate::memory`]).
    pub shadowed: HashSet<String>,
    /// Non-fatal problems (dropped links, unsupported knxprod features, …).
    pub warnings: Vec<String>,
}

/// The bytes of one table and, for memory-mapped tables, where they were
/// overlaid.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TableImage {
    pub bytes: Vec<u8>,
    pub placement: Option<TablePlacement>,
}

/// A table overlaid into a code segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TablePlacement {
    pub segment_id: String,
    /// Byte offset of the table within the segment.
    pub offset: usize,
    pub len: usize,
}

/// The configuration decoded back out of a device image.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceConfiguration {
    pub parameters: HashMap<String, String>,
    pub links: Vec<Link>,
}

/// Build the download image for a device from user values and links.
pub fn build_device_image(
    ap: &ApplicationProgram,
    values: &HashMap<String, String>,
    links: &[Link],
) -> Result<DeviceImage> {
    build_device_image_eval(&Evaluation::new(ap, values), links)
}

/// Build the download image for an evaluated configuration.
pub fn build_device_image_eval(ev: &Evaluation, links: &[Link]) -> Result<DeviceImage> {
    let ap = ev.ap;
    let format = TableFormat::of(ap)?;
    let static_ = ap
        .static_
        .as_ref()
        .ok_or_else(|| anyhow!("application program has no <Static>"))?;
    let mut image = build_parameter_image_eval(ev)?;
    let mut warnings = ev.warnings.clone();
    if ap.is_secure_enabled == "true" {
        warnings.push(
            "the application supports KNX Data Secure, which knxcfg does not implement: \
             the device can only be downloaded while it is commissioned without security"
                .to_string(),
        );
    }
    for k in &ev.unknown_keys {
        warnings.push(format!("parameter {k} does not exist in {}; ignored", ap.id));
    }

    let (active, conflicts) = tables::active_group_objects_eval(ev);
    warnings.extend(conflicts);
    let active_set: HashSet<u16> = active.iter().map(|(a, _)| *a).collect();
    let mut active_links = Vec::new();
    for l in links {
        if active_set.contains(&l.asap) {
            active_links.push(*l);
        } else {
            warnings.push(format!(
                "link of group object {} to {} dropped: the object is not active with the \
                 current parameters",
                l.asap,
                ga_string(l.ga)
            ));
        }
    }
    let (address_bytes, association_bytes) =
        tables::build_address_and_association_tables(format, &active_links, TableLimits::of(ap))?;
    let linked: HashSet<u16> = active_links.iter().map(|l| l.asap).collect();

    let (address_table, association_table, group_object_table) = match format {
        TableFormat::System7 => {
            let place = |t: Option<&crate::Table>, name: &str| -> Result<(String, usize)> {
                let t = t.ok_or_else(|| anyhow!("application has no <{name}>"))?;
                if t.code_segment.is_empty() {
                    bail!("<{name}> has no CodeSegment");
                }
                Ok((t.code_segment.clone(), t.offset.parse().unwrap_or(0)))
            };
            let (seg, off) = place(static_.address_table.as_ref(), "AddressTable")?;
            let at = overlay(&mut image.segments, &seg, off, address_bytes)?;
            let (seg, off) = place(static_.association_table.as_ref(), "AssociationTable")?;
            let oat = overlay(&mut image.segments, &seg, off, association_bytes)?;

            let t = static_
                .com_object_table
                .as_ref()
                .ok_or_else(|| anyhow!("application has no <ComObjectTable>"))?;
            let off: usize = t.offset.parse().unwrap_or(0);
            let segment = image
                .segments
                .iter_mut()
                .find(|s| s.id == t.code_segment)
                .ok_or_else(|| anyhow!("GoT segment {} not found", t.code_segment))?;
            segment.data = tables::overlay_group_object_table(&segment.data, off, &active, &linked)?;
            let count = tables::group_object_table_entry_count(&segment.data, off).unwrap_or(0);
            let len = 3 + count * 4;
            let got = TableImage {
                bytes: segment.data[off..off + len].to_vec(),
                placement: Some(TablePlacement { segment_id: t.code_segment.clone(), offset: off, len }),
            };
            (at, oat, got)
        }
        TableFormat::SystemB => (
            TableImage { bytes: address_bytes, placement: None },
            TableImage { bytes: association_bytes, placement: None },
            TableImage {
                bytes: tables::build_group_object_table_b(&active, &linked)?,
                placement: None,
            },
        ),
    };

    Ok(DeviceImage {
        segments: image.segments,
        format,
        active_group_objects: active,
        links: active_links,
        address_table,
        association_table,
        group_object_table,
        shadowed: image.shadowed,
        warnings,
    })
}

fn ga_string(ga: u16) -> String {
    format!("{}/{}/{}", ga >> 11, (ga >> 8) & 0x07, ga & 0xFF)
}

fn overlay(segments: &mut [SegmentImage], id: &str, offset: usize, bytes: Vec<u8>) -> Result<TableImage> {
    let segment = segments
        .iter_mut()
        .find(|s| s.id == id)
        .ok_or_else(|| anyhow!("segment {id} not found"))?;
    let end = offset + bytes.len();
    if end > segment.data.len() {
        bail!(
            "table of {} bytes does not fit in segment {id} ({} bytes) at offset {offset}",
            bytes.len(),
            segment.data.len()
        );
    }
    segment.data[offset..end].copy_from_slice(&bytes);
    Ok(TableImage {
        placement: Some(TablePlacement { segment_id: id.to_string(), offset, len: bytes.len() }),
        bytes,
    })
}

/// Decode an image back into parameters and links: the inverse of
/// [`build_device_image_eval`] for the same evaluation. This checks that the
/// encoder and decoder agree; it does not prove that the bytes are what ETS
/// would write (only a comparison with an ETS download can do that).
pub fn read_back_device(ev: &Evaluation, image: &DeviceImage) -> Result<DeviceConfiguration> {
    let parameters = decode_parameter_image(ev, &image.segments)?;
    let links = tables::read_links(
        image.format,
        &image.address_table.bytes,
        &image.association_table.bytes,
    )?;
    Ok(DeviceConfiguration { parameters, links })
}

/// Which download to plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadKind {
    /// Complete download (application, parameters, tables).
    Full,
    /// Parameters only (`AppliesTo="par"`, mask procedure sub type `par`).
    Partial,
}

impl DownloadKind {
    fn applies(self, applies_to: &str) -> bool {
        if applies_to.trim().is_empty() {
            return true;
        }
        let want = match self {
            DownloadKind::Full => "full",
            DownloadKind::Partial => "par",
        };
        applies_to.split(',').any(|a| a.trim() == want)
    }

    fn mask_sub_type(self) -> &'static str {
        match self {
            DownloadKind::Full => "all",
            DownloadKind::Partial => "par",
        }
    }
}

/// One concrete step of the download sequence, ready to be encoded as
/// management messages by the transport layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadStep {
    Connect,
    Disconnect,
    Restart,
    /// Verify a property (`DM_InterfaceObjectVerify`).
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
    /// Load event `03h 00h/01h` (allocate an absolute data/stack segment)
    /// followed by the segment's data. `data` is empty for RAM and stack
    /// segments: ETS only allocates those.
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
    /// Load event `03h 02h`: task segment address, PEI type and application
    /// id (manufacturer, application number, version). The id is zero for the
    /// table load state machines (1 and 2).
    TaskSegment {
        lsm_idx: u8,
        address: u32,
        pei_type: u8,
        app_id: [u8; 5],
    },
    LoadCompleted {
        lsm_idx: u8,
    },
    /// Load event `03h 0Bh` (data relative allocation).
    RelSegment {
        lsm_idx: u8,
        size: u32,
        mode: u8,
        fill: u8,
    },
    /// Write `data` at `offset` relative to the object's
    /// `PID_TABLE_REFERENCE` (read from the device first).
    WriteRelMem {
        obj_idx: u8,
        offset: u32,
        data: Vec<u8>,
        verify: bool,
    },
    WriteProp {
        obj_idx: u8,
        prop_id: u8,
        data: Vec<u8>,
        verify: bool,
    },
    /// Read a property into the image (`LdCtrlLoadImageProp`).
    ReadProp {
        obj_idx: u8,
        prop_id: u8,
    },
    Delay {
        ms: u32,
    },
    /// Map a device error code for the following steps (0 = ignore).
    MapError {
        original: u32,
        mapped: u32,
    },
}

/// Build the download sequence for an image.
///
/// `ev` is the evaluation the image was built from (load procedures may
/// contain parameter-dependent `choose` blocks). `master` is required for
/// `MergedProcedure` applications (the mask's procedure lives in
/// `knx_master.xml`).
pub fn download_plan(
    ev: &Evaluation,
    image: &DeviceImage,
    master: Option<&MasterData>,
    kind: DownloadKind,
) -> Result<Vec<DownloadStep>> {
    let ap = ev.ap;
    let static_ = ap
        .static_
        .as_ref()
        .ok_or_else(|| anyhow!("application program has no <Static>"))?;
    let procedures = static_
        .load_procedures
        .as_ref()
        .map(|p| p.items.as_slice())
        .unwrap_or(&[]);

    let steps: Vec<LoadStep> = match ap.load_procedure_style.as_str() {
        "MergedProcedure" => {
            let master = master
                .ok_or_else(|| anyhow!("{} uses a merged load procedure; knx_master.xml is needed", ap.id))?;
            let mask = master
                .mask_version(&ap.mask_version)
                .ok_or_else(|| anyhow!("mask version {} not found in knx_master.xml", ap.mask_version))?;
            let proc_ = mask.load_procedure(kind.mask_sub_type()).ok_or_else(|| {
                anyhow!(
                    "mask {} has no remote load procedure of type {:?}",
                    ap.mask_version,
                    kind.mask_sub_type()
                )
            })?;
            let mut out = Vec::new();
            for step in &proc_.steps {
                match step {
                    LoadStep::Merge(m) => {
                        for fragment in procedures.iter().filter(|p| p.merge_id == m.merge_id) {
                            out.extend(fragment.steps.iter().cloned());
                        }
                    }
                    other => out.push(other.clone()),
                }
            }
            out
        }
        "ProductProcedure" | "" => {
            if kind != DownloadKind::Full {
                bail!("{} defines only a complete download procedure", ap.id);
            }
            let own: Vec<_> = procedures.iter().filter(|p| p.merge_id.is_empty()).collect();
            match own.as_slice() {
                [] => bail!("application program has no <LoadProcedure>"),
                [one] => one.steps.clone(),
                _ => bail!("application program has {} load procedures; expected one", own.len()),
            }
        }
        other => bail!("unsupported LoadProcedureStyle {other:?}"),
    };
    let mut flat = Vec::new();
    flatten_choose(ev, &steps, &mut flat);
    let steps = flat;
    let checks = device_checks(ev)?;

    let app_id = application_id(ap)?;
    let pei_type: u8 = ap.pei_type.parse().unwrap_or(0);

    let mut out = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        if !kind.applies(step.applies_to()) {
            continue;
        }
        // Pre-download verification right after the connection is up.
        if matches!(step, LoadStep::Connect) && out.is_empty() {
            out.push(DownloadStep::Connect);
            out.extend(checks.iter().cloned());
            continue;
        }
        let n = |s: &str, what: &str| -> Result<u32> {
            s.trim()
                .parse::<u32>()
                .with_context(|| format!("load step {}: bad {what} {s:?}", i + 1))
        };
        let b = |s: &str, what: &str| -> Result<u8> {
            n(s, what)?
                .try_into()
                .map_err(|_| anyhow!("load step {}: {what} {s:?} out of range", i + 1))
        };
        out.push(match step {
            LoadStep::Connect => DownloadStep::Connect,
            LoadStep::Disconnect => DownloadStep::Disconnect,
            LoadStep::Restart => DownloadStep::Restart,
            LoadStep::Unload(s) => DownloadStep::Unload { lsm_idx: b(&s.lsm_idx, "LsmIdx")? },
            LoadStep::Load(s) => DownloadStep::Load { lsm_idx: b(&s.lsm_idx, "LsmIdx")? },
            LoadStep::LoadCompleted(s) => {
                DownloadStep::LoadCompleted { lsm_idx: b(&s.lsm_idx, "LsmIdx")? }
            }
            LoadStep::TaskSegment(s) => {
                let lsm_idx = b(&s.lsm_idx, "LsmIdx")?;
                let table = matches!(lsm_idx, 1 | 2);
                DownloadStep::TaskSegment {
                    lsm_idx,
                    address: n(&s.address, "Address")?,
                    pei_type: if table { 0 } else { pei_type },
                    app_id: if table { [0; 5] } else { app_id },
                }
            }
            LoadStep::CompareProp(s) => DownloadStep::CompareProp {
                obj_idx: b(&s.obj_idx, "ObjIdx")?,
                prop_id: b(&s.prop_id, "PropId")?,
                inline_data: parse_hex(&s.inline_data)
                    .with_context(|| format!("load step {}: InlineData", i + 1))?,
            },
            LoadStep::AbsSegment(s) => {
                let address = n(&s.address, "Address")?;
                let size = n(&s.size, "Size")? as usize;
                let seg_type = b(&s.seg_type, "SegType")?;
                let mem_type = b(&s.mem_type, "MemType")?;
                // RAM (2) and stack segments are allocated, not written.
                let data = if mem_type == 2 || seg_type == 1 {
                    Vec::new()
                } else {
                    let segment = image
                        .segments
                        .iter()
                        .find(|seg| seg.address.parse::<u32>().ok() == Some(address))
                        .ok_or_else(|| {
                            anyhow!("load step {}: no code segment at address {address:#06x}", i + 1)
                        })?;
                    if segment.data.len() > size {
                        bail!(
                            "load step {}: segment {} is {} bytes, the load procedure allocates {size}",
                            i + 1,
                            segment.id,
                            segment.data.len()
                        );
                    }
                    let mut data = segment.data.clone();
                    data.resize(size, 0);
                    data
                };
                DownloadStep::AbsSegment {
                    lsm_idx: b(&s.lsm_idx, "LsmIdx")?,
                    seg_type,
                    address,
                    size,
                    access: b(&s.access, "Access")?,
                    mem_type,
                    seg_flags: b(&s.seg_flags, "SegFlags")?,
                    data,
                }
            }
            LoadStep::RelSegment(s) => {
                let lsm_idx = b(&s.lsm_idx, "LsmIdx")?;
                // For the tables the size is the current table size
                // (`03_05_03 §3.9.3.4`, `Size="*"`).
                let size = match table_of(image, lsm_idx) {
                    Some(t) => t.bytes.len() as u32,
                    None => n(&s.size, "Size")?,
                };
                DownloadStep::RelSegment {
                    lsm_idx,
                    size,
                    mode: b(&s.mode, "Mode")?,
                    fill: b(&s.fill, "Fill")?,
                }
            }
            LoadStep::WriteRelMem(s) => {
                let obj_idx = b(&s.obj_idx, "ObjIdx")?;
                let offset = n(&s.offset, "Offset")?;
                let size = n(&s.size, "Size")? as usize;
                let source: &[u8] = match table_of(image, obj_idx) {
                    Some(t) => &t.bytes,
                    None => {
                        &image
                            .segments
                            .iter()
                            .find(|seg| seg.load_state_machine == Some(obj_idx))
                            .ok_or_else(|| {
                                anyhow!("load step {}: no relative segment for object {obj_idx}", i + 1)
                            })?
                            .data
                    }
                };
                let start = (offset as usize).min(source.len());
                let end = start.saturating_add(size).min(source.len());
                DownloadStep::WriteRelMem {
                    obj_idx,
                    offset,
                    data: source[start..end].to_vec(),
                    verify: s.verify == "true",
                }
            }
            LoadStep::WriteProp(s) => {
                let prop_id = b(&s.prop_id, "PropId")?;
                let mut data = parse_hex(&s.inline_data)
                    .with_context(|| format!("load step {}: InlineData", i + 1))?;
                // PID_PROGRAM_VERSION: the mask procedure carries a zero
                // placeholder for the application id (`InlineData="*"`).
                if prop_id == 13 && data.iter().all(|b| *b == 0) {
                    data = app_id.to_vec();
                }
                DownloadStep::WriteProp {
                    obj_idx: b(&s.obj_idx, "ObjIdx")?,
                    prop_id,
                    data,
                    verify: s.verify == "true",
                }
            }
            LoadStep::LoadImageProp(s) => DownloadStep::ReadProp {
                obj_idx: b(&s.obj_idx, "ObjIdx")?,
                prop_id: b(&s.prop_id, "PropId")?,
            },
            LoadStep::Delay(s) => DownloadStep::Delay { ms: n(&s.milliseconds, "MilliSeconds")? },
            LoadStep::MapError(s) => DownloadStep::MapError {
                original: n(&s.original_error, "OriginalError")?,
                mapped: n(&s.mapped_error, "MappedError")?,
            },
            LoadStep::Merge(m) => bail!(
                "load step {}: LdCtrlMerge {} outside a merged procedure",
                i + 1,
                m.merge_id
            ),
            LoadStep::Choose(_) => unreachable!("flattened above"),
            LoadStep::Other => bail!(
                "load step {}: unsupported load control (supported: Connect, Disconnect, \
                 Restart, Unload, Load, LoadCompleted, AbsSegment, TaskSegment, RelSegment, \
                 WriteRelMem, WriteProp, CompareProp, LoadImageProp, Delay, MapError, Merge)",
                i + 1
            ),
        });
    }
    Ok(out)
}

/// `CompareProp` steps for the device-identity property parameters
/// (`PID_HARDWARE_TYPE` / `PID_ORDER_INFO` of the device object).
fn device_checks(ev: &Evaluation) -> Result<Vec<DownloadStep>> {
    let mut out = Vec::new();
    let scope = &ev.scopes[0];
    for p in scope.parameters() {
        let Some(loc) = p.property.as_ref().filter(|l| crate::eval::is_device_check(l)) else {
            continue;
        };
        let value = ev.value(&scope.key(&p.id)).unwrap_or(&p.value);
        let data = crate::memory::encode_value_bytes(ev.ap, p, value)
            .with_context(|| format!("{} = {value:?}", p.id))?;
        let check = DownloadStep::CompareProp {
            obj_idx: 0,
            prop_id: loc.property_id.trim().parse().unwrap_or(0),
            inline_data: data,
        };
        if !out.contains(&check) {
            out.push(check);
        }
    }
    Ok(out)
}

/// Replace `choose` blocks by the steps of their active `when` branches
/// (device-level parameters; all matching tests, `default` if none).
fn flatten_choose(ev: &Evaluation, steps: &[LoadStep], out: &mut Vec<LoadStep>) {
    for step in steps {
        match step {
            LoadStep::Choose(c) => {
                let current = ev.ref_value(0, &c.param_ref_id).unwrap_or("");
                let matching: Vec<_> = c
                    .whens
                    .iter()
                    .filter(|w| w.default != "true" && crate::eval::test_matches(&w.test, current))
                    .collect();
                let active = if matching.is_empty() {
                    c.whens.iter().filter(|w| w.default == "true").collect()
                } else {
                    matching
                };
                for w in active {
                    flatten_choose(ev, &w.steps, out);
                }
            }
            other => out.push(other.clone()),
        }
    }
}

/// The table image loaded through a System B table object (1/2/3).
fn table_of(image: &DeviceImage, idx: u8) -> Option<&TableImage> {
    if image.format != TableFormat::SystemB {
        return None;
    }
    match idx {
        1 => Some(&image.address_table),
        2 => Some(&image.association_table),
        3 => Some(&image.group_object_table),
        _ => None,
    }
}

/// The 5-octet application id: manufacturer (2), application number (2),
/// application version (1).
pub fn application_id(ap: &ApplicationProgram) -> Result<[u8; 5]> {
    let manufacturer = ap
        .id
        .strip_prefix("M-")
        .and_then(|s| s.get(..4))
        .and_then(|s| u16::from_str_radix(s, 16).ok())
        .ok_or_else(|| anyhow!("cannot read the manufacturer from {:?}", ap.id))?;
    let number: u16 = ap
        .application_number
        .parse()
        .map_err(|_| anyhow!("bad ApplicationNumber {:?}", ap.application_number))?;
    let version: u8 = ap
        .application_version
        .parse()
        .map_err(|_| anyhow!("bad ApplicationVersion {:?}", ap.application_version))?;
    let [m0, m1] = manufacturer.to_be_bytes();
    let [n0, n1] = number.to_be_bytes();
    Ok([m0, m1, n0, n1, version])
}

fn parse_hex(s: &str) -> Result<Vec<u8>> {
    let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.len() % 2 != 0 {
        bail!("odd number of hex digits in {s:?}");
    }
    (0..compact.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&compact[i..i + 2], 16).map_err(|_| anyhow!("bad hex {s:?}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LdCtrlAbsSegment, LdCtrlLoad, LdCtrlLoadCompleted, LdCtrlTaskSegment, LdCtrlUnload};

    fn system7_ap() -> ApplicationProgram {
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
                        // count 1, RAM-flags 0x0700, descriptor 071A DF 00, pad.
                        data: Some("AQcABxrfAAA=".into()),
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
                    object_size: "1 Bit".into(),
                    ..Default::default()
                }],
            }),
            load_procedures: Some(crate::LoadProcedures {
                items: vec![crate::LoadProcedure {
                    merge_id: String::new(),
                    steps: vec![
                        LoadStep::Connect,
                        LoadStep::Unload(LdCtrlUnload { lsm_idx: "3".into() }),
                        LoadStep::Load(LdCtrlLoad { lsm_idx: "3".into() }),
                        LoadStep::AbsSegment(LdCtrlAbsSegment {
                            lsm_idx: "3".into(),
                            seg_type: "0".into(),
                            address: "1792".into(),
                            size: "82".into(),
                            access: "0".into(),
                            mem_type: "2".into(),
                            seg_flags: "0".into(),
                        }),
                        LoadStep::AbsSegment(LdCtrlAbsSegment {
                            lsm_idx: "3".into(),
                            seg_type: "0".into(),
                            address: "17408".into(),
                            size: "8".into(),
                            access: "255".into(),
                            mem_type: "3".into(),
                            seg_flags: "128".into(),
                        }),
                        LoadStep::TaskSegment(LdCtrlTaskSegment {
                            lsm_idx: "3".into(),
                            address: "17408".into(),
                        }),
                        LoadStep::LoadCompleted(LdCtrlLoadCompleted { lsm_idx: "3".into() }),
                        LoadStep::Restart,
                        LoadStep::Disconnect,
                    ],
                }],
            }),
            ..Default::default()
        };
        crate::ApplicationProgram {
            id: "M-0083_A-00B6-32-9642".into(),
            application_number: "182".into(),
            application_version: "50".into(),
            mask_version: "MV-0705".into(),
            load_procedure_style: "ProductProcedure".into(),
            pei_type: "1".into(),
            static_: Some(static_),
            ..Default::default()
        }
    }

    #[test]
    fn system7_image_overlays_all_tables() {
        let ap = system7_ap();
        let links = vec![Link::new(0, 0x000A)];
        let image = build_device_image(&ap, &HashMap::new(), &links).unwrap();
        let seg = |id| image.segments.iter().find(|s| s.id == id).unwrap();
        assert_eq!(&seg("AS-4000").data[0..5], &[0x02, 0x00, 0x00, 0x00, 0x0A]);
        assert_eq!(&seg("AS-4201").data[0..3], &[0x01, 0x01, 0x00]);
        // GoT: header kept; ASAP 0 linked: comm + template priority 11.
        assert_eq!(&seg("AS-4400").data[0..7], &[0x01, 0x07, 0x00, 0x07, 0x1A, 0x07, 0x00]);
        assert_eq!(image.group_object_table.placement.as_ref().unwrap().len, 7);

        let ev = Evaluation::new(&ap, &HashMap::new());
        let config = read_back_device(&ev, &image).unwrap();
        assert_eq!(config.links, vec![Link { asap: 0, ga: 0x000A, send: true }]);
    }

    #[test]
    fn links_to_inactive_objects_are_reported() {
        let ap = system7_ap();
        let image = build_device_image(&ap, &HashMap::new(), &[Link::new(5, 1)]).unwrap();
        assert!(image.links.is_empty());
        assert!(image.warnings.iter().any(|w| w.contains("group object 5")));
    }

    #[test]
    fn product_procedure_plan() {
        let ap = system7_ap();
        let image = build_device_image(&ap, &HashMap::new(), &[]).unwrap();
        let plan = download_plan(&Evaluation::new(&ap, &HashMap::new()), &image, None, DownloadKind::Full).unwrap();
        assert_eq!(plan.len(), 9);
        // RAM segment: allocated, no data (and no image segment needed).
        assert!(matches!(&plan[3], DownloadStep::AbsSegment { mem_type: 2, data, .. } if data.is_empty()));
        assert!(matches!(&plan[4], DownloadStep::AbsSegment { data, .. } if data.len() == 8));
        assert_eq!(
            plan[5],
            DownloadStep::TaskSegment {
                lsm_idx: 3,
                address: 17408,
                pei_type: 1,
                app_id: [0x00, 0x83, 0x00, 0xB6, 0x32],
            }
        );
        assert!(download_plan(&Evaluation::new(&ap, &HashMap::new()), &image, None, DownloadKind::Partial).is_err());
    }

    #[test]
    fn unknown_load_controls_are_errors() {
        let mut ap = system7_ap();
        ap.static_.as_mut().unwrap().load_procedures.as_mut().unwrap().items[0]
            .steps
            .push(LoadStep::Other);
        let image = build_device_image(&ap, &HashMap::new(), &[]).unwrap();
        assert!(download_plan(&Evaluation::new(&ap, &HashMap::new()), &image, None, DownloadKind::Full).is_err());
    }

    #[test]
    fn merged_procedure_system_b() {
        let master_xml = r#"<KNX><MasterData><MaskVersions>
          <MaskVersion Id="MV-07B0" Name="System B" ManagementModel="SystemB"><HawkConfigurationData><Procedures>
            <Procedure ProcedureType="Load" ProcedureSubType="all" Access="remote local2">
              <LdCtrlConnect />
              <LdCtrlLoad LsmIdx="4" />
              <LdCtrlMerge MergeId="2" />
              <LdCtrlLoad LsmIdx="1" />
              <LdCtrlRelSegment LsmIdx="1" Size="2" Mode="0" Fill="0" />
              <LdCtrlMerge MergeId="4" />
              <LdCtrlWriteRelMem ObjIdx="1" Offset="0" Size="1048576" Verify="true" />
              <LdCtrlWriteProp ObjIdx="4" PropId="13" Verify="true" InlineData="0000000000" />
              <LdCtrlRestart />
            </Procedure>
          </Procedures></HawkConfigurationData></MaskVersion>
        </MaskVersions></MasterData></KNX>"#;
        let master = crate::master::parse_master(master_xml).unwrap();
        let app_xml = r#"<LoadProcedures>
              <LoadProcedure MergeId="2">
                <LdCtrlRelSegment AppliesTo="full" LsmIdx="4" Size="4" Mode="1" Fill="0" />
                <LdCtrlRelSegment AppliesTo="par" LsmIdx="4" Size="4" Mode="0" Fill="0" />
              </LoadProcedure>
              <LoadProcedure MergeId="4">
                <LdCtrlWriteRelMem AppliesTo="full,par" ObjIdx="4" Offset="0" Size="4" Verify="true" />
              </LoadProcedure>
            </LoadProcedures>"#;
        let procs: crate::LoadProcedures = quick_xml::de::from_str(app_xml).unwrap();
        let ap = ApplicationProgram {
            id: "M-0071_A-1421-14-7A11".into(),
            application_number: "5153".into(),
            application_version: "20".into(),
            mask_version: "MV-07B0".into(),
            load_procedure_style: "MergedProcedure".into(),
            static_: Some(crate::Static {
                code: Some(crate::Code {
                    relative: vec![crate::Segment {
                        id: "RS-04".into(),
                        size: "4".into(),
                        load_state_machine: "4".into(),
                        data: Some("AQIDBA==".into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                com_objects: None,
                com_object_table: Some(crate::ComObjectTable {
                    items: vec![crate::ComObject {
                        number: "1".into(),
                        communication_flag: "Enabled".into(),
                        object_size: "1 Byte".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                load_procedures: Some(procs),
                ..Default::default()
            }),
            ..Default::default()
        };
        let image = build_device_image(&ap, &HashMap::new(), &[Link::new(1, 0x0801)]).unwrap();
        assert_eq!(image.address_table.bytes, [0x00, 0x01, 0x08, 0x01]);
        assert!(download_plan(&Evaluation::new(&ap, &HashMap::new()), &image, None, DownloadKind::Full).is_err());
        let plan = download_plan(&Evaluation::new(&ap, &HashMap::new()), &image, Some(&master), DownloadKind::Full).unwrap();
        assert_eq!(
            plan,
            vec![
                DownloadStep::Connect,
                DownloadStep::Load { lsm_idx: 4 },
                DownloadStep::RelSegment { lsm_idx: 4, size: 4, mode: 1, fill: 0 },
                DownloadStep::Load { lsm_idx: 1 },
                DownloadStep::RelSegment { lsm_idx: 1, size: 4, mode: 0, fill: 0 },
                DownloadStep::WriteRelMem { obj_idx: 4, offset: 0, data: vec![1, 2, 3, 4], verify: true },
                DownloadStep::WriteRelMem {
                    obj_idx: 1,
                    offset: 0,
                    data: vec![0x00, 0x01, 0x08, 0x01],
                    verify: true
                },
                DownloadStep::WriteProp {
                    obj_idx: 4,
                    prop_id: 13,
                    data: vec![0x00, 0x71, 0x14, 0x21, 0x14],
                    verify: true
                },
                DownloadStep::Restart,
            ]
        );
    }
}
