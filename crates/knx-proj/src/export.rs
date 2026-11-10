//! ETS project (`.knxproj`) export: typed model → project file (M3).
//!
//! The archive holds what ETS 5.7 writes (see `docs/knxproj-format.md`):
//! `P-XXXX/project.xml` and `P-XXXX/0.xml` built from the model, the product
//! data of the devices (`M-XXXX/`, `knx_master.xml`) copied from their
//! knxprods, and a `<dir>.signature` for every directory. ETS imports a
//! project only if the signatures are valid; without a signing key the
//! archive is written unsigned.
//!
//! Like ETS, the export writes only what differs from the product: parameter
//! values the model stores, objects with links or own settings.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use knx_core::{
    Area, DeviceInstance, Line, GroupAddress, GroupAddressStyle, GroupRange, IndividualAddress, Medium, Project, Space,
};
use knx_prod::eval::{full_key, short_key};
use knx_prod::tables;
use knx_prod::{ApplicationProgram, Evaluation};
use rsa::pkcs8::DecodePrivateKey;
use rsa::{Pkcs1v15Sign, RsaPrivateKey};
use sha1::{Digest, Sha1};

use crate::merge;

/// A device to export with its product data.
pub struct ExportDevice<'a> {
    pub ia: IndividualAddress,
    pub ap: &'a ApplicationProgram,
    /// The knxprod holding the device's product data.
    pub knxprod: PathBuf,
    /// `ProductRefId` of the device.
    pub product: String,
    /// The product's name: a device named like its product gets no name of its
    /// own in the project, as in ETS.
    pub product_name: String,
}

/// The key ETS checks project signatures with (RSA, PKCS#8 PEM).
pub fn read_signing_key(path: &Path) -> Result<RsaPrivateKey> {
    let pem = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    RsaPrivateKey::from_pkcs8_pem(&pem).with_context(|| format!("{} is no PKCS#8 RSA key", path.display()))
}

/// Write `project` to `dest`. Returns warnings about what was not exported.
pub fn export(
    project: &Project,
    devices: &[ExportDevice],
    key: Option<&RsaPrivateKey>,
    dest: &Path,
) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    let id = project_id(&project.meta.name);
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();

    let (installation, last_puid) = Writer::new(&id).installation(project, devices, &mut warnings)?;
    files.insert(format!("{id}/0.xml"), installation.into_bytes());
    files.insert(format!("{id}/project.xml"), project_xml(project, &id, last_puid).into_bytes());

    product_files(devices, &mut files)?;

    let mut dirs: BTreeSet<String> = BTreeSet::new();
    for name in files.keys() {
        if let Some((dir, _)) = name.split_once('/') {
            dirs.insert(dir.to_string());
        }
    }
    match key {
        Some(key) => {
            for dir in &dirs {
                let signature = sign_directory(&files, dir, key)?;
                files.insert(format!("{dir}.signature"), signature.into_bytes());
            }
        }
        None => warnings.push("no signing key: ETS does not import an unsigned project".to_string()),
    }

    let file = File::create(dest).with_context(|| format!("creating {}", dest.display()))?;
    let mut out = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default());
    for (name, data) in &files {
        out.start_file(name.as_str(), options)?;
        out.write_all(data)?;
    }
    out.finish()?;
    Ok(warnings)
}

/// `P-XXXX` from the project name, so that exporting again keeps the id.
fn project_id(name: &str) -> String {
    let h = Sha1::digest(name.as_bytes());
    format!("P-{:04X}", u16::from_be_bytes([h[0], h[1]]) & 0x7FFF | 0x0100)
}

