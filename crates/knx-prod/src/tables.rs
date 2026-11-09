//! Group communication tables (M5): Group Address Table (GrAT),
//! Group Object Association Table (GrOAT) and Group Object Table (GoT).
//!
//! Two table families are implemented, selected by the `Flavour` of the
//! mask's table resources in `knx_master.xml` (see [`TableFormat::of`]), named
//! here by the masks they were written for
//! ([`TableFormat`]):
//!
//! **System 7** (BIM M112, masks 0701h/0705h), memory mapped, absolute
//! segments given by the knxprod `<AddressTable>`/`<AssociationTable>`/
//! `<ComObjectTable>` headers — byte-confirmed against ETS5 for mask 0705
//! (`docs/ets5-findings.md`):
//!
//! - GrAT: `[length:1][individual address:2][GA:2]…` — the length counts the
//!   individual address; GAs ascending. TSAP = 1-based index of the GA.
//! - GrOAT: `[length:1]([TSAP:1][ASAP:1])…`; ASAP = `ComObject/@Number`
//!   (0-based in these applications).
//! - GoT (`03_05_01 §4.18.9.1`): `[count:1][RAM-flags table pointer:2]`
//!   followed by 4-octet descriptors `[data pointer:2][config:1][type:1]`.
//!   The knxprod template already holds the pointers (they point into the
//!   application's RAM segment); ETS rewrites config and type octets.
//!
//! **System B** (mask 07B0h), property based, loaded through the table load
//! state machines (object 1/2/3) with `LdCtrlWriteRelMem`:
//!
//! - GrAT Realisation Type 7 (`03_05_01 §4.16.8`): `[length:2][GA:2]…`.
//! - GrOAT Realisation Type 6, format 1 (`§4.17.5`):
//!   `[length:2]([TSAP:2][ASAP:2])…`.
//! - GoT Realisation Type 7 (`§4.18.6`): `[length:2][descriptor:2]…`, one
//!   descriptor per ASAP 1..n: config bits 15..8, value field type 7..0.
//!
//! In both families the device transmits on the **first** association of a
//! group object (`§4.17.5.2.5`), so the sending group address is ordered
//! first among the associations of its object.
//!
//! All multi-octet integers are big-endian.

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail, Result};

use crate::eval::Evaluation;
use crate::master::{MasterData, MaskVersion};
use crate::{ApplicationProgram, ComObject, ComObjectRef};

/// The table realisation family of a mask version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableFormat {
    /// BIM M112 / "System 7": masks 0701h/0705h and those with the same tables.
    System7,
    /// System B: mask 07B0h and those with the same tables.
    SystemB,
    /// A coupler: no group tables, a filter table of one bit per group
    /// address instead (`03_05_01 §5.2.1`, Realisation Type 2, masks 0912h
    /// and 091Ah in their property based configuration).
    Coupler,
}

impl TableFormat {
    /// The format of an application's tables, from the mask in the master data.
    pub fn of_application(ap: &ApplicationProgram, master: &MasterData) -> Result<Self> {
        let mask = master
            .mask_version(&ap.mask_version)
            .ok_or_else(|| anyhow!("mask version {} not found in knx_master.xml", ap.mask_version))?;
        Self::of(mask)
    }

    /// The format that encodes the tables of a mask.
    pub fn of(mask: &MaskVersion) -> Result<Self> {
        let flavours = (
            mask.resource_flavour("GroupAddressTable"),
            mask.resource_flavour("GroupAssociationTable"),
            mask.resource_flavour("GroupObjectTable"),
        );
        match flavours {
            (
                Some("AddressTable_Bcu1"),
                Some("AssociationTable_M112"),
                Some("GroupObjectTable_M112"),
            ) => Ok(TableFormat::System7),
            (
                Some("AddressTable_SystemB"),
                Some("AssociationTable_SystemB"),
                Some("GroupObjectTable_SystemB"),
            ) => Ok(TableFormat::SystemB),
            (Some(address), Some(association), Some(object)) => bail!(
                "mask {} ({}) uses tables knxcfg cannot encode: {address}, {association}, {object}",
                mask.id,
                mask.name
            ),
            _ if mask.resource_length("GroupFilterTable") == Some(FILTER_TABLE_LEN as u32)
                && mask.resource_location("GroupFilterTable") == Some(("LcFilterMemory", 0x0200)) =>
            {
                Ok(TableFormat::Coupler)
            }
            _ if mask.resource_length("GroupFilterTable") == Some(FILTER_TABLE_LEN as u32) => bail!(
                "mask {} ({}) is a coupler whose filter table is not at the fixed address 0200h \
                 (Coupler Model 2.0, Realisation Type 3); knxcfg cannot download it yet",
                mask.id,
                mask.name
            ),
            _ if mask.has_resource("GroupFilterTable") => bail!(
                "mask {} ({}) is a coupler with a filter table of {} octets; knxcfg only \
                 builds the full table of {FILTER_TABLE_LEN} octets (masks 0912h, 091Ah)",
                mask.id,
                mask.name,
                mask.resource_length("GroupFilterTable").unwrap_or(0)
            ),
            _ => bail!("mask {} ({}) has no group tables that knxcfg can use", mask.id, mask.name),
        }
    }
}

