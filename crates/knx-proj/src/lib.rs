//! ETS project (`.knxproj`) import: project file → typed model.
//!
//! A `.knxproj` is a ZIP archive holding the project and the product data of
//! everything it uses (see `docs/knxproj-format.md`):
//!
//! ```text
//! P-XXXX/project.xml      # name, group address style
//! P-XXXX/0.xml            # topology, device instances, group addresses
//! M-XXXX/...              # Catalog.xml, Hardware.xml, application programs
//! knx_master.xml
//! ```
//!
//! Importing needs the application program of every device, because the
//! project only refers to its `ParameterRef`s and `ComObjectRef`s. The product
//! data embedded in the archive can be unpacked with
//! [`ProjectFile::extract_product`].
//!
//! Anything that cannot be carried over exactly is reported in
//! [`Imported::warnings`], never dropped silently. Each device is also checked
//! against ETS's own results: the group objects ETS marks `IsActive` have to
//! match the ones [`knx_prod::Evaluation`] finds active.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use knx_core::{
    Area, DeviceInstance, DeviceRef, GroupAddress, GroupAddressEntry, GroupObjectLink,
    GroupAddressStyle, IndividualAddress, Line, Medium, ObjectSettings, ParameterValue, Priority, Project, Space,
};
use knx_prod::eval::{full_key, short_key};
use knx_prod::tables::{self, GroupObject, Link, ObjectOverride, TableFormat};
use knx_prod::{ApplicationProgram, Evaluation};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// XML
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct InfoDoc {
    #[serde(rename = "Project")]
    project: InfoProject,
}

#[derive(Debug, Deserialize)]
struct InfoProject {
    #[serde(rename = "ProjectInformation")]
    information: ProjectInformation,
}

#[derive(Debug, Deserialize)]
struct ProjectInformation {
    #[serde(rename = "@Name", default)]
    name: String,
    #[serde(rename = "@GroupAddressStyle", default)]
    group_address_style: String,
    #[serde(rename = "@Comment", default)]
    comment: String,
}

#[derive(Debug, Deserialize)]
struct InstallationDoc {
    #[serde(rename = "Project")]
    project: InstallationProject,
}

#[derive(Debug, Deserialize)]
struct InstallationProject {
    #[serde(rename = "Installations")]
    installations: Installations,
}

#[derive(Debug, Deserialize)]
struct Installations {
    #[serde(rename = "Installation", default)]
    items: Vec<Installation>,
}

#[derive(Debug, Deserialize)]
struct Installation {
    #[serde(rename = "Topology", default)]
    topology: Option<Topology>,
    #[serde(rename = "GroupAddresses", default)]
    group_addresses: Option<GroupAddresses>,
    #[serde(rename = "Locations", default)]
    locations: Option<XmlLocations>,
}

#[derive(Debug, Deserialize)]
struct XmlLocations {
    #[serde(rename = "Space", default)]
    spaces: Vec<XmlSpace>,
}

#[derive(Debug, Deserialize)]
struct XmlSpace {
    #[serde(rename = "@Type", default)]
    kind: String,
    #[serde(rename = "@Name", default)]
    name: String,
    #[serde(rename = "@Number", default)]
    number: String,
    #[serde(rename = "@Usage", default)]
    usage: String,
    #[serde(rename = "@Description", default)]
    description: String,
    #[serde(rename = "DeviceInstanceRef", default)]
    devices: Vec<XmlRef>,
    #[serde(rename = "Space", default)]
    spaces: Vec<XmlSpace>,
}

#[derive(Debug, Deserialize)]
struct XmlRef {
    #[serde(rename = "@RefId")]
    ref_id: String,
}

#[derive(Debug, Deserialize)]
struct Topology {
    #[serde(rename = "Area", default)]
    areas: Vec<XmlArea>,
    /// Devices that are in the project but on no line.
    #[serde(rename = "UnassignedDevices", default)]
    unassigned: Option<XmlSegment>,
}

#[derive(Debug, Deserialize)]
struct XmlArea {
    #[serde(rename = "@Address")]
    address: u8,
    #[serde(rename = "@Name", default)]
    name: String,
    #[serde(rename = "Line", default)]
    lines: Vec<XmlLine>,
}

#[derive(Debug, Deserialize)]
struct XmlLine {
    #[serde(rename = "@Address")]
    address: u8,
    #[serde(rename = "@Name", default)]
    name: String,
    #[serde(rename = "@MediumTypeRefId", default)]
    medium: String,
    #[serde(rename = "@Description", default)]
    description: String,
    #[serde(rename = "DeviceInstance", default)]
    devices: Vec<XmlDevice>,
    /// Newer ETS versions place the devices in `Segment`s of the line.
    #[serde(rename = "Segment", default)]
    segments: Vec<XmlSegment>,
}

#[derive(Debug, Deserialize)]
struct XmlSegment {
    #[serde(rename = "DeviceInstance", default)]
    devices: Vec<XmlDevice>,
}

#[derive(Debug, Deserialize)]
struct XmlDevice {
    #[serde(rename = "@Id", default)]
    id: String,
    #[serde(rename = "@Address", default)]
    address: Option<u8>,
    #[serde(rename = "@Name", default)]
    name: String,
    #[serde(rename = "@Hardware2ProgramRefId", default)]
    h2p: String,
    #[serde(rename = "@ProductRefId", default)]
    product: String,
    #[serde(rename = "@Description", default)]
    description: String,
    #[serde(rename = "@Comment", default)]
    comment: String,
    #[serde(rename = "@SerialNumber", default)]
    serial_number: String,
    #[serde(rename = "ParameterInstanceRefs", default)]
    parameters: Option<ParameterInstanceRefs>,
    #[serde(rename = "ComObjectInstanceRefs", default)]
    objects: Option<ComObjectInstanceRefs>,
    #[serde(rename = "ModuleInstances", default)]
    modules: Option<XmlModuleInstances>,
    #[serde(rename = "GroupObjectTree", default)]
    object_tree: Option<GroupObjectTree>,
}

#[derive(Debug, Deserialize)]
struct XmlModuleInstances {
    #[serde(rename = "ModuleInstance", default)]
    items: Vec<XmlModuleInstance>,
}

#[derive(Debug, Deserialize)]
struct XmlModuleInstance {
    #[serde(rename = "@RefId")]
    ref_id: String,
    #[serde(rename = "Arguments", default)]
    arguments: Option<XmlArguments>,
}