/// A GUID derived from the project name, in the form ETS writes.
fn project_guid(name: &str) -> String {
    let h = Sha1::digest(format!("knxcfg:{name}").as_bytes());
    let hex: String = h.iter().take(16).map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// The signature ETS checks for directory `dir` of the archive: an RSA
/// PKCS#1 v1.5 SHA-1 signature over the SHA-1 of the file list
/// `name:base64(sha1(content))`, sorted by the name relative to the
/// directory (with `\` separators) and joined with `,`.
fn sign_directory(files: &BTreeMap<String, Vec<u8>>, dir: &str, key: &RsaPrivateKey) -> Result<String> {
    use base64::Engine;
    let digest = directory_digest(files, dir);
    let signature = key.sign(Pkcs1v15Sign::new::<Sha1>(), &digest).context("signing")?;
    Ok(base64::engine::general_purpose::STANDARD.encode(signature))
}

/// The SHA-1 that [`sign_directory`] signs.
fn directory_digest(files: &BTreeMap<String, Vec<u8>>, dir: &str) -> Vec<u8> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let prefix = format!("{dir}/");
    let mut entries: Vec<(String, String)> = files
        .iter()
        .filter_map(|(name, data)| {
            let rel = name.strip_prefix(&prefix)?;
            Some((rel.replace('/', "\\"), b64.encode(Sha1::digest(data))))
        })
        .collect();
    entries.sort();
    let manifest: Vec<String> = entries.iter().map(|(rel, h)| format!("{rel}:{h}")).collect();
    Sha1::digest(manifest.join(",").as_bytes()).to_vec()
}

fn project_xml(project: &Project, id: &str, last_puid: u32) -> String {
    let style = match project.meta.group_address_style {
        Some(GroupAddressStyle::TwoLevel) => "TwoLevel",
        Some(GroupAddressStyle::Free) => "Free",
        _ => "ThreeLevel",
    };
    let mut s = String::from(HEADER);
    let _ = write!(
        s,
        "  <Project Id=\"{id}\">\r\n    <ProjectInformation Name=\"{}\" GroupAddressStyle=\"{style}\" Comment=\"{}\" \
         CompletionStatus=\"Editing\" LastUsedPuid=\"{last_puid}\" Guid=\"{}\" />\r\n  </Project>\r\n</KNX>",
        attr(&project.meta.name),
        attr(project.meta.comment.as_deref().unwrap_or("")),
        project_guid(&project.meta.name),
    );
    s
}

const HEADER: &str = "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n<KNX xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
     xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\" CreatedBy=\"knxcfg\" ToolVersion=\"5.7.1093.38570\" \
     xmlns=\"http://knx.org/xml/project/20\">\r\n";

/// Builds `0.xml`, numbering ids and `Puid`s as it goes.
struct Writer {
    id: String,
    puid: u32,
    out: String,
    /// Device id by individual address, for the building structure.
    device_ids: HashMap<IndividualAddress, String>,
    /// `GA-n` by group address, for links.
    ga_ids: HashMap<GroupAddress, String>,
}

impl Writer {
    fn new(id: &str) -> Self {
        Writer { id: id.to_string(), puid: 0, out: String::new(), device_ids: HashMap::new(), ga_ids: HashMap::new() }
    }

    fn next_puid(&mut self) -> u32 {
        self.puid += 1;
        self.puid
    }

    fn line(&mut self, depth: usize, text: &str) {
        for _ in 0..depth {
            self.out.push_str("  ");
        }
        self.out.push_str(text);
        self.out.push_str("\r\n");
    }

    /// `0.xml` and the last `Puid` used.
    fn installation(mut self, project: &Project, devices: &[ExportDevice], warnings: &mut Vec<String>) -> Result<(String, u32)> {
        let mut gas: Vec<GroupAddress> = project.groups.iter().map(|g| g.ga).collect();
        gas.sort();
        for (i, ga) in gas.iter().enumerate() {
            self.ga_ids.insert(*ga, format!("GA-{}", i + 1));
        }

        self.out.push_str(HEADER);
        self.line(1, &format!("<Project Id=\"{}\">", self.id));
        self.line(2, "<Installations>");
        self.line(3, "<Installation Name=\"\" BCUKey=\"4294967295\" IPRoutingLatencyTolerance=\"2000\">");
        self.topology(project, devices, warnings)?;
        if !project.locations.is_empty() {
            self.line(4, "<Locations>");
            let mut n = 0;
            for s in &project.locations {
                self.space(s, 5, &mut n, warnings);
            }
            self.line(4, "</Locations>");
        }
        self.group_addresses(project);
        self.line(3, "</Installation>");
        self.line(2, "</Installations>");
        self.line(1, "</Project>");
        self.out.push_str("</KNX>");
        Ok((self.out, self.puid))
    }