/// The filter table of a coupler: one bit for each of the 65536 group
/// addresses (`03_05_01 §5.2.1.3`).
pub const FILTER_TABLE_LEN: usize = 8192;

/// Build a coupler's filter table: the bit of a group address is set when
/// telegrams to it are passed. Bit `ga mod 8` (LSB first) of octet `ga div 8`;
/// group address 0 (broadcast) is never set.
pub fn build_filter_table(gas: impl IntoIterator<Item = u16>) -> Vec<u8> {
    let mut table = vec![0u8; FILTER_TABLE_LEN];
    for ga in gas.into_iter().filter(|&ga| ga != 0) {
        table[ga as usize / 8] |= 1 << (ga % 8);
    }
    table
}

/// The group addresses set in a filter table.
pub fn read_filter_table(table: &[u8]) -> Vec<u16> {
    (1..=u16::MAX)
        .filter(|&ga| table.get(ga as usize / 8).is_some_and(|b| b & (1 << (ga % 8)) != 0))
        .collect()
}

/// One entry of the Association Table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Association {
    /// 1-based index into the Group Address Table.
    pub tsap: u16,
    /// Group object number.
    pub asap: u16,
}

/// A project link: group object → group address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link {
    pub asap: u16,
    /// Raw 16-bit group address.
    pub ga: u16,
    /// The group address the object transmits on.
    pub send: bool,
}

impl Link {
    pub fn new(asap: u16, ga: u16) -> Self {
        Self { asap, ga, send: false }
    }
}

/// Table capacities from the knxprod headers (`MaxEntries`).
#[derive(Debug, Clone, Copy, Default)]
pub struct TableLimits {
    pub address: Option<usize>,
    pub association: Option<usize>,
}

