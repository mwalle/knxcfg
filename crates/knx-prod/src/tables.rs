//! Memory-image table builders (M5): Group Address Table, Association Table
//! and Group Object Table.
//!
//! These produce the byte content that ETS writes into the table memory
//! segments at download time (the knxprod `<AddressTable>`/
//! `<AssociationTable>`/`<ComObjectTable>` headers give the segment + offset;
//! the entries themselves are empty in knxprod).
//!
//! The entry formats follow the KNX System B realisations (source:
//! `03_05_01 §4.16/4.17/4.18`):
//!
//! - **Group Address Table** — Realisation Type 7 (`§4.16.8`): a 2-octet
//!   current length followed by that many 16-bit group addresses, sorted
//!   ascending. The TSAP (connection number) is `1` for the first entry.
//! - **Association Table** — Realisation Type 6, format 0 (`§4.17.5`): a
//!   2-octet current length followed by 2-octet associations
//!   `TSAP` (high octet) `ASAP` (low octet), both 1-based. `ASAP` is the
//!   group object number; `TSAP` indexes the Group Address Table.
//! - **Group Object Table** — Realisation Type 7 (`§4.18.6`): a 2-octet
//!   current length followed by 2-octet group object descriptors (flags in
//!   the high octet, value field type in the low octet).
//!
//! All multi-octet integers are **big-endian**, matching the KNX spec's
//! `PDT_UNSIGNED_INT` / 16-bit field convention. (Byte-identity against the
//! ETS5 oracle is still to be confirmed — see `check_*` helpers.)

use std::collections::HashMap;

use crate::{ApplicationProgram, ModuleDef, ModuleInstance, UiNode};

/// One entry of the Association Table (GrOAT format 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Association {
    /// Transport Service Access Point: 1-based index into the Group Address
    /// Table (0 = none / unused).
    pub tsap: u16,
    /// Application Service Access Point: 1-based group object number
    /// (0 = none / unused).
    pub asap: u16,
}

/// A complete set of device tables ready for download.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceTables {
    /// Group Address Table bytes (GrAT Realisation Type 7).
    pub address: Vec<u8>,
    /// Association Table bytes (GrOAT format 0).
    pub association: Vec<u8>,
    /// Group Object Table bytes (Realisation Type 7).
    pub group_objects: Vec<u8>,
}

/// Build all three device tables from the group objects and the links.
///
/// `objects` are the device's group objects in ASAP order (index `i` →
/// ASAP `i + 1`). `links` map an ASAP (group object number) to a 16-bit group
/// address; any group object without a link is associated with TSAP 0
/// (unused). `ga_dpt` is ignored here — only the 16-bit address matters.
pub fn build_device_tables(objects: &[GroupObject], links: &[(u16, u16)]) -> DeviceTables {
    // Collect the set of referenced group addresses, in ascending order, to
    // form the Group Address Table. TSAP = 1-based index into this list.
    let mut gas: Vec<u16> = links.iter().map(|&(_, ga)| ga).collect();
    gas.sort_unstable();
    gas.dedup();
    let tsap_of: std::collections::HashMap<u16, u16> = gas
        .iter()
        .enumerate()
        .map(|(i, &ga)| (ga, i as u16 + 1))
        .collect();

    let address = build_group_address_table(gas);

    let associations: Vec<Association> = links
        .iter()
        .filter_map(|&(asap, ga)| {
            tsap_of.get(&ga).map(|&tsap| Association { tsap, asap })
        })
        .collect();
    let association = build_association_table(associations);

    let group_objects = build_group_object_table(objects);

    DeviceTables { address, association, group_objects }
}