    fn topology(&mut self, project: &Project, devices: &[ExportDevice], warnings: &mut Vec<String>) -> Result<()> {
        let by_ia: HashMap<IndividualAddress, &ExportDevice> = devices.iter().map(|d| (d.ia, d)).collect();
        let instances: HashMap<IndividualAddress, &DeviceInstance> = project.devices.iter().map(|d| (d.ia, d)).collect();
        let (mut areas, mut lines, mut dis) = (0, 0, 0);
        self.line(4, "<Topology>");
        for area in &complete_topology(&project.topology) {
            areas += 1;
            let puid = self.next_puid();
            self.line(
                5,
                &format!(
                    "<Area Id=\"{}-0_A-{areas}\" Address=\"{}\" Name=\"{}\" Puid=\"{puid}\">",
                    self.id,
                    area.number,
                    attr(&area.name)
                ),
            );
            for line in &area.lines {
                lines += 1;
                let puid = self.next_puid();
                let medium = match line.medium {
                    None | Some(Medium::Tp) => "MT-0",
                    Some(Medium::Pl) => "MT-1",
                    Some(Medium::Rf) => "MT-2",
                    Some(Medium::Ip) => "MT-5",
                };
                let description = opt_attr("Description", line.description.as_deref());
                let open = format!(
                    "<Line Id=\"{}-0_L-{lines}\" Address=\"{}\" Name=\"{}\"{description} MediumTypeRefId=\"{medium}\" Puid=\"{puid}\"",
                    self.id,
                    line.number,
                    attr(&line.name)
                );
                if line.devices.is_empty() {
                    self.line(6, &format!("{open} />"));
                    continue;
                }
                self.line(6, &format!("{open}>"));
                for d in &line.devices {
                    let Some(dev) = by_ia.get(&d.ia) else {
                        warnings.push(format!("{}: no product data, the device is not exported", d.ia));
                        continue;
                    };
                    dis += 1;
                    let id = format!("{}-0_DI-{dis}", self.id);
                    self.device_ids.insert(d.ia, id.clone());
                    let empty = DeviceInstance { ia: d.ia, parameters: vec![], links: vec![], objects: vec![] };
                    let instance = instances.get(&d.ia).copied().unwrap_or(&empty);
                    self.device(&id, d, dev, instance, warnings)?;
                }
                self.line(6, "</Line>");
            }
            self.line(5, "</Area>");
        }
        self.line(4, "</Topology>");
        Ok(())
    }

    fn device(
        &mut self,
        id: &str,
        d: &knx_core::DeviceRef,
        dev: &ExportDevice,
        instance: &DeviceInstance,
        warnings: &mut Vec<String>,
    ) -> Result<()> {
        let ap = dev.ap;
        let values: HashMap<String, String> = instance
            .parameters
            .iter()
            .map(|p| (full_key(ap, &p.id), yaml_string(&p.value)))
            .collect();
        let ev = Evaluation::new(ap, &values);

        let name = if d.name == dev.product_name { "" } else { d.name.as_str() };
        let serial = d.serial_number.as_deref().and_then(serial_base64);
        let puid = self.next_puid();
        let mut open = format!(
            "<DeviceInstance Id=\"{id}\" Address=\"{}\" Name=\"{}\" ProductRefId=\"{}\" Hardware2ProgramRefId=\"{}\" \
             Comment=\"{}\" Description=\"{}\"",
            d.ia.device,
            attr(name),
            attr(&dev.product),
            attr(&d.hardware),
            attr(d.comment.as_deref().unwrap_or("")),
            attr(d.description.as_deref().unwrap_or("")),
        );
        open.push_str(&opt_attr("SerialNumber", serial.as_deref()));
        let _ = write!(open, " Puid=\"{puid}\"");

        let parameters = parameter_refs(ap, &ev, &values, d.ia, warnings);
        let objects = self.object_refs(&ev, instance, d.ia, warnings);
        if parameters.is_empty() && objects.is_empty() {
            self.line(7, &format!("{open} />"));
            return Ok(());
        }
        self.line(7, &format!("{open}>"));
        if !parameters.is_empty() {
            self.line(8, "<ParameterInstanceRefs>");
            for (ref_id, value) in parameters {
                self.line(9, &format!("<ParameterInstanceRef RefId=\"{}\" Value=\"{}\" />", attr(&ref_id), attr(&value)));
            }
            self.line(8, "</ParameterInstanceRefs>");
        }
        if !objects.is_empty() {
            self.line(8, "<ComObjectInstanceRefs>");
            for o in objects {
                self.line(9, &o);
            }
            self.line(8, "</ComObjectInstanceRefs>");
        }
        self.line(7, "</DeviceInstance>");
        Ok(())
    }