#[derive(Debug, Deserialize)]
struct XmlArguments {
    #[serde(rename = "Argument", default)]
    items: Vec<XmlArgument>,
}

#[derive(Debug, Deserialize)]
struct XmlArgument {
    #[serde(rename = "@RefId")]
    ref_id: String,
    #[serde(rename = "@Value", default)]
    value: String,
}

#[derive(Debug, Deserialize)]
struct GroupObjectTree {
    #[serde(rename = "Nodes", default)]
    nodes: Option<TreeNodes>,
}

#[derive(Debug, Deserialize)]
struct TreeNodes {
    #[serde(rename = "Node", default)]
    items: Vec<TreeNode>,
}

#[derive(Debug, Deserialize)]
struct TreeNode {
    #[serde(rename = "@GroupObjectInstances", default)]
    objects: String,
    #[serde(rename = "Nodes", default)]
    nodes: Option<TreeNodes>,
}

impl TreeNodes {
    fn collect<'a>(&'a self, out: &mut Vec<&'a str>) {
        for node in &self.items {
            out.extend(node.objects.split_whitespace());
            if let Some(nodes) = &node.nodes {
                nodes.collect(out);
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct ParameterInstanceRefs {
    #[serde(rename = "ParameterInstanceRef", default)]
    items: Vec<ParameterInstanceRef>,
}

#[derive(Debug, Deserialize)]
struct ParameterInstanceRef {
    #[serde(rename = "@RefId")]
    ref_id: String,
    #[serde(rename = "@Value", default)]
    value: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ComObjectInstanceRefs {
    #[serde(rename = "ComObjectInstanceRef", default)]
    items: Vec<ComObjectInstanceRef>,
}

#[derive(Debug, Deserialize)]
struct ComObjectInstanceRef {
    #[serde(rename = "@RefId")]
    ref_id: String,
    #[serde(rename = "@IsActive", default)]
    is_active: Option<bool>,
    #[serde(rename = "@Priority", default)]
    priority: String,
    #[serde(rename = "@ReadFlag", default)]
    read_flag: String,
    #[serde(rename = "@WriteFlag", default)]
    write_flag: String,
    #[serde(rename = "@CommunicationFlag", default)]
    communication_flag: String,
    #[serde(rename = "@TransmitFlag", default)]
    transmit_flag: String,
    #[serde(rename = "@UpdateFlag", default)]
    update_flag: String,
    #[serde(rename = "@ReadOnInitFlag", default)]
    read_on_init_flag: String,
    #[serde(rename = "@Text", default)]
    text: Option<String>,
    #[serde(rename = "@FunctionText", default)]
    function_text: Option<String>,
    #[serde(rename = "@DatapointType", default)]
    datapoint_type: Option<String>,
    /// Newer ETS versions: space-separated group address ids. The first is taken
    /// as the sending one, which is not confirmed (docs/knxproj-format.md).
    #[serde(rename = "@Links", default)]
    links: String,
    #[serde(rename = "Connectors", default)]
    connectors: Option<Connectors>,
}

#[derive(Debug, Deserialize)]
struct Connectors {
    #[serde(rename = "Send", default)]
    send: Vec<Connector>,
    #[serde(rename = "Receive", default)]
    receive: Vec<Connector>,
}

#[derive(Debug, Deserialize)]
struct Connector {
    #[serde(rename = "@GroupAddressRefId")]
    group_address: String,
}

#[derive(Debug, Deserialize)]
struct GroupAddresses {
    #[serde(rename = "GroupRanges", default)]
    ranges: Option<GroupRanges>,
}

#[derive(Debug, Deserialize)]
struct GroupRanges {
    #[serde(rename = "GroupRange", default)]
    items: Vec<GroupRange>,
}

#[derive(Debug, Deserialize)]
struct GroupRange {
    #[serde(rename = "@Name", default)]
    name: String,
    #[serde(rename = "@RangeStart", default)]
    start: u16,
    #[serde(rename = "@RangeEnd", default)]
    end: u16,
    #[serde(rename = "@Description", default)]
    description: String,
    #[serde(rename = "GroupRange", default)]
    ranges: Vec<GroupRange>,
    #[serde(rename = "GroupAddress", default)]
    addresses: Vec<XmlGroupAddress>,
}

#[derive(Debug, Deserialize)]
struct XmlGroupAddress {
    #[serde(rename = "@Id")]
    id: String,
    #[serde(rename = "@Address")]
    address: u16,
    #[serde(rename = "@Name", default)]
    name: String,
    #[serde(rename = "@DatapointType", default)]
    datapoint_type: String,
    /// "Weiterleiten (nicht filtern)" in ETS.
    #[serde(rename = "@Unfiltered", default)]
    unfiltered: String,
    #[serde(rename = "@Description", default)]
    description: String,
    #[serde(rename = "@Comment", default)]
    comment: String,
}

/// `None` for an empty attribute.
fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// ETS line media (`MediumTypeRefId`); twisted pair is the default.
fn medium(id: &str) -> Option<Medium> {
    match id {
        "MT-1" => Some(Medium::Pl),
        "MT-2" => Some(Medium::Rf),
        "MT-5" => Some(Medium::Ip),
        _ => None,
    }
}

/// `SerialNumber` (base64 of 6 octets) as `<manufacturer>:<serial>` in hex.
fn serial_number(b64: &str) -> Option<String> {
    use base64::Engine;
    let b = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let [m0, m1, s0, s1, s2, s3] = b.as_slice().try_into().ok()?;
    Some(format!("{:02X}{:02X}:{:02X}{:02X}{:02X}{:02X}", m0, m1, s0, s1, s2, s3))
}

// ---------------------------------------------------------------------------
// Project file
// ---------------------------------------------------------------------------

/// An opened `.knxproj` file.
#[derive(Debug)]
pub struct ProjectFile {
    path: PathBuf,
    name: String,
    group_address_style: String,
    comment: String,
    installation: Installation,
}

impl ProjectFile {
    /// Open and parse a `.knxproj` file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut archive = open_archive(&path)?;
        let names: Vec<String> = archive.file_names().map(str::to_owned).collect();
        let (project_xml, installation_xml) = project_entries(&names)?;

        let info: InfoDoc = parse(&read_entry(&mut archive, &project_xml)?)
            .with_context(|| format!("parsing {project_xml}"))?;
        let doc: InstallationDoc = parse(&read_entry(&mut archive, &installation_xml)?)
            .with_context(|| format!("parsing {installation_xml}"))?;
        let mut installations = doc.project.installations.items;
        if installations.len() != 1 {
            bail!(
                "{installation_xml} has {} installations, only projects with one are supported",
                installations.len()
            );
        }
        Ok(Self {
            path,
            name: info.project.information.name,
            group_address_style: info.project.information.group_address_style,
            comment: info.project.information.comment,
            installation: installations.remove(0),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn devices(&self) -> impl Iterator<Item = &XmlDevice> {
        self.installation
            .topology
            .iter()
            .flat_map(|t| &t.areas)
            .flat_map(|a| &a.lines)
            .flat_map(XmlLine::devices)
    }

    /// The `Hardware2ProgramRefId`s of the devices in the project.
    pub fn hardware_refs(&self) -> BTreeSet<String> {
        self.devices()
            .filter(|d| d.has_application())
            .map(|d| d.h2p.clone())
            .collect()
    }

    /// Write the product data embedded for `manufacturer` (`M-0083`) as a
    /// `.knxprod` file. A knxprod holds one manufacturer, a project can hold
    /// several. The output does not depend on when it is written, so importing
    /// again leaves the file unchanged.
    pub fn extract_product(&self, manufacturer: &str, dest: &Path) -> Result<()> {
        let mut archive = open_archive(&self.path)?;
        let dir = format!("{manufacturer}/");
        let mut names: Vec<String> = archive
            .file_names()
            .filter(|n| (n.starts_with(&dir) && !n.ends_with('/')) || *n == "knx_master.xml")
            .map(str::to_owned)
            .collect();
        names.sort();
        if !names.iter().any(|n| n.ends_with("/Catalog.xml")) {
            bail!("{} has no product data for {manufacturer}", self.path.display());
        }

        let file = File::create(dest).with_context(|| format!("creating {}", dest.display()))?;
        let mut out = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .last_modified_time(zip::DateTime::default());
        for name in names {
            let data = read_entry(&mut archive, &name)?;
            out.start_file(name.as_str(), options)?;
            out.write_all(&data)?;
        }
        out.finish()?;
        Ok(())
    }

    /// Convert to the typed model. `resolve` provides the product data of a
    /// `Hardware2ProgramRefId`.
    pub fn convert(&self, mut resolve: impl FnMut(&str) -> Result<ResolvedProduct>) -> Result<Imported> {
        let mut warnings = Vec::new();
        let style = match self.group_address_style.as_str() {
            "" => None,
            "ThreeLevel" => Some(GroupAddressStyle::ThreeLevel),
            "TwoLevel" => Some(GroupAddressStyle::TwoLevel),
            "Free" => Some(GroupAddressStyle::Free),
            other => {
                warnings.push(format!("unknown group address style {other}"));
                None
            }
        };

        let unassigned = self.installation.topology.iter().filter_map(|t| t.unassigned.as_ref());
        for dev in unassigned.flat_map(|u| &u.devices) {
            warnings.push(format!(
                "device {} ({}) is on no line and was skipped",
                dev.id,
                if dev.name.is_empty() { &dev.h2p } else { &dev.name },
            ));
        }

        let groups = self.collect_groups(&mut warnings)?;
        let ga_ids: GaIds = groups
            .iter()
            .flat_map(|(ids, entry)| ids.iter().map(|id| (id.clone(), entry.ga)))
            .collect();

        let mut project = Project::default();
        project.meta.name = self.name.clone();
        project.meta.group_address_style = style;
        project.meta.comment = non_empty(&self.comment);
        project.groups = groups.into_iter().map(|(_, entry)| entry).collect();
        let ranges = self.installation.group_addresses.as_ref().and_then(|g| g.ranges.as_ref());
        project.group_ranges = ranges.iter().flat_map(|r| &r.items).filter_map(group_range).collect();
        let mut device_ids: HashMap<&str, IndividualAddress> = HashMap::new();

        let mut products: HashMap<String, ResolvedProduct> = HashMap::new();
        let mut checks = Vec::new();
        let mut couplers = Vec::new();
        let mut seen = HashSet::new();
        for xml_area in self.installation.topology.iter().flat_map(|t| &t.areas) {
            let mut area = Area {
                number: xml_area.address,
                name: xml_area.name.clone(),
                lines: Vec::new(),
            };
            for xml_line in &xml_area.lines {
                let mut line = Line {
                    number: xml_line.address,
                    name: xml_line.name.clone(),
                    medium: medium(&xml_line.medium),
                    description: non_empty(&xml_line.description),
                    devices: Vec::new(),
                };
                for dev in xml_line.devices() {
                    let Some(address) = dev.address else {
                        warnings.push(format!(
                            "device {} on line {}.{} has no address and was skipped",
                            dev.id, area.number, line.number
                        ));
                        continue;
                    };
                    let ia = IndividualAddress::new(area.number, line.number, address)
                        .with_context(|| format!("device {}", dev.id))?;
                    if !seen.insert(ia) {
                        warnings.push(format!("a second device at {ia} ({}) was skipped", dev.id));
                        continue;
                    }
                    if !dev.has_application() {
                        warnings.push(format!("device {ia} has no application program and was skipped"));
                        continue;
                    }
                    if !products.contains_key(&dev.h2p) {
                        products.insert(dev.h2p.clone(), resolve(&dev.h2p)?);
                    }
                    let product = &products[&dev.h2p];
                    let converted = convert_device(dev, ia, product, &ga_ids);
                    if let Some(values) = converted.coupler_values {
                        couplers.push((ia, dev.h2p.clone(), values, checks.len()));
                    }
                    warnings.extend(converted.warnings.iter().map(|w| format!("{ia}: {w}")));
                    checks.push(DeviceCheck {
                        ia,
                        compared_objects: converted.compared_objects,
                        visibility_mismatches: converted.visibility_mismatches,
                        compared_arguments: converted.compared_arguments,
                        argument_mismatches: converted.argument_mismatches,
                        image_error: converted.image_error,
                    });
                    line.devices.push(DeviceRef {
                        ia,
                        name: if dev.name.is_empty() {
                            product.name.clone()
                        } else {
                            dev.name.clone()
                        },
                        hardware: dev.h2p.clone(),
                        application: None,
                        knxprod: product.knxprod.clone(),
                        product: non_empty(&dev.product),
                        description: non_empty(&dev.description),
                        comment: non_empty(&dev.comment),
                        serial_number: serial_number(&dev.serial_number),
                    });
                    device_ids.insert(&dev.id, ia);
                    project.devices.push(converted.instance);
                }
                area.lines.push(line);
            }
            project.topology.push(area);
        }
        // A coupler's filter table needs the links of all the other devices.
        for (ia, h2p, values, check) in couplers {
            let ev = Evaluation::new(&products[&h2p].application, &values);
            let filter = project.coupler_filter(ia).iter().map(|ga| ga.to_u16()).collect();
            match knx_prod::build_coupler_image(&ev, &filter) {
                Ok(image) => warnings.extend(image.warnings.iter().map(|w| format!("{ia}: {w}"))),
                Err(e) => {
                    warnings.push(format!("{ia}: no download image can be built: {e:#}"));
                    checks[check].image_error = Some(format!("{e:#}"));
                }
            }
        }
        let spaces = self.installation.locations.iter().flat_map(|l| &l.spaces);
        project.locations = spaces.map(|s| space(s, &device_ids, &mut warnings)).collect();
        project.sort();
        Ok(Imported { project, warnings, checks })
    }

    /// All group addresses with the ids they are referenced by.
    fn collect_groups(&self, warnings: &mut Vec<String>) -> Result<Vec<(Vec<String>, GroupAddressEntry)>> {
        fn walk<'a>(range: &'a GroupRange, out: &mut Vec<&'a XmlGroupAddress>) {
            out.extend(&range.addresses);
            for r in &range.ranges {
                walk(r, out);
            }
        }
        let mut xml = Vec::new();
        let ranges = self.installation.group_addresses.as_ref().and_then(|g| g.ranges.as_ref());
        for r in ranges.iter().flat_map(|r| &r.items) {
            walk(r, &mut xml);
        }

        let mut out = Vec::with_capacity(xml.len());
        for g in xml {
            let ga: GroupAddress = g
                .address
                .to_string()
                .parse()
                .with_context(|| format!("group address {}", g.id))?;
            if ga.is_broadcast() {
                warnings.push(format!("group address {} is 0/0/0 and was skipped", g.id));
                continue;
            }
            // The `DatapointType` attribute is a list; the first one is the type.
            let dpt = g.datapoint_type.split_whitespace().next().map(str::to_owned);
            // `Links` attributes refer to `P-XXXX-0_GA-1` as `GA-1`.
            let mut ids = vec![g.id.clone()];
            if let Some((_, short)) = g.id.rsplit_once('_') {
                ids.push(short.to_string());
            }
            out.push((
                ids,
                GroupAddressEntry {
                    ga,
                    name: g.name.clone(),
                    dpt,
                    unfiltered: g.unfiltered == "true",
                    description: non_empty(&g.description),
                    comment: non_empty(&g.comment),
                },
            ));
        }
        Ok(out)
    }
}

fn group_range(r: &GroupRange) -> Option<knx_core::GroupRange> {
    let ga = |raw: u16| raw.to_string().parse::<GroupAddress>().ok();
    Some(knx_core::GroupRange {
        start: ga(r.start)?,
        end: ga(r.end)?,
        name: r.name.clone(),
        description: non_empty(&r.description),
        ranges: r.ranges.iter().filter_map(group_range).collect(),
    })
}

fn space(s: &XmlSpace, devices: &HashMap<&str, IndividualAddress>, warnings: &mut Vec<String>) -> Space {
    let mut ias = Vec::new();
    for d in &s.devices {
        match devices.get(d.ref_id.as_str()) {
            Some(ia) => ias.push(*ia),
            None => warnings.push(format!("{} {:?} refers to device {}, which was not imported", s.kind, s.name, d.ref_id)),
        }
    }
    Space {
        kind: s.kind.clone(),
        name: s.name.clone(),
        number: non_empty(&s.number),
        usage: non_empty(&s.usage),
        description: non_empty(&s.description),
        devices: ias,
        spaces: s.spaces.iter().map(|c| space(c, devices, warnings)).collect(),
    }
}

impl XmlDevice {
    /// Devices without an application have no `Hardware2ProgramRefId`, or one
    /// that stops at `..._HP`.
    fn has_application(&self) -> bool {
        !self.h2p.is_empty() && !self.h2p.ends_with("_HP")
    }
}

impl XmlLine {
    fn devices(&self) -> impl Iterator<Item = &XmlDevice> {
        self.devices
            .iter()
            .chain(self.segments.iter().flat_map(|s| &s.devices))
    }
}

/// The product data of one device type.
pub struct ResolvedProduct {
    pub application: ApplicationProgram,
    /// The format of the tables, or why no image can be built for this product.
    pub table_format: std::result::Result<TableFormat, String>,
    /// Catalog name, used for devices without a name of their own.
    pub name: String,
    /// knxprod path to record on the device (see `DeviceRef::knxprod`).
    pub knxprod: Option<String>,
}

/// The result of an import.
#[derive(Debug)]
pub struct Imported {
    pub project: Project,
    /// What could not be carried over exactly, or looks wrong.
    pub warnings: Vec<String>,
    pub checks: Vec<DeviceCheck>,
}

/// How one imported device compares with ETS's own results.
#[derive(Debug)]
pub struct DeviceCheck {
    pub ia: IndividualAddress,
    /// Number of group object refs whose state ETS gives away: `IsActive`
    /// (older ETS versions), or having links or being listed in the group
    /// object tree, which only an active object can.
    pub compared_objects: usize,
    /// Group objects ETS and the evaluator disagree on.
    pub visibility_mismatches: Vec<String>,
    /// Number of module arguments ETS recorded (these set the object numbers).
    pub compared_arguments: usize,
    /// Module arguments that differ from what the evaluator uses.
    pub argument_mismatches: Vec<String>,
    /// Why the download image could not be built, if so.
    pub image_error: Option<String>,
}

/// Group address by the id the project refers to it with.
type GaIds = HashMap<String, GroupAddress>;

// ---------------------------------------------------------------------------
// Device conversion
// ---------------------------------------------------------------------------

struct ConvertedDevice {
    instance: DeviceInstance,
    warnings: Vec<String>,
    compared_objects: usize,
    visibility_mismatches: Vec<String>,
    compared_arguments: usize,
    argument_mismatches: Vec<String>,
    image_error: Option<String>,
    /// A coupler's parameter values; its image is built once all devices are
    /// known.
    coupler_values: Option<HashMap<String, String>>,
}

/// Where a project `RefId` points to in the application: the scope and the
/// full id of the `ParameterRef`/`ComObjectRef`.
///
/// Device level: `M-XXXX_A-YYYY_P-1_R-1` or `O-2_R-31` (ETS 5.7 leaves out the
/// application prefix on objects). Module level: `MD-2_M-2_MI-1_P-26_R-1`,
/// where `MD-2_M-2` is the instance and the ref is `MD-2_P-26_R-1` of the module
/// definition. Repeated instances (`MI-2`, ...) are not known to the evaluator
/// and stay unresolved, as does anything that is not a ref of that scope.
fn locate(ev: &Evaluation, ap: &ApplicationProgram, ref_id: &str) -> Option<(usize, String)> {
    let bare = short_key(ap, ref_id);
    let Some((instance, rest)) = bare.split_once("_MI-1_") else {
        return Some((0, full_key(ap, &bare)));
    };
    let definition = instance.split("_M-").next()?;
    let scope = ev.scope_index(&full_key(ap, instance))?;
    Some((scope, full_key(ap, &format!("{definition}_{rest}"))))
}

/// One value of a parameter, from one of its refs.
struct RefValue {
    scope: usize,
    ref_id: String,
    value: String,
}

/// The value of every parameter of a device. A parameter with several refs can
/// have a different value per ref (`P-20008_R-20018` = 100 next to
/// `P-20008_R-123` = 10). The model stores one value per parameter: the one of
/// the ref that is active.
fn parameter_values(
    dev: &XmlDevice,
    ap: &ApplicationProgram,
    warnings: &mut Vec<String>,
) -> BTreeMap<String, String> {
    let ev = Evaluation::new(ap, &HashMap::new());
    let mut by_key: BTreeMap<String, Vec<RefValue>> = BTreeMap::new();
    for p in dev.parameters.iter().flat_map(|p| &p.items) {
        let Some(value) = p.value.clone() else { continue };
        let target = locate(&ev, ap, &p.ref_id).and_then(|(scope, ref_id)| {
            let param = ev.scopes[scope].parameter_ref(&ref_id)?.ref_id.clone();
            Some((short_key(ap, &ev.scopes[scope].key(&param)), scope, ref_id))
        });
        let Some((key, scope, ref_id)) = target else {
            warnings.push(format!(
                "parameter {} = {value:?} is not in {}, the value is lost",
                p.ref_id, ap.id
            ));
            continue;
        };
        by_key.entry(key).or_default().push(RefValue { scope, ref_id, value });
    }

    let mut values = BTreeMap::new();
    let mut conflicts = BTreeMap::new();
    for (key, refs) in by_key {
        if refs.windows(2).all(|w| w[0].value == w[1].value) {
            values.insert(key, refs.into_iter().next().unwrap().value);
        } else {
            conflicts.insert(key, refs);
        }
    }
    if conflicts.is_empty() {
        return values;
    }

    // Which ref is active can itself depend on these values, so evaluate with
    // the unambiguous ones, decide, and decide once more with the result.
    let mut chosen = BTreeMap::new();
    for _ in 0..2 {
        let mut user: HashMap<String, String> = values
            .iter()
            .chain(chosen.iter())
            .map(|(k, v): (&String, &String)| (full_key(ap, k), v.clone()))
            .collect();
        for (k, refs) in &conflicts {
            user.entry(full_key(ap, k)).or_insert_with(|| refs[0].value.clone());
        }
        let ev = Evaluation::new(ap, &user);
        for (key, refs) in &conflicts {
            let active: Vec<&RefValue> =
                refs.iter().filter(|r| ev.is_ref_active(r.scope, &r.ref_id)).collect();
            let distinct: HashSet<&str> = active.iter().map(|r| r.value.as_str()).collect();
            chosen.insert(key.clone(), active.first().unwrap_or(&&refs[0]).value.clone());
            if distinct.len() > 1 {
                warnings.push(format!(
                    "parameter {key} has different values on several active refs, using {}",
                    active[0].value
                ));
            }
        }
    }
    warnings.dedup();
    values.extend(chosen);
    values
}

fn convert_device(
    dev: &XmlDevice,
    ia: IndividualAddress,
    product: &ResolvedProduct,
    ga_ids: &GaIds,
) -> ConvertedDevice {
    let ap = &product.application;
    let mut warnings = Vec::new();
    let values = parameter_values(dev, ap, &mut warnings);
    let user: HashMap<String, String> =
        values.iter().map(|(k, v)| (full_key(ap, k), v.clone())).collect();
    let ev = Evaluation::new(ap, &user);
    warnings.extend(ev.warnings.iter().cloned());

    let active: HashSet<(usize, String)> = ev
        .active_com_object_refs()
        .map(|(scope, r)| (scope, r.id.clone()))
        .collect();

    let translations: Vec<_> = ap.languages.languages.iter().map(|l| ap.translations(&l.identifier)).collect();
    let mut links: Vec<GroupObjectLink> = Vec::new();
    let mut objects: Vec<ObjectSettings> = Vec::new();
    let mut compared_objects = 0;
    let mut visibility_mismatches = Vec::new();
    let mut compared: HashSet<(usize, String)> = HashSet::new();
    for object in dev.objects.iter().flat_map(|o| &o.items) {
        let resolved = locate(&ev, ap, &object.ref_id).and_then(|(scope, ref_id)| {
            let com_ref = ev.scopes[scope].com_object_ref(&ref_id)?;
            let (asap, resolved) = tables::resolve_com_object_ref(&ev, scope, com_ref)?;
            Some((scope, ref_id, asap, resolved))
        });
        let connected = connected_addresses(object);
        let Some((scope, ref_id, asap, resolved)) = resolved else {
            warnings.push(format!(
                "group object {} is not in {}{}",
                object.ref_id,
                ap.id,
                if connected.is_empty() { "" } else { ", its links are lost" },
            ));
            continue;
        };

        let is_active = active.contains(&(scope, ref_id.clone()));
        if let Some(ets_active) = object.is_active {
            compared_objects += 1;
            compared.insert((scope, ref_id.clone()));
            if ets_active != is_active {
                visibility_mismatches.push(format!(
                    "group object #{asap} ({}): ETS has it {}, the evaluator {}",
                    object.ref_id,
                    active_word(ets_active),
                    active_word(is_active),
                ));
            }
        } else if !connected.is_empty() {
            // ETS 5.7 has no `IsActive`, but only an active object has links.
            compared_objects += 1;
            if !is_active {
                visibility_mismatches.push(format!(
                    "group object #{asap} ({}) has links in ETS, the evaluator has it inactive",
                    object.ref_id
                ));
            }
        }
        if object.is_active != Some(false) {
            let texts = ev.scopes[scope]
                .com_object_ref(&ref_id)
                .map(|r| ProductTexts::of(&translations, r, &resolved))
                .unwrap_or_default();
            let settings = object_settings(object, asap, &resolved, &texts);
            if !settings.is_empty() {
                objects.push(settings);
            }
        } else if !connected.is_empty() {
            warnings.push(format!(
                "group object #{asap} ({}) is inactive in ETS, its links were not imported",
                object.ref_id
            ));
            continue;
        }

        for (id, send) in connected {
            let Some(&ga) = ga_ids.get(id) else {
                warnings.push(format!("group object #{asap} links to unknown group address {id}"));
                continue;
            };
            add_link(&mut links, asap, ga, send, &mut warnings);
        }
    }

    // The group object tree only lists active objects, but not all of them.
    let mut listed = Vec::new();
    if let Some(nodes) = dev.object_tree.as_ref().and_then(|t| t.nodes.as_ref()) {
        nodes.collect(&mut listed);
    }
    for ref_id in listed {
        compared_objects += 1;
        if !locate(&ev, ap, ref_id).is_some_and(|target| active.contains(&target)) {
            visibility_mismatches.push(format!(
                "group object {ref_id} is in the ETS group object tree, the evaluator has it inactive"
            ));
        }
    }

    let (compared_arguments, argument_mismatches) = compare_arguments(dev, ap, &ev);
    warnings.extend(argument_mismatches.iter().cloned());

    // The objects ETS lists as inactive are covered above. One it does not
    // list at all but the evaluator activates is a mismatch only if ETS stated
    // visibility for this device.
    if !compared.is_empty() {
        for (scope, com_ref) in ev.active_com_object_refs() {
            if !compared.contains(&(scope, com_ref.id.clone())) {
                visibility_mismatches.push(format!(
                    "group object {}: the evaluator has it active, ETS does not list it",
                    short_key(ap, &com_ref.id)
                ));
            }
        }
    }
    warnings.extend(visibility_mismatches.iter().cloned());

    let table_links: Vec<Link> = links
        .iter()
        .map(|l| Link { asap: l.object_number, ga: l.ga.to_u16(), send: l.send })
        .collect();
    let coupler = product.table_format == Ok(TableFormat::Coupler);
    let image = product
        .table_format
        .clone()
        .map_err(|e| anyhow!(e))
        .and_then(|format| {
            knx_prod::device_image::build_device_image_with(&ev, format, &table_links, &object_overrides(&objects))
        });
    let image_error = match image {
        _ if coupler => None, // built in `convert`, once all links are known
        Ok(image) => {
            warnings.extend(image.warnings);
            None
        }
        Err(e) => {
            warnings.push(format!("no download image can be built: {e:#}"));
            Some(format!("{e:#}"))
        }
    };

    ConvertedDevice {
        instance: DeviceInstance {
            ia,
            parameters: values
                .into_iter()
                .map(|(id, value)| ParameterValue { id, value: serde_yaml::Value::String(value) })
                .collect(),
            links,
            objects,
        },
        warnings,
        compared_objects,
        visibility_mismatches,
        compared_arguments,
        argument_mismatches,
        image_error,
        coupler_values: coupler.then_some(user),
    }
}

/// Compare the module arguments ETS recorded with the ones the evaluator uses.
/// Numeric arguments are the base numbers of the instance's objects and
/// parameters, so a difference means wrong object numbers.
fn compare_arguments(
    dev: &XmlDevice,
    ap: &ApplicationProgram,
    ev: &Evaluation,
) -> (usize, Vec<String>) {
    let mut compared = 0;
    let mut mismatches = Vec::new();
    for instance in dev.modules.iter().flat_map(|m| &m.items) {
        let Some(scope) = ev.scope_index(&full_key(ap, &instance.ref_id)) else {
            mismatches.push(format!("module instance {} is not in {}", instance.ref_id, ap.id));
            continue;
        };
        for arg in instance.arguments.iter().flat_map(|a| &a.items) {
            compared += 1;
            let ours = ev.scopes[scope].arg_value(&full_key(ap, &arg.ref_id));
            if ours != Some(arg.value.as_str()) {
                mismatches.push(format!(
                    "module instance {}: argument {} is {:?} in ETS, {:?} in the evaluator",
                    instance.ref_id, arg.ref_id, arg.value, ours
                ));
            }
        }
    }
    (compared, mismatches)
}

fn active_word(active: bool) -> &'static str {
    if active {
        "active"
    } else {
        "inactive"
    }
}

/// The group address ids of an object, with whether it sends on them.
fn connected_addresses(object: &ComObjectInstanceRef) -> Vec<(&str, bool)> {
    let mut out = Vec::new();
    if let Some(c) = &object.connectors {
        out.extend(c.send.iter().map(|s| (s.group_address.as_str(), true)));
        out.extend(c.receive.iter().map(|r| (r.group_address.as_str(), false)));
    }
    out.extend(object.links.split_whitespace().enumerate().map(|(i, id)| (id, i == 0)));
    out
}

fn add_link(
    links: &mut Vec<GroupObjectLink>,
    object_number: u16,
    ga: GroupAddress,
    send: bool,
    warnings: &mut Vec<String>,
) {
    let has_sender = links.iter().any(|l| l.object_number == object_number && l.send);
    let send = if send && has_sender {
        warnings.push(format!(
            "group object #{object_number} has several sending addresses, {ga} is linked as receiving"
        ));
        false
    } else {
        send
    };
    match links.iter_mut().find(|l| l.object_number == object_number && l.ga == ga) {
        Some(l) => l.send |= send,
        None => links.push(GroupObjectLink { object_number, ga, send }),
    }
}

/// The flags and priority the project sets for an object where they differ
/// from the product's.
/// An object's `Text` and `FunctionText` in all languages of the product, as
/// templates (`Input {{ChNo}}: {{0}}`).
#[derive(Default)]
struct ProductTexts {
    text: Vec<String>,
    function_text: Vec<String>,
}

impl ProductTexts {
    fn of(translations: &[HashMap<String, HashMap<String, String>>], r: &knx_prod::ComObjectRef, resolved: &GroupObject) -> Self {
        let collect = |attr: &str, raw: &str| {
            let mut out = vec![raw.to_string()];
            for t in translations {
                let get = |id: &str| t.get(id).and_then(|a| a.get(attr));
                if let Some(text) = get(&r.id).or_else(|| get(&r.ref_id)) {
                    out.push(text.clone());
                }
            }
            out
        };
        ProductTexts {
            text: collect("Text", &resolved.text),
            function_text: collect("FunctionText", &resolved.function_text),
        }
    }
}

/// Whether `text` is `template` with its `{{…}}` placeholders filled in.
fn fills_template(template: &str, text: &str) -> bool {
    let mut literals = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let Some(end) = rest[start..].find("}}") else { break };
        literals.push(&rest[..start]);
        rest = &rest[start + end + 2..];
    }
    literals.push(rest);
    let [first, middle @ .., last] = literals.as_slice() else { return template == text };
    if literals.len() == 1 {
        return template == text;
    }
    let Some(mut tail) = text.strip_prefix(first) else { return false };
    for lit in middle {
        match tail.find(lit) {
            Some(i) => tail = &tail[i + lit.len()..],
            None => return false,
        }
    }
    tail.ends_with(last)
}

fn object_settings(object: &ComObjectInstanceRef, asap: u16, resolved: &GroupObject, texts: &ProductTexts) -> ObjectSettings {
    let f = resolved.flags;
    let differs = |set: &str, product: bool| match set {
        "Enabled" if !product => Some(true),
        "Disabled" if product => Some(false),
        _ => None,
    };
    // A product that states no priority means Low: ETS records `Low` for
    // those objects.
    let product_priority = resolved.priority.or(tables::priority_bits("Low"));
    let priority = match object.priority.as_str() {
        "" => None,
        p if tables::priority_bits(p) == product_priority => None,
        p => p.parse::<Priority>().ok(),
    };
    // ETS writes the text it shows, in the project's language with the
    // placeholders filled in; only a text no product language explains was
    // set by the user.
    let own_text = |set: &Option<String>, product: &[String]| {
        set.as_ref().filter(|t| !product.iter().any(|p| fills_template(p, t))).cloned()
    };
    let product_dpt = resolved.datapoint_type.split_whitespace().next().unwrap_or("");
    ObjectSettings {
        object_number: asap,
        text: own_text(&object.text, &texts.text),
        function_text: own_text(&object.function_text, &texts.function_text),
        dpt: object.datapoint_type.as_ref().filter(|d| d.split_whitespace().next() != Some(product_dpt)).cloned(),
        communication: differs(&object.communication_flag, f.communicate),
        read: differs(&object.read_flag, f.read),
        write: differs(&object.write_flag, f.write),
        transmit: differs(&object.transmit_flag, f.transmit),
        update: differs(&object.update_flag, f.update),
        read_on_init: differs(&object.read_on_init_flag, f.read_on_init),
        priority,
    }
}

/// The table builder's form of a device's object settings.
pub fn object_overrides(objects: &[ObjectSettings]) -> Vec<ObjectOverride> {
    objects
        .iter()
        .map(|o| ObjectOverride {
            asap: o.object_number,
            communicate: o.communication,
            read: o.read,
            write: o.write,
            transmit: o.transmit,
            update: o.update,
            read_on_init: o.read_on_init,
            priority: o.priority.and_then(|p| tables::priority_bits(p.ets_name())),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Archive
// ---------------------------------------------------------------------------

fn open_archive(path: &Path) -> Result<zip::ZipArchive<File>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    zip::ZipArchive::new(file).with_context(|| format!("reading {} as a ZIP archive", path.display()))
}

/// The project's `project.xml` and installation file among the archive entries.
fn project_entries(names: &[String]) -> Result<(String, String)> {
    let is_project_dir = |d: &str| d.starts_with("P-") && !d.contains('.');
    if let Some(packed) = names
        .iter()
        .find(|n| n.starts_with("P-") && n.ends_with(".zip") && !n.contains('/'))
    {
        bail!(
            "the project is stored in {packed}, which means it is password protected; \
             export it from ETS without a password"
        );
    }
    let project_xml: Vec<&String> = names
        .iter()
        .filter(|n| n.split_once('/').is_some_and(|(d, f)| is_project_dir(d) && f == "project.xml"))
        .collect();
    let [project_xml] = project_xml[..] else {
        bail!(
            "expected exactly one P-XXXX/project.xml in the archive, found {}",
            project_xml.len()
        );
    };
    let dir = project_xml.split_once('/').map(|(d, _)| d).unwrap_or_default();
    let mut installations = names.iter().filter(|n| {
        n.strip_prefix(dir)
            .and_then(|r| r.strip_prefix('/'))
            .and_then(|f| f.strip_suffix(".xml"))
            .is_some_and(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
    });
    let installation = installations
        .next()
        .ok_or_else(|| anyhow!("no installation file ({dir}/0.xml) in the archive"))?;
    if installations.next().is_some() {
        bail!("the project has several installation files, only one is supported");
    }
    Ok((project_xml.clone(), installation.clone()))
}

fn read_entry(archive: &mut zip::ZipArchive<File>, name: &str) -> Result<Vec<u8>> {
    let mut entry = archive
        .by_name(name)
        .with_context(|| format!("reading ZIP entry {name}"))?;
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut buf)?;
    Ok(buf)
}

fn parse<T: serde::de::DeserializeOwned>(data: &[u8]) -> Result<T> {
    let text = std::str::from_utf8(data).context("not UTF-8")?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    Ok(quick_xml::de::from_str(text)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INSTALLATION: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<KNX xmlns="http://knx.org/xml/project/12"><Project Id="P-1"><Installations><Installation>
  <Topology><Area Address="1" Name="A"><Line Address="2" Name="L">
    <DeviceInstance Id="D1" Address="3" Hardware2ProgramRefId="H">
      <ParameterInstanceRefs>
        <ParameterInstanceRef RefId="APP_P-1_R-1" Value="1" />
      </ParameterInstanceRefs>
      <ComObjectInstanceRefs>
        <ComObjectInstanceRef RefId="APP_O-0_R-1" IsActive="true" Priority="Low">
          <Connectors><Send GroupAddressRefId="P-1-0_GA-1" /><Receive GroupAddressRefId="P-1-0_GA-2" /></Connectors>
        </ComObjectInstanceRef>
        <ComObjectInstanceRef RefId="O-1_R-2" Links="GA-2 GA-1" />
      </ComObjectInstanceRefs>
    </DeviceInstance>
    <DeviceInstance Id="D2" Hardware2ProgramRefId="H" />
  </Line></Area></Topology>
  <GroupAddresses><GroupRanges><GroupRange><GroupRange>
    <GroupAddress Id="P-1-0_GA-1" Address="2563" Name="one" DatapointType="DPST-1-1 DPST-1-1" />
    <GroupAddress Id="P-1-0_GA-2" Address="2564" Name="two" />
  </GroupRange></GroupRange></GroupRanges></GroupAddresses>
</Installation></Installations></Project></KNX>"#;

    fn entries(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn finds_the_project_entries() {
        let names = entries(&["knx_master.xml", "M-0083/Catalog.xml", "P-1/project.xml", "P-1/0.xml", "P-1.signature"]);
        assert_eq!(
            project_entries(&names).unwrap(),
            ("P-1/project.xml".to_string(), "P-1/0.xml".to_string())
        );
    }

    #[test]
    fn password_protected_projects_are_reported() {
        let names = entries(&["knx_master.xml", "P-1.zip", "P-1.signature"]);
        let err = project_entries(&names).unwrap_err().to_string();
        assert!(err.contains("password"), "{err}");
    }

    #[test]
    fn parses_an_installation() {
        let doc: InstallationDoc = parse(INSTALLATION.as_bytes()).unwrap();
        let inst = &doc.project.installations.items[0];
        let line = &inst.topology.as_ref().unwrap().areas[0].lines[0];
        assert_eq!(line.devices.len(), 2);
        assert_eq!(line.devices[1].address, None);
        let objects = &line.devices[0].objects.as_ref().unwrap().items;
        assert_eq!(
            connected_addresses(&objects[0]),
            [("P-1-0_GA-1", true), ("P-1-0_GA-2", false)]
        );
        assert_eq!(connected_addresses(&objects[1]), [("GA-2", true), ("GA-1", false)]);
        assert_eq!(objects[0].is_active, Some(true));
        assert_eq!(objects[1].is_active, None);
    }

    #[test]
    fn a_byte_order_mark_is_accepted() {
        let with_bom = format!("\u{feff}{INSTALLATION}");
        assert!(parse::<InstallationDoc>(with_bom.as_bytes()).is_ok());
    }

    #[test]
    fn locates_device_and_module_refs() {
        use knx_prod::{
            Dynamic, ModuleDef, ModuleDefs, ModuleInstance, ParameterRef, ParameterRefs, Static, UiNode,
        };
        let ap = ApplicationProgram {
            id: "M-1_A-1".into(),
            module_defs: Some(ModuleDefs {
                items: vec![ModuleDef {
                    id: "M-1_A-1_MD-2".into(),
                    static_: Some(Static {
                        parameter_refs: Some(ParameterRefs {
                            items: vec![ParameterRef {
                                id: "M-1_A-1_MD-2_P-26_R-1".into(),
                                ref_id: "M-1_A-1_MD-2_P-26".into(),
                                ..Default::default()
                            }],
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
            }),
            dynamic: Some(Dynamic {
                nodes: vec![UiNode::Module(ModuleInstance {
                    id: "M-1_A-1_MD-2_M-2".into(),
                    ref_id: "M-1_A-1_MD-2".into(),
                    ..Default::default()
                })],
            }),
            ..Default::default()
        };
        let ev = Evaluation::new(&ap, &HashMap::new());
        let scope = ev.scope_index("M-1_A-1_MD-2_M-2").unwrap();
        let module_ref = Some((scope, "M-1_A-1_MD-2_P-26_R-1".to_string()));

        // The application prefix is optional.
        assert_eq!(locate(&ev, &ap, "M-1_A-1_P-1_R-1"), Some((0, "M-1_A-1_P-1_R-1".to_string())));
        assert_eq!(locate(&ev, &ap, "P-1_R-1"), Some((0, "M-1_A-1_P-1_R-1".to_string())));
        // ETS writes parameters with the prefix and objects without it.
        assert_eq!(locate(&ev, &ap, "MD-2_M-2_MI-1_P-26_R-1"), module_ref);
        assert_eq!(locate(&ev, &ap, "M-1_A-1_MD-2_M-2_MI-1_P-26_R-1"), module_ref);
        // An instance the application does not have.
        assert_eq!(locate(&ev, &ap, "MD-2_M-9_MI-1_P-26_R-1"), None);
        // A repeated instance is not resolved into the first one: it is looked
        // up as a device ref, which does not exist, and so gets reported.
        let (scope, id) = locate(&ev, &ap, "MD-2_M-2_MI-2_P-26_R-1").unwrap();
        assert!(ev.scopes[scope].parameter_ref(&id).is_none());
    }

    #[test]
    fn only_one_sending_address_per_object() {
        let ga = |s: &str| s.parse::<GroupAddress>().unwrap();
        let mut links = Vec::new();
        let mut warnings = Vec::new();
        add_link(&mut links, 1, ga("1/0/1"), true, &mut warnings);
        add_link(&mut links, 1, ga("1/0/2"), true, &mut warnings);
        add_link(&mut links, 1, ga("1/0/1"), false, &mut warnings);
        assert_eq!(links.len(), 2);
        assert!(links[0].send);
        assert!(!links[1].send);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn object_settings_keep_what_differs_from_the_product() {
        let mut doc: InstallationDoc = parse(INSTALLATION.as_bytes()).unwrap();
        let line = &mut doc.project.installations.items[0].topology.as_mut().unwrap().areas[0].lines[0];
        let mut object = line.devices[0].objects.as_mut().unwrap().items.remove(0);
        let product = GroupObject {
            flags: tables::GroupObjectFlags { communicate: true, write: true, ..Default::default() },
            value_field_type: 0,
            priority: None,
            text: String::new(),
            function_text: String::new(),
            datapoint_type: String::new(),
            object_size: String::new(),
        };
        // `Priority="Low"` equals a product without a priority.
        assert!(object_settings(&object, 4, &product, &ProductTexts::default()).is_empty());

        object.write_flag = "Enabled".into();
        object.transmit_flag = "Enabled".into();
        object.communication_flag = "Disabled".into();
        object.priority = "Alert".into();
        let settings = object_settings(&object, 4, &product, &ProductTexts::default());
        assert_eq!(
            settings,
            ObjectSettings {
                object_number: 4,
                communication: Some(false),
                transmit: Some(true),
                priority: Some(Priority::Alert),
                ..Default::default()
            }
        );
        let o = object_overrides(&[settings])[0];
        let mut obj = product.clone();
        o.apply(&mut obj);
        assert!(!obj.flags.communicate && obj.flags.transmit && obj.flags.write);
        assert_eq!(obj.priority, Some(0b10));
    }
}