/// Build the list of `GroupObject` descriptors (and their absolute ASAP
/// numbers) from an application program, expanding module group objects.
///
/// Device-level `<ComObjectTable>` objects keep their `Number` as the ASAP.
/// Module `<ComObjects>` objects use `BaseNumber (argument) + Number`, where
/// `BaseNumber` names the module-definition argument (e.g. `BaseObj`,
/// `BaseObjGroupt`, `ObjNumberBase`) whose per-instance value is the stride
/// base. `Allocates` on the argument is the per-instance stride.
///
/// NOTE: this enumerates **all** `Dynamic` module instances, including
/// mutually-exclusive channel modes (e.g. "single" vs "group" input). Those
/// modes overlap in ASAP space; the *active* mode is chosen by `choose`/`when`
/// visibility against the project's parameter values — a downstream concern
/// for the download builder (`docs/ets5-findings.md`).
pub fn group_objects(
    ap: &ApplicationProgram,
) -> Vec<(u16, GroupObject)> {
    let mut out = Vec::new();

    // Device-level group objects.
    if let Some(static_) = &ap.static_ {
        if let Some(t) = &static_.com_object_table {
            for co in &t.items {
                let asap = co.number.parse::<u16>().unwrap_or(0);
                out.push((asap, group_object_of(co)));
            }
        }
    }

    // Module group objects, instantiated per module instance.
    let modules: HashMap<&str, &ModuleDef> = ap
        .module_defs
        .as_ref()
        .map(|m| m.items.iter().map(|m| (m.id.as_str(), m)).collect())
        .unwrap_or_default();
    if let Some(dynamic) = &ap.dynamic {
        let mut instances = Vec::new();
        collect_module_instances(&dynamic.nodes, &modules, &mut instances);
        for (instance, module) in instances {
            let Some(static_) = &module.static_ else {
                continue;
            };
            let Some(com_objs) = &static_.com_objects else {
                continue;
            };
            for co in &com_objs.items {
                let number = co.number.parse::<u16>().unwrap_or(0);
                // BaseNumber argument default 0 (device-level of the module).
                let base = if co.base_number.is_empty() {
                    0
                } else {
                    instance
                        .arg_value(&co.base_number)
                        .and_then(|v| v.parse::<u16>().ok())
                        .unwrap_or(0)
                };
                out.push((base + number, group_object_of(co)));
            }
        }
    }

    out
}

/// Collect every module instance (instance + its `ModuleDef`) from a `Dynamic`
/// tree, descending into all `when` branches (memory allocation is static).
fn collect_module_instances<'a>(
    nodes: &'a [UiNode],
    modules: &HashMap<&str, &'a ModuleDef>,
    out: &mut Vec<(&'a ModuleInstance, &'a ModuleDef)>,
) {
    for node in nodes {
        match node {
            UiNode::Module(instance) => {
                if let Some(module) = modules.get(instance.ref_id.as_str()) {
                    out.push((instance, module));
                }
            }
            UiNode::Choose(choose) => {
                for when in &choose.whens {
                    collect_module_instances(&when.nodes, modules, out);
                }
            }
            _ => {
                if let Some(children) = node.children() {
                    collect_module_instances(children, modules, out);
                }
            }
        }
    }
}