    /// `ComObjectInstanceRef` elements for the objects with links or settings.
    fn object_refs(
        &self,
        ev: &Evaluation,
        instance: &DeviceInstance,
        ia: IndividualAddress,
        warnings: &mut Vec<String>,
    ) -> Vec<String> {
        let mut active: HashMap<u16, String> = HashMap::new();
        for (scope, r) in ev.active_com_object_refs() {
            if let Some((asap, _)) = tables::resolve_com_object_ref(ev, scope, r) {
                active.entry(asap).or_insert_with(|| object_ref_id(ev, scope, &r.id));
            }
        }
        let mut numbers: BTreeSet<u16> = instance.links.iter().map(|l| l.object_number).collect();
        numbers.extend(instance.objects.iter().map(|o| o.object_number));

        let mut out = Vec::new();
        for n in numbers {
            let Some(ref_id) = active.get(&n) else {
                warnings.push(format!("{ia}: group object #{n} is not active, its links and settings are not exported"));
                continue;
            };
            let mut e = format!("<ComObjectInstanceRef RefId=\"{}\"", attr(ref_id));
            if let Some(s) = instance.objects.iter().find(|o| o.object_number == n) {
                e.push_str(&opt_attr("Text", s.text.as_deref()));
                e.push_str(&opt_attr("FunctionText", s.function_text.as_deref()));
                e.push_str(&opt_attr("Priority", s.priority.map(|p| p.ets_name())));
                for (name, v) in [
                    ("ReadFlag", s.read),
                    ("WriteFlag", s.write),
                    ("CommunicationFlag", s.communication),
                    ("TransmitFlag", s.transmit),
                    ("UpdateFlag", s.update),
                    ("ReadOnInitFlag", s.read_on_init),
                ] {
                    e.push_str(&opt_attr(name, v.map(|on| if on { "Enabled" } else { "Disabled" })));
                }
                e.push_str(&opt_attr("DatapointType", s.dpt.as_deref()));
            }
            // The sending address comes first (docs/knxproj-format.md).
            let mut links: Vec<_> = instance.links.iter().filter(|l| l.object_number == n).collect();
            links.sort_by_key(|l| (!l.send, l.ga));
            let ids: Vec<&str> = links.iter().filter_map(|l| self.ga_ids.get(&l.ga).map(String::as_str)).collect();
            if !ids.is_empty() {
                let _ = write!(e, " Links=\"{}\"", ids.join(" "));
            }
            e.push_str(" />");
            out.push(e);
        }
        out
    }

    fn space(&mut self, s: &Space, depth: usize, n: &mut u32, warnings: &mut Vec<String>) {
        *n += 1;
        let puid = self.next_puid();
        let mut open = format!(
            "<Space Type=\"{}\" Id=\"{}-0_BP-{n}\" Name=\"{}\"",
            attr(&s.kind),
            self.id,
            attr(&s.name)
        );
        open.push_str(&opt_attr("Number", s.number.as_deref()));
        open.push_str(&opt_attr("Usage", s.usage.as_deref()));
        open.push_str(&opt_attr("Description", s.description.as_deref()));
        let _ = write!(open, " Puid=\"{puid}\"");
        if s.devices.is_empty() && s.spaces.is_empty() {
            self.line(depth, &format!("{open} />"));
            return;
        }
        self.line(depth, &format!("{open}>"));
        for c in &s.spaces {
            self.space(c, depth + 1, n, warnings);
        }
        for ia in &s.devices {
            match self.device_ids.get(ia) {
                Some(id) => {
                    let id = id.clone();
                    self.line(depth + 1, &format!("<DeviceInstanceRef RefId=\"{id}\" />"));
                }
                None => warnings.push(format!("{} {:?}: device {ia} is not exported", s.kind, s.name)),
            }
        }
        self.line(depth, "</Space>");
    }