impl TableLimits {
    pub fn of(ap: &ApplicationProgram) -> Self {
        let s = ap.static_.as_ref();
        let max = |t: Option<&crate::Table>| t.and_then(|t| t.max_entries.parse().ok());
        Self {
            address: max(s.and_then(|s| s.address_table.as_ref())),
            association: max(s.and_then(|s| s.association_table.as_ref())),
        }
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
    /// From the knxprod `*Flag` attributes (`Enabled`/`Disabled`).
    pub fn from_com_object(co: &ComObject) -> Self {
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

/// A resolved group object (base `ComObject` with its active `ComObjectRef`
/// overrides applied).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupObject {
    pub flags: GroupObjectFlags,
    /// Value field type (`03_05_01 Table 88`).
    pub value_field_type: u8,
    /// Transmission priority bits (00 system, 01 normal, 10 urgent, 11 low),
    /// if the knxprod states one.
    pub priority: Option<u8>,
    pub text: String,
    pub function_text: String,
    pub datapoint_type: String,
    pub object_size: String,
}

impl GroupObject {
    fn of(co: &ComObject) -> Self {
        Self {
            flags: GroupObjectFlags::from_com_object(co),
            value_field_type: value_field_type_from_size(&co.object_size),
            priority: priority_bits(&co.priority),
            text: co.text.clone(),
            function_text: co.function_text.clone(),
            datapoint_type: co.datapoint_type.clone(),
            object_size: co.object_size.clone(),
        }
    }
}

/// knxprod `Priority` → transmission priority bits.
pub fn priority_bits(p: &str) -> Option<u8> {
    match p {
        "Low" => Some(0b11),
        "High" => Some(0b01),
        "Alert" => Some(0b10),
        _ => None,
    }
}

/// Every group object the application can expose (all module instances, all
/// channel modes), by ASAP. Mutually exclusive modes share ASAPs; the first
/// definition is kept. Used to validate links.
pub fn group_objects(ap: &ApplicationProgram) -> Vec<(u16, GroupObject)> {
    let ev = Evaluation::new(ap, &HashMap::new());
    let mut out = Vec::new();
    for (si, scope) in ev.scopes.iter().enumerate() {
        for co in scope_com_objects(scope.static_) {
            out.push((asap_of(&ev, si, co), GroupObject::of(co)));
        }
    }
    out.sort_by_key(|(a, _)| *a);
    out.dedup_by_key(|(a, _)| *a);
    out
}

/// The group objects that are active for the given parameter values.
pub fn active_group_objects(
    ap: &ApplicationProgram,
    values: &HashMap<String, String>,
) -> Vec<(u16, GroupObject)> {
    active_group_objects_eval(&Evaluation::new(ap, values)).0
}

/// Active group objects of an evaluation, sorted by ASAP, plus conflicts
/// (two active objects with the same ASAP).
///
/// A scope (device or module instance) that defines `ComObjectRef`s exposes
/// exactly the objects whose `ComObjectRefRef` is reached through active
/// branches. A scope without any `ComObjectRef` (legacy layout) exposes all
/// of its objects while the scope itself is active.
pub fn active_group_objects_eval(ev: &Evaluation) -> (Vec<(u16, GroupObject)>, Vec<String>) {
    let mut out: Vec<(u16, GroupObject)> = Vec::new();
    let mut seen: HashMap<u16, String> = HashMap::new();
    let mut conflicts = Vec::new();
    let mut push = |asap: u16, obj: GroupObject, id: &str, out: &mut Vec<(u16, GroupObject)>| {
        if let Some(other) = seen.get(&asap) {
            if other != id {
                conflicts.push(format!(
                    "group objects {other} and {id} are both active with number {asap}"
                ));
            }
            return;
        }
        seen.insert(asap, id.to_string());
        out.push((asap, obj));
    };

    let mut with_refs = HashSet::new();
    for (si, scope) in ev.scopes.iter().enumerate() {
        let has_refs = scope
            .static_
            .and_then(|s| s.com_object_refs.as_ref())
            .is_some_and(|r| !r.items.is_empty());
        if has_refs {
            with_refs.insert(si);
        } else if ev.is_scope_active(si) {
            for co in scope_com_objects(scope.static_) {
                push(asap_of(ev, si, co), GroupObject::of(co), &co.id, &mut out);
            }
        }
    }
    for (si, com_ref) in ev.active_com_object_refs() {
        if !with_refs.contains(&si) {
            continue;
        }
        let Some(co) = scope_com_objects(ev.scopes[si].static_).find(|c| c.id == com_ref.ref_id)
        else {
            continue;
        };
        let merged = merged_com_object(co, com_ref);
        let key = format!("{}/{}", ev.scopes[si].prefix, co.id);
        push(asap_of(ev, si, &merged), GroupObject::of(&merged), &key, &mut out);
    }
    out.sort_by_key(|(a, _)| *a);
    (out, conflicts)
}

fn scope_com_objects(static_: Option<&crate::Static>) -> impl Iterator<Item = &ComObject> {
    static_.into_iter().flat_map(|s| {
        s.com_object_table
            .iter()
            .flat_map(|t| t.items.iter())
            .chain(s.com_objects.iter().flat_map(|c| c.items.iter()))
    })
}

/// ASAP of an object: `Number`, plus the instance's `BaseNumber` argument for
/// module objects.
fn asap_of(ev: &Evaluation, scope: usize, co: &ComObject) -> u16 {
    let number = co.number.trim().parse::<u16>().unwrap_or(0);
    let base = if co.base_number.is_empty() {
        0
    } else {
        ev.scopes[scope]
            .arg_value(&co.base_number)
            .and_then(|v| v.trim().parse::<u16>().ok())
            .unwrap_or(0)
    };
    base + number
}

/// ASAP and resolved object of a `ComObjectRef` in a scope, whether or not it
/// is active.
pub fn resolve_com_object_ref(
    ev: &Evaluation,
    scope: usize,
    com_ref: &ComObjectRef,
) -> Option<(u16, GroupObject)> {
    let co = scope_com_objects(ev.scopes[scope].static_).find(|c| c.id == com_ref.ref_id)?;
    let merged = merged_com_object(co, com_ref);
    Some((asap_of(ev, scope, &merged), GroupObject::of(&merged)))
}

/// Merge a `ComObjectRef`'s non-empty overrides onto its base `ComObject`.
fn merged_com_object(co: &ComObject, r: &ComObjectRef) -> ComObject {
    let mut m = co.clone();
    let set = |dst: &mut String, src: &String| {
        if !src.is_empty() {
            *dst = src.clone();
        }
    };
    set(&mut m.object_size, &r.object_size);
    set(&mut m.read_flag, &r.read_flag);
    set(&mut m.write_flag, &r.write_flag);
    set(&mut m.communication_flag, &r.communication_flag);
    set(&mut m.transmit_flag, &r.transmit_flag);
    set(&mut m.update_flag, &r.update_flag);
    set(&mut m.read_on_init_flag, &r.read_on_init_flag);
    set(&mut m.priority, &r.priority);
    set(&mut m.text, &r.text);
    set(&mut m.function_text, &r.function_text);
    set(&mut m.datapoint_type, &r.datapoint_type);
    m
}

/// Value field type code from an `ObjectSize` (`"1 Bit"` → 0, `"2 Bytes"` →
/// 8; `03_05_01 Table 88`). Unknown sizes map to 7 (1 octet).
pub fn value_field_type_from_size(size: &str) -> u8 {
    let s = size.trim();
    if let Some(bits) = s.strip_suffix(" Bit") {
        bits.trim().parse::<u8>().map(|n| n.saturating_sub(1)).unwrap_or(0)
    } else if let Some(bytes) = s.strip_suffix(" Bytes").or_else(|| s.strip_suffix(" Byte")) {
        match bytes.trim().parse::<u16>().unwrap_or(0) {
            1 => 7,
            2 => 8,
            3 => 9,
            4 => 10,
            6 => 11,
            8 => 12,
            10 => 13,
            14 => 14,
            5 => 15,
            7 => 16,
            9 => 17,
            n @ 11..=13 => (n + 7) as u8,
            n @ 15..=252 => (n + 6) as u8,
            _ => 7,
        }
    } else {
        0
    }
}

/// Order links into associations: by ASAP, the sending GA first, then by
/// TSAP. Returns the distinct GAs (ascending, TSAP order) and the
/// associations.
fn associations(links: &[Link]) -> Result<(Vec<u16>, Vec<Association>)> {
    let mut gas: Vec<u16> = links.iter().map(|l| l.ga).collect();
    gas.sort_unstable();
    gas.dedup();
    let tsap_of: HashMap<u16, u16> = gas
        .iter()
        .enumerate()
        .map(|(i, &ga)| (ga, i as u16 + 1))
        .collect();

    let mut senders: HashMap<u16, u16> = HashMap::new();
    for l in links.iter().filter(|l| l.send) {
        if let Some(prev) = senders.insert(l.asap, l.ga) {
            if prev != l.ga {
                bail!("group object {} has more than one sending group address", l.asap);
            }
        }
    }
    let mut assocs: Vec<(Association, bool)> = links
        .iter()
        .map(|l| {
            let a = Association { tsap: tsap_of[&l.ga], asap: l.asap };
            (a, senders.get(&l.asap) == Some(&l.ga))
        })
        .collect();
    assocs.sort_by_key(|(a, send)| (a.asap, !send, a.tsap));
    assocs.dedup_by_key(|(a, _)| *a);
    Ok((gas, assocs.into_iter().map(|(a, _)| a).collect()))
}

/// Build the Group Address Table and the Association Table.
pub fn build_address_and_association_tables(
    format: TableFormat,
    links: &[Link],
    limits: TableLimits,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let (gas, assocs) = associations(links)?;
    if let Some(max) = limits.address {
        if gas.len() > max {
            bail!(
                "{} group addresses, the device's address table holds {max}",
                gas.len()
            );
        }
    }
    if let Some(max) = limits.association {
        if assocs.len() > max {
            bail!(
                "{} associations, the device's association table holds {max}",
                assocs.len()
            );
        }
    }
    Ok(match format {
        TableFormat::System7 => (
            build_group_address_table(&gas)?,
            build_association_table(&assocs)?,
        ),
        TableFormat::SystemB => (
            build_group_address_table_b(&gas)?,
            build_association_table_b(&assocs)?,
        ),
        TableFormat::Coupler => bail!("a coupler has no group address and association tables"),
    })
}

/// System 7 GrAT: 1-octet length (counting the individual address), the
/// individual address (ETS writes `00 00`), then the GAs ascending.
pub fn build_group_address_table(gas: &[u16]) -> Result<Vec<u8>> {
    if gas.len() + 1 > 255 {
        bail!("{} group addresses do not fit a 1-octet table length", gas.len());
    }
    let mut out = Vec::with_capacity(3 + gas.len() * 2);
    out.push((gas.len() + 1) as u8);
    out.extend_from_slice(&[0x00, 0x00]);
    for ga in gas {
        out.extend_from_slice(&ga.to_be_bytes());
    }
    Ok(out)
}

/// System 7 GrOAT: 1-octet length, then `[TSAP:1][ASAP:1]` entries.
pub fn build_association_table(assocs: &[Association]) -> Result<Vec<u8>> {
    if assocs.len() > 255 {
        bail!("{} associations do not fit a 1-octet table length", assocs.len());
    }
    let mut out = Vec::with_capacity(1 + assocs.len() * 2);
    out.push(assocs.len() as u8);
    for a in assocs {
        if a.tsap > 255 || a.asap > 255 {
            bail!(
                "association TSAP {} / group object {} does not fit one octet",
                a.tsap,
                a.asap
            );
        }
        out.push(a.tsap as u8);
        out.push(a.asap as u8);
    }
    Ok(out)
}

/// System B GrAT (RT 7): 2-octet length, then the GAs ascending.
pub fn build_group_address_table_b(gas: &[u16]) -> Result<Vec<u8>> {
    if gas.len() > u16::MAX as usize {
        bail!("too many group addresses");
    }
    let mut out = Vec::with_capacity(2 + gas.len() * 2);
    out.extend_from_slice(&(gas.len() as u16).to_be_bytes());
    for ga in gas {
        out.extend_from_slice(&ga.to_be_bytes());
    }
    Ok(out)
}

/// System B GrOAT (RT 6, format 1): 2-octet length, then
/// `[TSAP:2][ASAP:2]` entries.
pub fn build_association_table_b(assocs: &[Association]) -> Result<Vec<u8>> {
    if assocs.len() > u16::MAX as usize {
        bail!("too many associations");
    }
    let mut out = Vec::with_capacity(2 + assocs.len() * 4);
    out.extend_from_slice(&(assocs.len() as u16).to_be_bytes());
    for a in assocs {
        out.extend_from_slice(&a.tsap.to_be_bytes());
        out.extend_from_slice(&a.asap.to_be_bytes());
    }
    Ok(out)
}

/// Read a GrAT back into its GAs (TSAP order). Trailing bytes are ignored.
pub fn read_group_address_table(format: TableFormat, data: &[u8]) -> Result<Vec<u16>> {
    let (count, start) = match format {
        TableFormat::System7 => {
            let Some(&len) = data.first() else { bail!("empty Group Address Table") };
            if len == 0 {
                bail!("Group Address Table length is zero");
            }
            (len as usize - 1, 3)
        }
        TableFormat::SystemB => {
            if data.len() < 2 {
                bail!("empty Group Address Table");
            }
            (u16::from_be_bytes([data[0], data[1]]) as usize, 2)
        }
        TableFormat::Coupler => bail!("a coupler has no group address table"),
    };
    let needed = start + count * 2;
    if data.len() < needed {
        bail!("Group Address Table truncated: need {needed} bytes, have {}", data.len());
    }
    Ok((0..count)
        .map(|i| u16::from_be_bytes([data[start + i * 2], data[start + i * 2 + 1]]))
        .collect())
}

/// Read an Association Table back. Trailing bytes are ignored.
pub fn read_association_table(format: TableFormat, data: &[u8]) -> Result<Vec<Association>> {
    let (count, start, width) = match format {
        TableFormat::System7 => {
            let Some(&len) = data.first() else { bail!("empty Association Table") };
            (len as usize, 1, 1)
        }
        TableFormat::SystemB => {
            if data.len() < 2 {
                bail!("empty Association Table");
            }
            (u16::from_be_bytes([data[0], data[1]]) as usize, 2, 2)
        }
        TableFormat::Coupler => bail!("a coupler has no association table"),
    };
    let needed = start + count * 2 * width;
    if data.len() < needed {
        bail!("Association Table truncated: need {needed} bytes, have {}", data.len());
    }
    let field = |off: usize| -> u16 {
        if width == 1 {
            data[off] as u16
        } else {
            u16::from_be_bytes([data[off], data[off + 1]])
        }
    };
    Ok((0..count)
        .map(|i| {
            let off = start + i * 2 * width;
            Association { tsap: field(off), asap: field(off + width) }
        })
        .collect())
}

/// Decode links from a GrAT and a GrOAT; the first association of each
/// object is its sending address.
pub fn read_links(format: TableFormat, address: &[u8], association: &[u8]) -> Result<Vec<Link>> {
    let gas = read_group_address_table(format, address)?;
    let assocs = read_association_table(format, association)?;
    let mut links = Vec::with_capacity(assocs.len());
    let mut has_sender = HashSet::new();
    for a in assocs {
        if a.tsap == 0 {
            continue;
        }
        let Some(&ga) = gas.get((a.tsap - 1) as usize) else {
            bail!("Association TSAP {} out of range", a.tsap);
        };
        links.push(Link { asap: a.asap, ga, send: has_sender.insert(a.asap) });
    }
    links.sort_by_key(|l| (l.asap, !l.send, l.ga));
    Ok(links)
}

/// GoT config octet flag bits (`03_05_01 §4.18.4.1`, RT 2 / System 7):
///
/// | bit | meaning |
/// |----:|---------|
/// | 7   | update enable |
/// | 6   | transmit enable |
/// | 5   | segment selector (0 = segment 00h) |
/// | 4   | write enable |
/// | 3   | read enable |
/// | 2   | communication enable |
/// | 1..0 | transmission priority (not included here) |
///
/// System 7 has no "read on init" flag (ETS greys it out).
pub fn go_descriptor_high(f: GroupObjectFlags) -> u8 {
    let mut b = 0u8;
    if f.update {
        b |= 0x80;
    }
    if f.transmit {
        b |= 0x40;
    }
    if f.write {
        b |= 0x10;
    }
    if f.read {
        b |= 0x08;
    }
    if f.communicate {
        b |= 0x04;
    }
    b
}

/// Communication enable bit of the config octet.
pub const COMMUNICATION_FLAG: u8 = 0x04;

/// Transmission priority bits of the config octet.
pub const PRIORITY_MASK: u8 = 0x03;

/// Final config octet of an active System 7 group object. The priority is
/// the knxprod `Priority` if given, else the template's. An object without
/// any association cannot communicate, so its communication bit is cleared
/// (observed ETS behaviour).
pub fn visible_group_object_flags(obj: &GroupObject, template_config: u8, linked: bool) -> u8 {
    let prio = obj.priority.unwrap_or(template_config & PRIORITY_MASK);
    let mut b = go_descriptor_high(obj.flags) | prio;
    if !linked {
        b &= !COMMUNICATION_FLAG;
    }
    b
}

/// Offsets of a System 7 GoT template (`§4.18.9.1`).
const GOT7_HEADER: usize = 3;
const GOT7_DESCRIPTOR: usize = 4;

/// Number of descriptors in a System 7 GoT template (its count octet).
pub fn group_object_table_entry_count(template: &[u8], base_offset: usize) -> Option<usize> {
    template.get(base_offset).map(|n| *n as usize)
}

/// Offset of the config octet of descriptor `asap` in a System 7 GoT.
pub fn got7_config_offset(base_offset: usize, asap: u16) -> usize {
    base_offset + GOT7_HEADER + asap as usize * GOT7_DESCRIPTOR + 2
}

/// Overlay a System 7 GoT template: active objects get their config and
/// type octets; inactive descriptors keep the template config with the
/// communication bit cleared. Data pointers and the RAM-flags pointer are
/// kept.
pub fn overlay_group_object_table(
    template: &[u8],
    base_offset: usize,
    active: &[(u16, GroupObject)],
    linked: &HashSet<u16>,
) -> Result<Vec<u8>> {
    let mut out = template.to_vec();
    let Some(count) = group_object_table_entry_count(template, base_offset) else {
        bail!("Group Object Table template is empty");
    };
    let end = base_offset + GOT7_HEADER + count * GOT7_DESCRIPTOR;
    if end > out.len() {
        bail!(
            "Group Object Table template declares {count} objects but holds only {} bytes",
            out.len()
        );
    }
    let by_asap: HashMap<u16, &GroupObject> = active.iter().map(|(a, o)| (*a, o)).collect();
    for (asap, _) in active {
        if *asap as usize >= count {
            bail!("group object {asap} is active but the device table has only {count} slots");
        }
    }
    for asap in 0..count as u16 {
        let cfg = got7_config_offset(base_offset, asap);
        out[cfg] = match by_asap.get(&asap) {
            Some(obj) => {
                out[cfg + 1] = obj.value_field_type;
                visible_group_object_flags(obj, out[cfg], linked.contains(&asap))
            }
            None => out[cfg] & !COMMUNICATION_FLAG,
        };
    }
    Ok(out)
}

/// Build a System B GoT (RT 7): one 2-octet descriptor per ASAP 1..max.
/// Numbers without an active object get an all-zero descriptor
/// (communication disabled). Unlinked objects get communication disabled,
/// as for System 7 (not verified against ETS for System B).
pub fn build_group_object_table_b(
    active: &[(u16, GroupObject)],
    linked: &HashSet<u16>,
) -> Result<Vec<u8>> {
    if active.iter().any(|(a, _)| *a == 0) {
        bail!("System B group object numbers start at 1, found object 0");
    }
    let max = active.iter().map(|(a, _)| *a).max().unwrap_or(0);
    let by_asap: HashMap<u16, &GroupObject> = active.iter().map(|(a, o)| (*a, o)).collect();
    let mut out = Vec::with_capacity(2 + max as usize * 2);
    out.extend_from_slice(&max.to_be_bytes());
    for asap in 1..=max {
        let (cfg, vft) = match by_asap.get(&asap) {
            Some(obj) => {
                let f = obj.flags;
                let mut c = go_descriptor_high(f);
                if f.read_on_init {
                    c |= 0x20;
                }
                if !linked.contains(&asap) {
                    c &= !COMMUNICATION_FLAG;
                }
                c |= obj.priority.unwrap_or(0b11);
                (c, obj.value_field_type)
            }
            None => (0, 0),
        };
        out.push(cfg);
        out.push(vft);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {

    /// The examples of `03_05_01`: 7/1879 (3F57h) is bit 7 of octet 07EAh
    /// (§5.3.1.1.1), 0/232 (E8h) bit 0 of octet 1Dh, at 021Dh in the
    /// coupler's memory (§5.2.1.3).
    #[test]
    fn filter_table_bits_follow_the_spec_examples() {
        let t = build_filter_table([0x3F57, 0x00E8, 0]);
        assert_eq!(t.len(), FILTER_TABLE_LEN);
        assert_eq!(t[0x07EA], 0x80);
        assert_eq!(t[0x1D], 0x01);
        assert_eq!(t[0], 0, "group address 0 is never set");
        assert_eq!(t.iter().filter(|b| **b != 0).count(), 2);
        assert_eq!(read_filter_table(&t), vec![0x00E8, 0x3F57]);
    }

    use super::*;

    fn obj(comm: bool) -> GroupObject {
        GroupObject {
            flags: GroupObjectFlags { communicate: comm, transmit: true, ..Default::default() },
            value_field_type: 0,
            priority: None,
            text: String::new(),
            function_text: String::new(),
            datapoint_type: String::new(),
            object_size: "1 Bit".into(),
        }
    }

    #[test]
    fn system7_address_and_association_tables() {
        let links = [Link::new(0, 0x000A), Link::new(6, 0x000B), Link::new(23, 0x000C)];
        let (a, b) =
            build_address_and_association_tables(TableFormat::System7, &links, TableLimits::default())
                .unwrap();
        // Byte-confirmed against ETS5 (docs/ets5-findings.md).
        assert_eq!(a, [0x04, 0x00, 0x00, 0x00, 0x0A, 0x00, 0x0B, 0x00, 0x0C]);
        assert_eq!(b, [0x03, 0x01, 0x00, 0x02, 0x06, 0x03, 0x17]);
        let back = read_links(TableFormat::System7, &a, &b).unwrap();
        assert_eq!(
            back.iter().map(|l| (l.asap, l.ga)).collect::<Vec<_>>(),
            vec![(0, 0x0A), (6, 0x0B), (23, 0x0C)]
        );
    }

    #[test]
    fn sending_address_comes_first() {
        let links = [
            Link::new(1, 0x0001),
            Link { asap: 1, ga: 0x0005, send: true },
            Link::new(2, 0x0001),
        ];
        let (a, b) =
            build_address_and_association_tables(TableFormat::System7, &links, TableLimits::default())
                .unwrap();
        // TSAP 1 = 0x0001, TSAP 2 = 0x0005; object 1 sends on TSAP 2.
        assert_eq!(b, [0x03, 0x02, 0x01, 0x01, 0x01, 0x01, 0x02]);
        let back = read_links(TableFormat::System7, &a, &b).unwrap();
        assert!(back.contains(&Link { asap: 1, ga: 0x0005, send: true }));
        assert!(back.contains(&Link { asap: 1, ga: 0x0001, send: false }));
    }

    #[test]
    fn limits_are_enforced() {
        let links: Vec<Link> = (0..300).map(|i| Link::new(0, i)).collect();
        assert!(build_address_and_association_tables(
            TableFormat::System7,
            &links,
            TableLimits::default()
        )
        .is_err());
        let links = [Link::new(0, 1), Link::new(0, 2)];
        let limits = TableLimits { address: Some(1), association: None };
        assert!(build_address_and_association_tables(TableFormat::SystemB, &links, limits).is_err());
        assert!(build_association_table(&[Association { tsap: 1, asap: 300 }]).is_err());
    }

    #[test]
    fn system_b_tables() {
        let links = [Link::new(1, 0x0A03), Link::new(2, 0x0A03)];
        let (a, b) =
            build_address_and_association_tables(TableFormat::SystemB, &links, TableLimits::default())
                .unwrap();
        assert_eq!(a, [0x00, 0x01, 0x0A, 0x03]);
        assert_eq!(b, [0x00, 0x02, 0x00, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00, 0x02]);
        assert_eq!(read_links(TableFormat::SystemB, &a, &b).unwrap().len(), 2);

        let linked: HashSet<u16> = [1].into_iter().collect();
        let got = build_group_object_table_b(&[(1, obj(true)), (3, obj(true))], &linked).unwrap();
        // length 3; ASAP 1 linked: T+C+low prio; ASAP 2 unused; ASAP 3 unlinked.
        assert_eq!(got, [0x00, 0x03, 0x47, 0x00, 0x00, 0x00, 0x43, 0x00]);
        assert!(build_group_object_table_b(&[(0, obj(true))], &linked).is_err());
    }

    #[test]
    fn system7_got_overlay_follows_spec_layout() {
        // count 2, RAM-flags pointer 0x0700, descriptors [ptr:2][cfg][type].
        let template = [0x02, 0x07, 0x00, 0x07, 0x1A, 0xDF, 0x00, 0x07, 0x22, 0xDF, 0x00, 0x00];
        let mut o = obj(true);
        o.value_field_type = 7;
        let linked: HashSet<u16> = [0].into_iter().collect();
        let out = overlay_group_object_table(&template, 0, &[(0, o)], &linked).unwrap();
        assert_eq!(&out[0..3], &[0x02, 0x07, 0x00]);
        // ASAP 0: pointer kept, config T+C + template priority 11, type 7.
        assert_eq!(&out[3..7], &[0x07, 0x1A, 0x47, 0x07]);
        // ASAP 1: inactive → template config without communication.
        assert_eq!(&out[7..11], &[0x07, 0x22, 0xDB, 0x00]);
        assert_eq!(out[11], 0x00);
    }

    #[test]
    fn value_field_type_mapping() {
        assert_eq!(value_field_type_from_size("1 Bit"), 0);
        assert_eq!(value_field_type_from_size("2 Bit"), 1);
        assert_eq!(value_field_type_from_size("1 Byte"), 7);
        assert_eq!(value_field_type_from_size("2 Bytes"), 8);
        assert_eq!(value_field_type_from_size("4 Bytes"), 10);
        assert_eq!(value_field_type_from_size("14 Bytes"), 14);
        assert_eq!(value_field_type_from_size("5 Bytes"), 15);
        assert_eq!(value_field_type_from_size("16 Bytes"), 22);
    }

    #[test]
    fn active_group_objects_respects_choose() {
        let device_static = crate::Static {
            parameters: Some(crate::Parameters {
                nodes: vec![crate::ParamNode::Parameter(crate::Parameter {
                    id: "P-1".into(),
                    value: "0".into(),
                    ..Default::default()
                })],
            }),
            parameter_refs: Some(crate::ParameterRefs {
                items: vec![crate::ParameterRef {
                    id: "PR-1".into(),
                    ref_id: "P-1".into(),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        };
        let module = |id: &str| crate::ModuleDef {
            id: id.into(),
            static_: Some(crate::Static {
                com_objects: Some(crate::ComObjects {
                    items: vec![ComObject {
                        id: format!("{id}_O-0"),
                        number: "0".into(),
                        ..Default::default()
                    }],
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let instance = |id: &str, md: &str| {
            crate::UiNode::Module(crate::ModuleInstance {
                id: id.into(),
                ref_id: md.into(),
                ..Default::default()
            })
        };
        let ap = crate::ApplicationProgram {
            static_: Some(device_static),
            dynamic: Some(crate::Dynamic {
                nodes: vec![crate::UiNode::Choose(crate::Choose {
                    param_ref_id: "PR-1".into(),
                    whens: vec![
                        crate::When {
                            test: "0".into(),
                            nodes: vec![instance("M-A", "MD-A")],
                            ..Default::default()
                        },
                        crate::When {
                            default: "true".into(),
                            nodes: vec![instance("M-B", "MD-B")],
                            ..Default::default()
                        },
                    ],
                })],
            }),
            module_defs: Some(crate::ModuleDefs { items: vec![module("MD-A"), module("MD-B")] }),
            ..Default::default()
        };

        let mut values = HashMap::new();
        values.insert("P-1".to_string(), "0".to_string());
        let ev = Evaluation::new(&ap, &values);
        let (active, conflicts) = active_group_objects_eval(&ev);
        assert_eq!(active.len(), 1);
        assert!(conflicts.is_empty());
        // Both modes exist in the full enumeration (same ASAP, kept once).
        assert_eq!(group_objects(&ap).len(), 1);
    }
}