fn group_object_of(co: &crate::ComObject) -> GroupObject {
    GroupObject {
        // `data_pointer` (the first descriptor octet) is the GO's value
        // address/offset, computed from the knxprod `<Memory>` placements;
        // it is not simply the ASAP. We leave it 0 until the value-area
        // allocation is decoded; the ASAP lives in the tuple returned by
        // `group_objects` and feeds the Association Table.
        data_pointer: 0,
        flags: GroupObjectFlags::from_com_object(co),
        value_field_type: value_field_type_from_size(&co.object_size),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GroupObjectFlags {
    pub transmit: bool,
    pub communicate: bool,
    pub read: bool,
    pub write: bool,
    pub update: bool,
    pub read_on_init: bool,
}

impl GroupObjectFlags {
    /// Build from knxprod `<ComObject>` `*Flag` attributes (values `Enabled`
    /// / `Disabled`, empty = disabled).
    pub fn from_com_object(co: &crate::ComObject) -> Self {
        let on = |s: &str| s == "Enabled";
        Self {
            transmit: on(&co.transmit_flag),
            communicate: on(&co.communication_flag),
            read: on(&co.read_flag),
            write: on(&co.write_flag),
            update: on(&co.update_flag),
            read_on_init: on(&co.read_on_init_flag),
        }
    }
}

/// A group object description (the input to the Group Object Table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupObject {
    /// **Data Pointer** (1 octet): offset of the GO's value MSB in its data
    /// segment (RAM/EEPROM). The segment is chosen by the Segment Selector
    /// bit (bit 5) of `flags` (0→segment 00h, 1→segment 100h). Computed by the
    /// application from the knxprod `<Memory>` placements — see
    /// `docs/ets5-findings.md` (`03_05_01 §4.18.3.1.2`).
    pub data_pointer: u8,
    pub flags: GroupObjectFlags,
    /// Value field type: the object size in bits/octets.
    pub value_field_type: u8,
}

/// The observed 4-octet memory descriptor per group object (mask 0705).
///
/// `[data_pointer][flags][0x00][vft]` — the octet after `flags` (`0x00`) is
/// the low half of the 16-bit `flags`/`config`+`type` pair (see RT1/RT2 in
/// `docs/ets5-findings.md`); `vft` is the value field type.
fn go_descriptor(o: &GroupObject) -> [u8; 4] {
    [
        o.data_pointer,
        go_descriptor_high(o.flags),
        0x00,
        o.value_field_type,
    ]
}

/// Encode the "value field type" code (low octet of the GO descriptor) from
/// an `<ComObject ObjectSize="…">` string, e.g. `"1 Bit"` → 0, `"2 Bytes"` → 8.
///
/// `03_05_01 §4.18.6.2.4.1.4 Table 88`:
///   0..6 → 1..7 bits, then 7 → 1 octet, 8 → 2, 9 → 3, 10 → 4, 11 → 6,
///   12 → 8, 13 → 10, 14 → 14 octets (the remaining sizes are non-standard
///   and not used by `ObjectSize`).
pub fn value_field_type_from_size(size: &str) -> u8 {
    let s = size.trim();
    if let Some(bits) = s.strip_suffix(" Bit") {
        bits.trim().parse::<u8>().map(|n| n - 1).unwrap_or(0)
    } else if let Some(bytes) = s
        .strip_suffix(" Bytes")
        .or_else(|| s.strip_suffix(" Byte"))
    {
        let n: u8 = bytes.trim().parse().unwrap_or(0);
        match n {
            1 => 7,
            2 => 8,
            3 => 9,
            4 => 10,
            6 => 11,
            8 => 12,
            10 => 13,
            14 => 14,
            // Unknown sizes map to 7 (1 octet) by default rather than 0.
            _ => 7,
        }
    } else {
        0
    }
}

/// Build the Group Address Table (GrAT) bytes.
///
/// `gas` are the 16-bit group addresses used by the device, in ascending
/// order. The caller must sort them; we sort defensively here too.
///
/// Confirmed against ETS5 (mask 0705): a **1-octet length** (counted
/// including the individual address), a **2-octet individual address**
/// (`0.0.0` = `00 00`), then the 16-bit GAs (big-endian, ascending). ETS wrote
/// length `04` at 0x4000 and `00 0A 00 0B 00 0C` at 0x4003 for 3 GAs.
pub fn build_group_address_table(mut gas: Vec<u16>) -> Vec<u8> {
    gas.sort_unstable();
    gas.dedup();
    let mut out = Vec::with_capacity(3 + gas.len() * 2);
    // Length = 1 (individual address) + number of GAs.
    out.push((gas.len() + 1) as u8);
    out.extend_from_slice(&[0x00, 0x00]); // individual address 0.0.0
    for ga in gas {
        out.extend_from_slice(&ga.to_be_bytes());
    }
    out
}

/// Build the Association Table (GrOAT format 0) bytes.
///
/// Associations must be sorted by ascending ASAP (group object number); the
/// entries are 2 octets each: `TSAP` (high) then `ASAP` (low).
///
/// The current length is **1 octet** (confirmed against ETS5: `03 01 00 02 06
/// 03 17` for 3 associations).
pub fn build_association_table(mut assocs: Vec<Association>) -> Vec<u8> {
    assocs.sort_by_key(|a| a.asap);
    let mut out = Vec::with_capacity(1 + assocs.len() * 2);
    out.push(assocs.len() as u8);
    for a in assocs {
        out.push(a.tsap as u8);
        out.push(a.asap as u8);
    }
    out
}

/// Build the Group Object Table (Realisation Type 7) bytes.
///
/// One 4-octet descriptor per group object, in ASAP order (index 0 → ASAP 1).
/// Confirmed against ETS5 (mask 0705): **no length prefix** — the segment is
/// exactly `26 × 4 = 104` octets. The memory-mapped format is
/// `[data_pointer][flags][00][vft]` (see `docs/ets5-findings.md`).
pub fn build_group_object_table(objects: &[GroupObject]) -> Vec<u8> {
    let mut out = Vec::with_capacity(objects.len() * 4);
    for o in objects {
        out.extend_from_slice(&go_descriptor(o));
    }
    out
}

/// Overlay the group object **flags** onto a knxprod `ComObjectTable` segment
/// template, keeping the template's `data_pointer` (and trailing octets).
///
/// The authoritative realisation of the mask-0705 GoT: the knxprod template
/// already carries each object's `data_pointer` (value address) in its first
/// octet; ETS only rewrites the **flags** octet (and the end marker).
///
/// `flags_by_record` maps the GoT record index (== ASAP, 0-based) to the
/// realized `GroupObjectFlags`.
pub fn overlay_group_object_table(
    template: &[u8],
    flags_by_record: &std::collections::HashMap<usize, GroupObjectFlags>,
) -> Vec<u8> {
    let mut out = template.to_vec();
    for (idx, flags) in flags_by_record {
        let offset = idx * 4;
        if offset + 1 < out.len() {
            out[offset + 1] = go_descriptor_high(*flags);
        }
    }
    out
}

/// High octet of the memory-mapped GO descriptor — the **Config Octet** of
/// `03_05_01 §4.18.3.1.2`/`§4.18.4.1` (RT1/RT2, used by mask 0705 System B):
///
/// | bit | name | meaning |
/// |----:|------|---------|
/// | 7   | Update enable | set → 1 |
/// | 6   | Transmit enable | set → 1 |
/// | 5   | Segment selector | 0 → segment 00h, 1 → segment 100h |
/// | 4   | Write enable | set → 1 |
/// | 3   | Read enable | set → 1 |
/// | 2   | Communication enable | set → 1 |
/// | 1..0| Transmission priority | 00b system, 01b normal, 10b urgent, 11b low |
///
/// NOTE: this is the *memory* config octet, which differs from the 2-octet
/// property descriptor in Table 87 (where bit 5 = Value Read on init).
fn go_descriptor_high(f: GroupObjectFlags) -> u8 {
    let mut b = 0u8;
    if f.update { b |= 0x80; }
    if f.transmit { b |= 0x40; }
    // segment selector bit 5 = 0 (segment 00h) by default.
    if f.write { b |= 0x10; }
    if f.read { b |= 0x08; }
    if f.communicate { b |= 0x04; }
    // bits 1..0 = transmission priority = 00b (system)
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_address_table_sorted_deduped_with_be_length() {
        // Unsorted + a duplicate → sorted + deduped.
        let bytes = build_group_address_table(vec![0x0119, 0x0019, 0x0019]);
        // 1-octet length (1 + 2 GAs = 3), 2-octet IA 0.0.0, then GAs.
        assert_eq!(&bytes[0..3], &[0x03, 0x00, 0x00]);
        assert_eq!(&bytes[3..5], &[0x00, 0x19]);
        assert_eq!(&bytes[5..7], &[0x01, 0x19]);
        assert_eq!(bytes.len(), 7);
    }

    #[test]
    fn association_table_format0_tsap_high_asap_low() {
        let bytes = build_association_table(vec![
            Association { tsap: 1, asap: 2 },
            Association { tsap: 3, asap: 1 },
        ]);
        // sorted by ASAP: (asap=1, tsap=3) then (asap=2, tsap=1)
        assert_eq!(&bytes[0..1], &[0x02]); // length 2 (1 octet)
        assert_eq!(&bytes[1..3], &[0x03, 0x01]); // tsap 3, asap 1
        assert_eq!(&bytes[3..5], &[0x01, 0x02]); // tsap 1, asap 2
    }

    #[test]
    fn group_object_descriptor_flags() {
        let f = GroupObjectFlags {
            transmit: true,
            communicate: true,
            read: false,
            write: true,
            update: false,
            read_on_init: true,
        };
        // Config octet (RT2): update=0, transmit=1, segment=0, write=1,
        // read=0, comm=1, priority=00 → 0b0101_0100 = 0x54.
        assert_eq!(go_descriptor_high(f), 0x54);
    }

    #[test]
    fn overlay_group_object_table_keeps_data_pointer() {
        // 2 records; record 1 has flags 0xdf in the template.
        let template = [0x19, 0xdf, 0x00, 0x07, 0x1a, 0xdf, 0x00, 0x07];
        let mut map = std::collections::HashMap::new();
        map.insert(1, GroupObjectFlags { communicate: false, ..Default::default() });
        let out = overlay_group_object_table(&template, &map);
        // record 0 unchanged.
        assert_eq!(&out[0..4], &[0x19, 0xdf, 0x00, 0x07]);
        // record 1: data_pointer 0x1a kept, flags rewritten (all off → 0x00).
        assert_eq!(&out[4..8], &[0x1a, 0x00, 0x00, 0x07]);
    }

    #[test]
    fn value_field_type_mapping() {
        assert_eq!(value_field_type_from_size("1 Bit"), 0);
        assert_eq!(value_field_type_from_size("2 Bit"), 1);
        assert_eq!(value_field_type_from_size("1 Byte"), 7);
        assert_eq!(value_field_type_from_size("2 Bytes"), 8);
        assert_eq!(value_field_type_from_size("4 Bytes"), 10);
        assert_eq!(value_field_type_from_size("8 Bytes"), 12);
        assert_eq!(value_field_type_from_size("14 Bytes"), 14);
    }

    #[test]
    fn group_object_table_be_length() {
        let objs = vec![
            GroupObject { data_pointer: 0x19, flags: GroupObjectFlags { communicate: true, ..Default::default() }, value_field_type: 8 },
            GroupObject { data_pointer: 0x1a, flags: GroupObjectFlags::default(), value_field_type: 0 },
        ];
        let bytes = build_group_object_table(&objs);
        // no length prefix; first descriptor: [0x19][0x04][0x00][0x08]
        assert_eq!(&bytes[0..4], &[0x19, 0x04, 0x00, 0x08]);
        // second descriptor: [0x1a][0x00][0x00][0x00]
        assert_eq!(&bytes[4..8], &[0x1a, 0x00, 0x00, 0x00]);
    }
}