    /// The group ranges with their addresses. Addresses outside every stored
    /// range get a main and middle group of their own, as ETS needs them.
    fn group_addresses(&mut self, project: &Project) {
        let mut ranges = project.group_ranges.clone();
        for g in &project.groups {
            if !ranges.iter().any(|r| contains(r, g.ga)) {
                add_default_range(&mut ranges, g.ga);
            }
        }
        ranges.sort_by_key(|r| r.start);
        let mut placed = BTreeSet::new();
        let mut n = 0;
        self.line(4, "<GroupAddresses>");
        self.line(5, "<GroupRanges>");
        for r in &ranges {
            self.group_range(r, project, 6, &mut n, &mut placed);
        }
        self.line(5, "</GroupRanges>");
        self.line(4, "</GroupAddresses>");
    }

    fn group_range(
        &mut self,
        r: &GroupRange,
        project: &Project,
        depth: usize,
        n: &mut u32,
        placed: &mut BTreeSet<GroupAddress>,
    ) {
        *n += 1;
        let puid = self.next_puid();
        let description = opt_attr("Description", r.description.as_deref());
        self.line(
            depth,
            &format!(
                "<GroupRange Id=\"{}-0_GR-{n}\" RangeStart=\"{}\" RangeEnd=\"{}\" Name=\"{}\"{description} Puid=\"{puid}\">",
                self.id,
                r.start.to_u16(),
                r.end.to_u16(),
                attr(&r.name)
            ),
        );
        for c in &r.ranges {
            self.group_range(c, project, depth + 1, n, placed);
        }
        let mut groups: Vec<_> = project.groups.iter().filter(|g| contains(r, g.ga) && !placed.contains(&g.ga)).collect();
        groups.sort_by_key(|g| g.ga);
        for g in groups {
            placed.insert(g.ga);
            let puid = self.next_puid();
            let mut e = format!(
                "<GroupAddress Id=\"{}-0_{}\" Address=\"{}\" Name=\"{}\"",
                self.id,
                self.ga_ids[&g.ga],
                g.ga.to_u16(),
                attr(&g.name)
            );
            if g.unfiltered {
                e.push_str(" Unfiltered=\"true\"");
            }
            e.push_str(&opt_attr("Description", g.description.as_deref()));
            e.push_str(&opt_attr("Comment", g.comment.as_deref()));
            e.push_str(&opt_attr("DatapointType", g.dpt.as_deref()));
            let _ = write!(e, " Puid=\"{puid}\" />");
            self.line(depth + 1, &e);
        }
        self.line(depth, "</GroupRange>");
    }
}

/// The topology with the backbone (area 0, line 0) and every area's main line
/// (line 0) added where the model has none: ETS fails on a project without
/// them.
fn complete_topology(topology: &[Area]) -> Vec<Area> {
    let mut areas = topology.to_vec();
    if !areas.iter().any(|a| a.number == 0) {
        areas.push(Area { number: 0, name: "Backbone area".into(), lines: Vec::new() });
    }
    for area in &mut areas {
        if !area.lines.iter().any(|l| l.number == 0) {
            let name = if area.number == 0 { "Backbone line" } else { "Main line" };
            area.lines.push(Line { number: 0, name: name.into(), medium: None, description: None, devices: Vec::new() });
        }
        area.lines.sort_by_key(|l| l.number);
    }
    areas.sort_by_key(|a| a.number);
    areas
}

/// Whether `ga` lies in `r` (a range's addresses are those not in one of its
/// sub-ranges, which are placed first).
fn contains(r: &GroupRange, ga: GroupAddress) -> bool {
    r.start <= ga && ga <= r.end
}

/// Add the main group (and middle group) of `ga` as ETS creates them.
fn add_default_range(ranges: &mut Vec<GroupRange>, ga: GroupAddress) {
    let raw = ga.to_u16();
    let main_start = raw & 0xF800;
    let middle_start = raw & 0xFF00;
    let at = |v: u16| v.to_string().parse::<GroupAddress>().expect("16-bit value");
    let main_pos = match ranges.iter().position(|r| r.start.to_u16() & 0xF800 == main_start && r.end.to_u16() >= main_start | 0x7FF) {
        Some(i) => i,
        None => {
            ranges.push(GroupRange {
                start: at(main_start.max(1)),
                end: at(main_start | 0x7FF),
                name: format!("Main group {}", main_start >> 11),
                description: None,
                ranges: Vec::new(),
            });
            ranges.len() - 1
        }
    };
    let main = &mut ranges[main_pos];
    if !main.ranges.iter().any(|r| contains(r, ga)) {
        main.ranges.push(GroupRange {
            start: at(middle_start.max(1)),
            end: at(middle_start | 0xFF),
            name: format!("Middle group {}/{}", main_start >> 11, (middle_start >> 8) & 7),
            description: None,
            ranges: Vec::new(),
        });
        main.ranges.sort_by_key(|r| r.start);
    }
}

/// `ParameterInstanceRef`s for the stored values: the value goes to the
/// parameter's active ref (or its first ref when none is active).
fn parameter_refs(
    ap: &ApplicationProgram,
    ev: &Evaluation,
    values: &HashMap<String, String>,
    ia: IndividualAddress,
    warnings: &mut Vec<String>,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut keys: Vec<&String> = values.keys().collect();
    keys.sort();
    for key in keys {
        let (prefix, param) = match key.rsplit_once('/') {
            Some((p, param)) => (p, param),
            None => ("", key.as_str()),
        };
        let Some(scope) = ev.scope_index(prefix) else {
            warnings.push(format!("{ia}: parameter {} is not in {}", short_key(ap, key), ap.id));
            continue;
        };
        let refs = ev.scopes[scope].refs_of(param);
        let Some(r) = refs.iter().find(|r| ev.is_ref_active(scope, &r.id)).or(refs.first()) else {
            warnings.push(format!("{ia}: parameter {} has no ParameterRef", short_key(ap, key)));
            continue;
        };
        out.push((instance_ref_id(ev, scope, &r.id, true), values[key].clone()));
    }
    out
}

/// The id ETS gives a ref of a scope: a device ref as it is, a module ref as
/// `<instance>_MI-1_<ref without the module definition>`. Parameters keep
/// the application prefix, objects do not.
fn instance_ref_id(ev: &Evaluation, scope: usize, ref_id: &str, keep_prefix: bool) -> String {
    let ap = ev.ap;
    let s = &ev.scopes[scope];
    let id = match s.module {
        None => ref_id.to_string(),
        Some(module) => {
            let rest = ref_id.strip_prefix(&format!("{}_", module.id)).unwrap_or(ref_id);
            format!("{}_MI-1_{}", s.prefix, short_key(ap, rest))
        }
    };
    if keep_prefix {
        full_key(ap, &id)
    } else {
        short_key(ap, &id)
    }
}

fn object_ref_id(ev: &Evaluation, scope: usize, ref_id: &str) -> String {
    instance_ref_id(ev, scope, ref_id, false)
}

/// Copy the product data the devices need: per manufacturer the merged
/// `Catalog.xml` and `Hardware.xml`, the devices' application programs and
/// other files (baggages), and the largest `knx_master.xml`.
fn product_files(devices: &[ExportDevice], files: &mut BTreeMap<String, Vec<u8>>) -> Result<()> {
    let mut by_knxprod: BTreeMap<&Path, BTreeSet<String>> = BTreeMap::new();
    for d in devices {
        by_knxprod.entry(d.knxprod.as_path()).or_default().insert(d.ap.id.clone());
    }
    let mut merged: BTreeMap<String, Vec<Vec<u8>>> = BTreeMap::new();
    for (path, apps) in by_knxprod {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut archive = zip::ZipArchive::new(file).with_context(|| format!("reading {}", path.display()))?;
        let names: Vec<String> = archive.file_names().map(str::to_owned).collect();
        for name in names {
            if name.ends_with('/') || name.ends_with(".signature") {
                continue;
            }
            let Some((dir, file_name)) = name.split_once('/') else {
                if name == "knx_master.xml" {
                    let data = read(&mut archive, &name)?;
                    if files.get(&name).map_or(true, |old| old.len() < data.len()) {
                        files.insert(name, data);
                    }
                }
                continue;
            };
            if !dir.starts_with("M-") {
                continue;
            }
            match file_name {
                "Catalog.xml" | "Hardware.xml" => merged.entry(name.clone()).or_default().push(read(&mut archive, &name)?),
                f if f.starts_with(&format!("{dir}_A-")) && f.ends_with(".xml") => {
                    let app = f.trim_end_matches(".xml");
                    if apps.iter().any(|a| a == app) {
                        let data = read(&mut archive, &name)?;
                        files.insert(name, data);
                    }
                }
                _ => {
                    let data = read(&mut archive, &name)?;
                    files.insert(name, data);
                }
            }
        }
    }
    if !files.contains_key("knx_master.xml") {
        bail!("no knxprod of the exported devices holds a knx_master.xml");
    }
    for (name, docs) in merged {
        files.insert(name.clone(), merge::merge_documents(&docs).with_context(|| format!("merging {name}"))?);
    }
    Ok(())
}

fn read(archive: &mut zip::ZipArchive<File>, name: &str) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut entry = archive.by_name(name).map_err(|e| anyhow!("{name}: {e}"))?;
    let mut data = Vec::new();
    entry.read_to_end(&mut data)?;
    Ok(data)
}

fn yaml_string(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Bool(b) => if *b { "1" } else { "0" }.to_string(),
        _ => String::new(),
    }
}

/// `0083:12345678` → base64 of the 6 octets, as ETS stores serial numbers.
fn serial_base64(s: &str) -> Option<String> {
    use base64::Engine;
    let (m, n) = s.split_once(':')?;
    let mut b = u16::from_str_radix(m, 16).ok()?.to_be_bytes().to_vec();
    b.extend(u32::from_str_radix(n, 16).ok()?.to_be_bytes());
    Some(base64::engine::general_purpose::STANDARD.encode(b))
}

fn opt_attr(name: &str, value: Option<&str>) -> String {
    match value {
        Some(v) => format!(" {name}=\"{}\"", attr(v)),
        None => String::new(),
    }
}

/// Escape an attribute value.
fn attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("&#xA;"),
            '\r' => out.push_str("&#xD;"),
            '\t' => out.push_str("&#x9;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_escaping() {
        assert_eq!(project_id("knx-sim"), project_id("knx-sim"));
        assert!(project_id("x").starts_with("P-"));
        assert_eq!(project_guid("a").len(), 36);
        assert_eq!(attr("a<b & \"c\"\n"), "a&lt;b &amp; &quot;c&quot;&#xA;");
        assert_eq!(serial_base64("0083:12345678").as_deref(), Some("AIMSNFZ4"));
        assert_eq!(crate::serial_number("AIMSNFZ4").as_deref(), Some("0083:12345678"));
    }

    /// The manifest is `name:base64(sha1)` per file, sorted, with `\\`
    /// separators and joined with `,`; files of other directories do not count.
    #[test]
    fn directory_digest_covers_the_files_of_the_directory() {
        let mut files = BTreeMap::new();
        files.insert("P-0101/project.xml".to_string(), b"a".to_vec());
        files.insert("P-0101/sub/0.xml".to_string(), b"b".to_vec());
        files.insert("M-0001/Catalog.xml".to_string(), b"c".to_vec());
        // sha1("a") = hvfkN/qlp/zhXR3cuerq6jd2Z7g=, sha1("b") =6dcfXufJLW3J6S/9rRe4vUlBj5g=
        let manifest = "project.xml:hvfkN/qlp/zhXR3cuerq6jd2Z7g=,sub\\0.xml:6dcfXufJLW3J6S/9rRe4vUlBj5g=";
        assert_eq!(directory_digest(&files, "P-0101"), Sha1::digest(manifest.as_bytes()).to_vec());
    }

    #[test]
    fn default_ranges_cover_ungrouped_addresses() {
        let mut ranges = Vec::new();
        add_default_range(&mut ranges, "1/2/3".parse().unwrap());
        add_default_range(&mut ranges, "1/2/4".parse().unwrap());
        add_default_range(&mut ranges, "1/3/0".parse().unwrap());
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start.to_string(), "1/0/0");
        assert_eq!(ranges[0].end.to_string(), "1/7/255");
        let middle: Vec<String> = ranges[0].ranges.iter().map(|r| format!("{}-{}", r.start, r.end)).collect();
        assert_eq!(middle, ["1/2/0-1/2/255", "1/3/0-1/3/255"]);
    }
}
