//! `knxcfg` — a git-friendly KNX configuration tool (ETS replacement).

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use knx_core::{
    Area, DeviceInstance, DeviceRef, GroupAddress, GroupAddressEntry, GroupObjectLink,
    IndividualAddress, Line, ParameterValue,
};
use std::path::{Path, PathBuf};

mod product_store;
mod tui;

#[derive(Parser)]
#[command(name = "knxcfg", version, about = "Git-friendly KNX configuration tool")]
struct Cli {
    /// Run as if started in <DIR> (like `git -C <DIR>`)
    #[arg(short = 'C', long = "project", global = true)]
    project: Option<PathBuf>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a new empty project directory
    Init { dir: PathBuf },
    /// Print the contents of the project (current dir or above)
    List,
    /// Copy knxprod files into the project's vendor/ store (or the global store)
    ImportProduct {
        /// One or more .knxprod files
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Copy into the global store instead of <project>/vendor/
        #[arg(long)]
        global: bool,
    },
    /// List products (whole product store, or a single knxprod file)
    Catalog {
        /// Optional knxprod file; omit to list the whole product store
        file: Option<PathBuf>,
    },
    /// Dump the parameters and group objects of an application program
    Probe {
        /// Hardware2ProgramRefId (e.g. M-0083_H-180-0_HP-00B4-32-3919)
        h2p: String,
        /// knxprod file override (auto-resolved from the product store)
        #[arg(long)]
        knxprod: Option<PathBuf>,
    },
    /// Print the dynamic parameter form (visibility resolved, defaults)
    Form {
        /// Hardware2ProgramRefId
        h2p: String,
        #[arg(long)]
        knxprod: Option<PathBuf>,
    },
    /// Build the device memory image from the knxprod defaults (M5)
    Memory {
        /// Hardware2ProgramRefId
        h2p: String,
        #[arg(long)]
        knxprod: Option<PathBuf>,
    },
    /// Build the group address / association / group object tables (M5)
    Tables {
        /// Hardware2ProgramRefId
        h2p: String,
        #[arg(long)]
        knxprod: Option<PathBuf>,
    },
    /// Add a device from the product store to the topology
    Add {
        /// Hardware2ProgramRefId (from `knxcfg catalog`)
        h2p: String,
        /// Individual address, e.g. 1.1.1
        ia: String,
        #[arg(long)]
        name: Option<String>,
        /// knxprod file override (auto-resolved from the product store)
        #[arg(long)]
        knxprod: Option<PathBuf>,
    },
    /// Remove a device from the topology
    Rm { ia: String },
    /// Add a group address
    GroupAdd {
        /// Group address, e.g. 1/2/3
        ga: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        dpt: Option<String>,
    },
    /// Remove a group address
    GroupRm { ga: String },
    /// Link a device group object to a group address
    Link {
        ia: String,
        /// Group object number
        object: u16,
        ga: String,
    },
    /// Remove a link
    Unlink {
        ia: String,
        object: u16,
        ga: String,
    },
    /// Set a device parameter value
    DeviceSet {
        ia: String,
        /// Parameter id (or `module-id/param-id` for module parameters)
        param: String,
        value: String,
    },
    /// Interactively edit a device's parameters (TUI)
    Edit {
        ia: String,
        /// knxprod file (defaults to the one recorded at `add` time)
        #[arg(long)]
        knxprod: Option<PathBuf>,
    },
    /// Export the project to an ETS .knxproj file (M3)
    Export,
    /// Import an ETS .knxproj file (M2)
    Import { file: PathBuf },
    /// Build and (M4: send) the download image for a device (M5 dry run)
    Download {
        /// Individual address, e.g. 1.1.1
        ia: String,
        /// knxprod file (defaults to the one recorded at `add` time)
        #[arg(long)]
        knxprod: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let Cli { project, command } = Cli::parse();
    match command {
        Cmd::Init { dir } => cmd_init(&dir),
        Cmd::List => cmd_list(&resolve_project(project.as_deref())?),
        Cmd::ImportProduct { files, global } => {
            let dest = if global {
                product_store::global_store_dir()
            } else {
                let dir = resolve_project(project.as_deref())?;
                product_store::vendor_dir(&dir)
            };
            product_store::import_files(&files, &dest)
        }
        Cmd::Catalog { file } => {
            let project_dir = discover_project(project.as_deref());
            cmd_catalog(file.as_deref(), project_dir.as_deref())
        }
        Cmd::Probe { h2p, knxprod } => {
            let file = product_store::resolve(
                &h2p,
                discover_project(project.as_deref()).as_deref(),
                None,
                knxprod.as_deref(),
            )?;
            cmd_probe(&file, &h2p)
        }
        Cmd::Form { h2p, knxprod } => {
            let file = product_store::resolve(
                &h2p,
                discover_project(project.as_deref()).as_deref(),
                None,
                knxprod.as_deref(),
            )?;
            cmd_form(&file, &h2p)
        }
        Cmd::Memory { h2p, knxprod } => {
            let file = product_store::resolve(
                &h2p,
                discover_project(project.as_deref()).as_deref(),
                None,
                knxprod.as_deref(),
            )?;
            cmd_memory(&file, &h2p)
        }
        Cmd::Tables { h2p, knxprod } => {
            let file = product_store::resolve(
                &h2p,
                discover_project(project.as_deref()).as_deref(),
                None,
                knxprod.as_deref(),
            )?;
            cmd_tables(&file, &h2p)
        }
        Cmd::Add { h2p, ia, name, knxprod } => {
            let dir = resolve_project(project.as_deref())?;
            let file = product_store::resolve(&h2p, Some(&dir), None, knxprod.as_deref())?;
            cmd_add(&dir, &file, &h2p, &ia, name.as_deref())
        }
        Cmd::Rm { ia } => {
            let dir = resolve_project(project.as_deref())?;
            cmd_rm(&dir, &ia)
        }
        Cmd::GroupAdd { ga, name, dpt } => {
            let dir = resolve_project(project.as_deref())?;
            cmd_group_add(&dir, &ga, name.as_deref(), dpt.as_deref())
        }
        Cmd::GroupRm { ga } => {
            let dir = resolve_project(project.as_deref())?;
            cmd_group_rm(&dir, &ga)
        }
        Cmd::Link { ia, object, ga } => {
            let dir = resolve_project(project.as_deref())?;
            cmd_link(&dir, &ia, object, &ga)
        }
        Cmd::Unlink { ia, object, ga } => {
            let dir = resolve_project(project.as_deref())?;
            cmd_unlink(&dir, &ia, object, &ga)
        }
        Cmd::DeviceSet { ia, param, value } => {
            let dir = resolve_project(project.as_deref())?;
            cmd_device_set(&dir, &ia, &param, &value)
        }
        Cmd::Edit { ia, knxprod } => {
            let dir = resolve_project(project.as_deref())?;
            cmd_edit(&dir, &ia, knxprod.as_deref())
        }
        Cmd::Export => bail!("`export` is not implemented yet (M3)"),
        Cmd::Import { .. } => bail!("`import` is not implemented yet (M2)"),
        Cmd::Download { ia, knxprod } => {
            let dir = resolve_project(project.as_deref())?;
            cmd_download(&dir, &ia, knxprod.as_deref())
        }
    }
}

/// Locate the project root: `--project` if given, otherwise walk up from the
/// current directory looking for `knx.yaml` (like git finds `.git`).
fn resolve_project(flag: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = flag {
        return Ok(p.to_path_buf());
    }
    let start = std::env::current_dir().context("getting current directory")?;
    for dir in start.ancestors() {
        if dir.join("knx.yaml").is_file() {
            return Ok(dir.to_path_buf());
        }
    }
    bail!(
        "not in a knxcfg project (no knx.yaml found in {} or any parent)",
        start.display()
    )
}

/// Like [`resolve_project`], but returns `None` instead of failing when there
/// is no project. Used by inspection commands, which also search the global
/// product store and therefore do not require a project.
fn discover_project(flag: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = flag {
        return Some(p.to_path_buf());
    }
    let start = std::env::current_dir().ok()?;
    start
        .ancestors()
        .find(|d| d.join("knx.yaml").is_file())
        .map(PathBuf::from)
}

fn cmd_init(dir: &PathBuf) -> Result<()> {
    if dir.exists() && std::fs::read_dir(dir)?.next().is_some() {
        bail!("{} is not empty", dir.display());
    }
    let project = knx_core::Project::default();
    knx_store::save(dir, &project)?;
    println!("Created empty project in {}", dir.display());
    Ok(())
}

fn cmd_list(dir: &Path) -> Result<()> {
    let project = knx_store::load(dir)
        .with_context(|| format!("loading project from {}", dir.display()))?;
    println!("Project: {} (format v{})", project.meta.name, project.meta.format_version);

    println!("\nTopology:");
    if project.topology.is_empty() {
        println!("  (empty)");
    }
    for area in &project.topology {
        println!("  Area {}  {}", area.number, area.name);
        for line in &area.lines {
            println!("    Line {}  {}", line.number, line.name);
            for dev in &line.devices {
                println!("      {} {}  [{}]", dev.ia, dev.name, dev.hardware);
            }
        }
    }

    println!("\nGroup addresses:");
    if project.groups.is_empty() {
        println!("  (empty)");
    }
    for g in &project.groups {
        match &g.dpt {
            Some(dpt) => println!("  {}  {}  ({})", g.ga, g.name, dpt),
            None => println!("  {}  {}", g.ga, g.name),
        }
    }

    println!("\nDevice instances:");
    if project.devices.is_empty() {
        println!("  (none)");
    }
    for dev in &project.devices {
        println!(
            "  {}  {} parameter(s), {} link(s)",
            dev.ia,
            dev.parameters.len(),
            dev.links.len()
        );
    }
    Ok(())
}

fn cmd_catalog(file: Option<&Path>, project_dir: Option<&Path>) -> Result<()> {
    if let Some(f) = file {
        let knxprod = knx_prod::KnxProd::open(f)
            .with_context(|| format!("reading {}", f.display()))?;
        print_knxprod(f, &knxprod);
        return Ok(());
    }

    let mut dirs: Vec<(&str, PathBuf)> = Vec::new();
    if let Some(p) = project_dir {
        dirs.push(("vendor", product_store::vendor_dir(p)));
    }
    dirs.push(("store", product_store::global_store_dir()));

    let mut any = false;
    for (label, dir) in dirs {
        for f in product_store::knxprod_files(&dir)? {
            any = true;
            println!("[{label}] {}", f.display());
            let knxprod = knx_prod::KnxProd::open(&f)
                .with_context(|| format!("reading {}", f.display()))?;
            print_knxprod(&f, &knxprod);
            println!();
        }
    }
    if !any {
        println!(
            "no knxprod files found; import one with `knxcfg import-product <file>`\nstore: {}",
            product_store::global_store_dir().display()
        );
    }
    Ok(())
}

fn print_knxprod(_file: &Path, knxprod: &knx_prod::KnxProd) {
    println!("Manufacturer: {}", knxprod.manufacturer_id);
    println!("{} product(s):", knxprod.products.len());
    for p in &knxprod.products {
        println!(
            "  {}  {}  [{}]",
            p.number,
            p.name,
            p.hardware2program_ref_id.as_deref().unwrap_or("?")
        );
    }
}

fn cmd_probe(file: &PathBuf, h2p: &str) -> Result<()> {
    use std::collections::HashMap;

    let knxprod = knx_prod::KnxProd::open(file)
        .with_context(|| format!("reading {}", file.display()))?;
    let ap = knxprod
        .application_program(h2p)
        .with_context(|| format!("resolving {h2p}"))?;

    println!("Application program: {} ({})", ap.name, ap.id);
    println!(
        "  number={}  version={}  mask={}  type={}  load={}",
        ap.application_number,
        ap.application_version,
        ap.mask_version,
        ap.program_type,
        ap.load_procedure_style
    );

    let static_ = ap.static_.clone().unwrap_or_default();

    let mut types: HashMap<String, knx_prod::ParameterType> = HashMap::new();
    if let Some(pt) = &static_.parameter_types {
        for t in &pt.items {
            types.insert(t.id.clone(), t.clone());
        }
    }

    let parameters = static_.parameters.clone().unwrap_or_default();
    let device_objects = all_com_objects(&static_);

    println!(
        "\nParameter types: {}   Parameters: {}   Unions: {}   Group objects: {}",
        types.len(),
        parameters.parameters().count(),
        parameters.unions().count(),
        device_objects.len()
    );

    println!("\n--- Parameters ---");
    for p in parameters.parameters() {
        let (kind, size) = match types.get(p.parameter_type.as_str()) {
            Some(t) => (t.kind(), t.size_in_bit()),
            None => ("?", None),
        };
        let size = size.map(|s| format!("{s}b")).unwrap_or_default();
        println!(
            "  {:<48} {:<7} {:<4} default={:<8} {}",
            p.id, kind, size, p.value, p.text
        );
    }

    println!("\n--- Unions ---");
    for u in parameters.unions() {
        println!(
            "  union ({} bit, {} alternatives)",
            u.size_in_bit,
            u.parameters.len()
        );
        for p in &u.parameters {
            println!("      {:<46} default={}", p.id, p.value);
        }
    }

    println!("\n--- Group objects ---");
    for o in &device_objects {
        println!(
            "  #{:<4} {:<32} {:<10} DPT={:<12} flags=R{}W{}C{}T{}U{}  {}",
            o.number,
            o.name,
            o.object_size,
            o.datapoint_type,
            mark(&o.read_flag),
            mark(&o.write_flag),
            mark(&o.communication_flag),
            mark(&o.transmit_flag),
            mark(&o.update_flag),
            o.function_text
        );
    }

    if let Some(mods) = &ap.module_defs {
        println!("\n--- Module definitions ({}) ---", mods.items.len());
        for m in &mods.items {
            let s = m.static_.clone().unwrap_or_default();
            let params = s.parameters.clone().unwrap_or_default();
            let mcom = all_com_objects(&s);
            println!(
                "  {}  ({} params, {} group objects)",
                m.name,
                params.parameters().count(),
                mcom.len()
            );
            for o in &mcom {
                println!(
                    "      #{:<4} {:<30} {:<10} DPT={:<12} {}",
                    o.number, o.name, o.object_size, o.datapoint_type, o.function_text
                );
            }
        }
    }
    Ok(())
}

fn mark(flag: &str) -> char {
    if flag == "Enabled" { '+' } else { '-' }
}

fn cmd_form(file: &PathBuf, h2p: &str) -> Result<()> {
    use std::collections::HashMap;

    let knxprod = knx_prod::KnxProd::open(file)
        .with_context(|| format!("reading {}", file.display()))?;
    let ap = knxprod
        .application_program(h2p)
        .with_context(|| format!("resolving {h2p}"))?;

    let values: HashMap<String, String> = HashMap::new();
    let lang = if ap.default_language.is_empty() { "en-US" } else { &ap.default_language };
    let sections = knx_form::evaluate(&ap, &values, lang);
    println!("{} ({} page(s))", ap.name, sections.len());
    for page in &sections {
        println!("\n# {}", page.title);
        for section in &page.sections {
            match &section.title {
                Some(t) => println!("  [{}]", t),
                None => {}
            }
            for field in &section.fields {
            let kind = match &field.kind {
                knx_form::FieldKind::Checkbox { off, on } => {
                    format!("bool [{}|{}]", off.label, on.label)
                }
                knx_form::FieldKind::Enum { options } => {
                    let labels: Vec<&str> = options.iter().map(|o| o.label.as_str()).collect();
                    format!("enum [{}]", labels.join("|"))
                }
                knx_form::FieldKind::Number { min, max, unit } => {
                    let range = match (min, max) {
                        (Some(lo), Some(hi)) => format!("{lo}..{hi}"),
                        (Some(lo), None) => format!("{lo}.."),
                        (None, Some(hi)) => format!("..{hi}"),
                        (None, None) => String::new(),
                    };
                    match unit {
                        Some(u) => format!("num {range} {u}"),
                        None => format!("num {range}"),
                    }
                }
                knx_form::FieldKind::Text => "text".to_string(),
            };
            let value = match &field.kind {
                knx_form::FieldKind::Checkbox { off, on } => {
                    if field.value == on.value {
                        on.label.clone()
                    } else if field.value == off.value {
                        off.label.clone()
                    } else {
                        field.value.clone()
                    }
                }
                _ => field.value.clone(),
            };
            println!("    {:<60} {:<24} = {}", field.label, kind, value);
            }
        }
    }
    Ok(())
}

fn cmd_memory(file: &PathBuf, h2p: &str) -> Result<()> {
    use std::collections::HashMap;

    let knxprod = knx_prod::KnxProd::open(file)
        .with_context(|| format!("reading {}", file.display()))?;
    let ap = knxprod
        .application_program(h2p)
        .with_context(|| format!("resolving {h2p}"))?;

    let values: HashMap<String, String> = HashMap::new();
    let image = knx_prod::build_parameter_image(&ap, &values)?;

    println!("Memory image: {} ({} segment(s))", ap.name, image.segments.len());
    for seg in &image.segments {
        let addr = if seg.address.is_empty() {
            "-".to_string()
        } else {
            format!("0x{:04x}", seg.address.parse::<u32>().unwrap_or(0))
        };
        let mem = if seg.memory_type.is_empty() { "flash" } else { &seg.memory_type };
        println!(
            "  {:<44} {}  {:>5} bytes  {}",
            seg.id, addr, seg.size, mem
        );
    }

    println!("\nParameter placements ({}):", image.placements.len());
    for p in &image.placements {
        println!(
            "  {:<40} {:<40} byte {:<4} bit {:<2} {} bit = {}",
            p.param_id, p.segment_id, p.offset, p.bit_offset, p.bit_len, p.value
        );
    }

    let mismatches = knx_prod::check_template(&ap)?;
    if mismatches.is_empty() {
        println!("\nTemplate check: all defaults match the shipped segment template.");
    } else {
        println!("\nTemplate check: {} default(s) differ from the shipped template:", mismatches.len());
        for m in &mismatches {
            println!("  - {m}");
        }
    }

    // Hex dump of the segments that carry parameters.
    let mut dumped: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for p in &image.placements {
        dumped.insert(p.segment_id.as_str());
    }
    for seg in &image.segments {
        if !dumped.contains(seg.id.as_str()) {
            continue;
        }
        println!("\n--- {} ({}) ---", seg.id, seg.memory_type);
        for (i, chunk) in seg.data.chunks(16).enumerate() {
            let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
            println!("  {:04x}  {}", i * 16, hex.join(" "));
        }
    }
    Ok(())
}

fn all_com_objects(s: &knx_prod::Static) -> Vec<knx_prod::ComObject> {
    let mut out = Vec::new();
    if let Some(t) = &s.com_object_table {
        out.extend(t.items.iter().cloned());
    }
    if let Some(c) = &s.com_objects {
        out.extend(c.items.iter().cloned());
    }
    out
}

/// Dump the group address / association / group object tables (M5).
///
/// Uses the knxprod group objects only (no project links), so the address and
/// association tables are empty; the group object table shows the device's
/// *visible* group objects as they would be encoded, with flags overlaid onto
/// the shipped knxprod GoT template (data pointers preserved).
fn cmd_tables(file: &PathBuf, h2p: &str) -> Result<()> {
    use std::collections::HashMap;

    let knxprod = knx_prod::KnxProd::open(file)
        .with_context(|| format!("reading {}", file.display()))?;
    let ap = knxprod
        .application_program(h2p)
        .with_context(|| format!("resolving {h2p}"))?;

    let values = HashMap::new();
    let objects = knx_prod::tables::active_group_objects(&ap, &values);
    println!("{}: {} visible group object(s)", ap.name, objects.len());

    let got = if let Some(t) = ap
        .static_
        .as_ref()
        .and_then(|s| s.com_object_table.as_ref())
    {
        if let Some(seg) = ap
            .static_
            .as_ref()
            .and_then(|s| s.code.as_ref())
            .and_then(|c| c.by_id(&t.code_segment))
        {
            let offset = t.offset.parse::<usize>().unwrap_or(0);
            let template = seg.decoded_data();
            // No project links here: every visible object is unlinked, so
            // ETS would clear its communication bit.
            let flags: HashMap<u16, u8> = objects
                .iter()
                .map(|(asap, obj)| {
                    let slot_flags = template
                        .get(offset + (*asap as usize + 1) * 4 + 1)
                        .copied()
                        .unwrap_or(0);
                    (
                        *asap,
                        knx_prod::tables::visible_group_object_flags(
                            obj.flags,
                            slot_flags,
                            false,
                        ),
                    )
                })
                .collect();
            knx_prod::tables::overlay_group_object_table_at(&template, offset, &flags)
        } else {
            let descriptors: Vec<knx_prod::tables::GroupObject> =
                objects.iter().map(|(_, o)| o.clone()).collect();
            knx_prod::tables::build_group_object_table(&descriptors)
        }
    } else {
        let descriptors: Vec<knx_prod::tables::GroupObject> =
            objects.iter().map(|(_, o)| o.clone()).collect();
        knx_prod::tables::build_group_object_table(&descriptors)
    };

    println!("\nGroup Object Table ({} bytes):", got.len());
    for (i, chunk) in got.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        println!("  {:04x}  {}", i * 16, hex.join(" "));
    }

    println!("\nASAP numbering (visible module instances):");
    for (asap, obj) in &objects {
        println!(
            "  #{:<4} flags={:02x} vft={}",
            asap,
            knx_prod::tables::go_descriptor_high(obj.flags),
            obj.value_field_type
        );
    }
    Ok(())
}

/// Build the full device download image and print the plan (M5 dry run).
///
/// The actual bus transfer (`A_Memory_Write` + load-control sequence over the
/// USB cEMI tunnel) is M4 transport work; until then this exercises and
/// displays the complete memory image the download path will send.
fn cmd_download(dir: &Path, ia_str: &str, knxprod_override: Option<&Path>) -> Result<()> {
    use std::collections::HashMap;

    let ia: IndividualAddress = ia_str.parse().with_context(|| format!("invalid IA {ia_str}"))?;
    let project = knx_store::load(dir)?;

    let device_ref = project
        .topology
        .iter()
        .flat_map(|a| a.lines.iter())
        .flat_map(|l| l.devices.iter())
        .find(|d| d.ia == ia)
        .ok_or_else(|| anyhow::anyhow!("device {ia} not found in topology"))?;
    let h2p = device_ref.hardware.clone();
    let knxprod_path = product_store::resolve(
        &h2p,
        Some(dir),
        device_ref.knxprod.as_deref().map(Path::new),
        knxprod_override,
    )?;

    let kp = knx_prod::KnxProd::open(&knxprod_path)
        .with_context(|| format!("reading {}", knxprod_path.display()))?;
    let ap = kp
        .application_program(&h2p)
        .with_context(|| format!("resolving {h2p}"))?;

    let dev = project
        .devices
        .iter()
        .find(|d| d.ia == ia)
        .ok_or_else(|| anyhow::anyhow!("device instance {ia} not found"))?;

    let mut values: HashMap<String, String> = HashMap::new();
    for p in &dev.parameters {
        values.insert(p.id.clone(), yaml_to_string(&p.value));
    }
    let links: Vec<(u16, u16)> = dev
        .links
        .iter()
        .map(|l| (l.object_number, l.ga.to_u16()))
        .collect();

    let image = knx_prod::build_device_image(&ap, &values, &links)?;

    println!("Download image for {ia} ({})", device_ref.name);
    println!("{} segment(s):", image.segments.len());
    for seg in &image.segments {
        let addr = if seg.address.is_empty() {
            "-".to_string()
        } else {
            format!("0x{:04x}", seg.address.parse::<u32>().unwrap_or(0))
        };
        let mem = if seg.memory_type.is_empty() { "flash" } else { &seg.memory_type };
        println!(
            "  {:<44} {}  {:>5} bytes  {}",
            seg.id, addr, seg.size, mem
        );
    }

    println!("\nActive group objects: {}", image.active_group_objects.len());
    for (asap, obj) in &image.active_group_objects {
        println!(
            "  #{:<4} flags={:02x} vft={}",
            asap,
            knx_prod::tables::go_descriptor_high(obj.flags),
            obj.value_field_type
        );
    }

    let mut dumped: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for t in [
        image.address_table.as_ref(),
        image.association_table.as_ref(),
        image.group_object_table.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        dumped.insert(t.segment_id.as_str());
    }
    if !dumped.is_empty() {
        println!("\nHex dump (table segments):");
        for seg in &image.segments {
            if !dumped.contains(seg.id.as_str()) {
                continue;
            }
            println!("\n--- {} ({}) ---", seg.id, seg.memory_type);
            for (i, chunk) in seg.data.chunks(16).enumerate() {
                let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
                println!("  {:04x}  {}", i * 16, hex.join(" "));
            }
        }
    }

    let plan = knx_prod::download_plan(&ap, &image)?;
    println!(
        "\nDownload plan ({} step(s), dry run — cEMI transport lands in M4):",
        plan.len()
    );
    for step in &plan {
        match step {
            knx_prod::DownloadStep::Connect => println!("  LdCtrlConnect"),
            knx_prod::DownloadStep::Disconnect => println!("  LdCtrlDisconnect"),
            knx_prod::DownloadStep::Restart => println!("  LdCtrlRestart"),
            knx_prod::DownloadStep::CompareProp {
                obj_idx,
                prop_id,
                inline_data,
            } => {
                let hex: Vec<String> =
                    inline_data.iter().map(|b| format!("{b:02x}")).collect();
                println!(
                    "  LdCtrlCompareProp ObjIdx={obj_idx} PropId={prop_id} InlineData={}",
                    hex.join("")
                );
            }
            knx_prod::DownloadStep::Unload { lsm_idx } => {
                println!("  LdCtrlUnload LsmIdx={lsm_idx}")
            }
            knx_prod::DownloadStep::Load { lsm_idx } => {
                println!("  LdCtrlLoad LsmIdx={lsm_idx}")
            }
            knx_prod::DownloadStep::AbsSegment {
                lsm_idx,
                seg_type,
                address,
                size,
                access,
                mem_type,
                seg_flags,
                data: _,
            } => println!(
                "  LdCtrlAbsSegment LsmIdx={lsm_idx} SegType={seg_type} Address=0x{address:04x} Size={size} Access={access} MemType={mem_type} SegFlags={seg_flags}"
            ),
            knx_prod::DownloadStep::TaskSegment { lsm_idx, address } => {
                println!("  LdCtrlTaskSegment LsmIdx={lsm_idx} Address=0x{address:04x}")
            }
            knx_prod::DownloadStep::LoadCompleted { lsm_idx } => {
                println!("  LdCtrlLoadCompleted LsmIdx={lsm_idx}")
            }
        }
    }

    // Read the image back through the inverse decoders and diff against the
    // project values. This is the dry-run half of the ETS byte-identity check
    // (the other half is reading a real device over the transport).
    let decoded = knx_prod::read_back_device(&ap, &image)?;
    println!("\nRead-back self-check:");
    let mut diffs = 0usize;
    for (id, value) in &values {
        if let Some(decoded_value) = decoded.parameters.get(id) {
            if decoded_value != value {
                println!("  param {id}: project {value} != decoded {decoded_value}");
                diffs += 1;
            }
        }
    }
    if decoded.links != links {
        println!("  links differ: project {links:?}");
        println!("              decoded {:?}", decoded.links);
        diffs += 1;
    }
    if diffs == 0 {
        println!("  OK: project parameters and links round-trip through the image");
    } else {
        println!("  {diffs} difference(s)");
    }

    println!("\n(not sent — USB cEMI transport is not implemented yet)");
    Ok(())
}

fn cmd_add(dir: &Path, knxprod_path: &Path, h2p: &str, ia_str: &str, name: Option<&str>) -> Result<()> {
    let ia: IndividualAddress = ia_str
        .parse()
        .with_context(|| format!("invalid individual address {ia_str}"))?;

    let kp = knx_prod::KnxProd::open(knxprod_path)
        .with_context(|| format!("reading {}", knxprod_path.display()))?;
    let product = kp
        .products
        .iter()
        .find(|p| p.hardware2program_ref_id.as_deref() == Some(h2p))
        .ok_or_else(|| anyhow::anyhow!("{h2p} not found in {}", knxprod_path.display()))?;

    let knxprod_abs = std::fs::canonicalize(knxprod_path).unwrap_or_else(|_| knxprod_path.to_path_buf());

    let mut project = knx_store::load(dir)?;

    let area = match project.topology.iter_mut().find(|a| a.number == ia.area) {
        Some(a) => a,
        None => {
            project.topology.push(Area {
                number: ia.area,
                name: format!("Area {}", ia.area),
                lines: vec![],
            });
            project.topology.last_mut().unwrap()
        }
    };
    let line = match area.lines.iter_mut().find(|l| l.number == ia.line) {
        Some(l) => l,
        None => {
            area.lines.push(Line {
                number: ia.line,
                name: format!("Line {}", ia.line),
                devices: vec![],
            });
            area.lines.last_mut().unwrap()
        }
    };
    if line.devices.iter().any(|d| d.ia == ia) {
        bail!("device {ia} already exists");
    }

    let dev_name = name.map(str::to_string).unwrap_or_else(|| product.name.clone());
    line.devices.push(DeviceRef {
        ia,
        name: dev_name,
        hardware: h2p.to_string(),
        application: None,
        knxprod: Some(knxprod_abs.to_string_lossy().into_owned()),
    });
    if !project.devices.iter().any(|d| d.ia == ia) {
        project.devices.push(DeviceInstance {
            ia,
            parameters: vec![],
            links: vec![],
        });
    }

    project.sort();
    knx_store::save(dir, &project)?;
    println!("Added {ia} ({})", product.name);
    Ok(())
}

fn cmd_rm(dir: &Path, ia_str: &str) -> Result<()> {
    let ia: IndividualAddress = ia_str.parse().with_context(|| format!("invalid IA {ia_str}"))?;
    let mut project = knx_store::load(dir)?;
    let mut removed = false;
    for area in &mut project.topology {
        for line in &mut area.lines {
            let before = line.devices.len();
            line.devices.retain(|d| d.ia != ia);
            removed |= line.devices.len() != before;
        }
    }
    project.devices.retain(|d| d.ia != ia);
    if !removed {
        bail!("device {ia} not found");
    }
    project.sort();
    knx_store::save(dir, &project)?;
    println!("Removed {ia}");
    Ok(())
}

fn cmd_group_add(dir: &Path, ga_str: &str, name: Option<&str>, dpt: Option<&str>) -> Result<()> {
    let ga: GroupAddress = ga_str.parse().with_context(|| format!("invalid GA {ga_str}"))?;
    let mut project = knx_store::load(dir)?;
    if project.groups.iter().any(|g| g.ga == ga) {
        bail!("group address {ga} already exists");
    }
    let name = name.map(str::to_string).unwrap_or_else(|| ga.to_string());
    project.groups.push(GroupAddressEntry {
        ga,
        name,
        dpt: dpt.map(str::to_string),
    });
    project.sort();
    knx_store::save(dir, &project)?;
    println!("Added group address {ga}");
    Ok(())
}

fn cmd_group_rm(dir: &Path, ga_str: &str) -> Result<()> {
    let ga: GroupAddress = ga_str.parse().with_context(|| format!("invalid GA {ga_str}"))?;
    let mut project = knx_store::load(dir)?;
    let before = project.groups.len();
    project.groups.retain(|g| g.ga != ga);
    if project.groups.len() == before {
        bail!("group address {ga} not found");
    }
    project.sort();
    knx_store::save(dir, &project)?;
    println!("Removed group address {ga}");
    Ok(())
}

fn cmd_link(dir: &Path, ia_str: &str, object: u16, ga_str: &str) -> Result<()> {
    let ia: IndividualAddress = ia_str.parse().with_context(|| format!("invalid IA {ia_str}"))?;
    let ga: GroupAddress = ga_str.parse().with_context(|| format!("invalid GA {ga_str}"))?;
    let mut project = knx_store::load(dir)?;
    let dev = project
        .devices
        .iter_mut()
        .find(|d| d.ia == ia)
        .ok_or_else(|| anyhow::anyhow!("device {ia} not found"))?;
    let link = GroupObjectLink { object_number: object, ga };
    if dev.links.contains(&link) {
        bail!("link {ia} #{object} -> {ga} already exists");
    }
    dev.links.push(link);
    project.sort();
    knx_store::save(dir, &project)?;
    println!("Linked {ia} object #{object} -> {ga}");
    Ok(())
}

fn cmd_unlink(dir: &Path, ia_str: &str, object: u16, ga_str: &str) -> Result<()> {
    let ia: IndividualAddress = ia_str.parse().with_context(|| format!("invalid IA {ia_str}"))?;
    let ga: GroupAddress = ga_str.parse().with_context(|| format!("invalid GA {ga_str}"))?;
    let mut project = knx_store::load(dir)?;
    let dev = project
        .devices
        .iter_mut()
        .find(|d| d.ia == ia)
        .ok_or_else(|| anyhow::anyhow!("device {ia} not found"))?;
    let link = GroupObjectLink { object_number: object, ga };
    let before = dev.links.len();
    dev.links.retain(|l| *l != link);
    if dev.links.len() == before {
        bail!("link {ia} #{object} -> {ga} not found");
    }
    project.sort();
    knx_store::save(dir, &project)?;
    println!("Unlinked {ia} object #{object} -> {ga}");
    Ok(())
}

fn cmd_device_set(dir: &Path, ia_str: &str, param: &str, value: &str) -> Result<()> {
    let ia: IndividualAddress = ia_str.parse().with_context(|| format!("invalid IA {ia_str}"))?;
    let mut project = knx_store::load(dir)?;
    let dev = project
        .devices
        .iter_mut()
        .find(|d| d.ia == ia)
        .ok_or_else(|| anyhow::anyhow!("device {ia} not found"))?;
    if let Some(p) = dev.parameters.iter_mut().find(|p| p.id == param) {
        p.value = serde_yaml::Value::String(value.to_string());
    } else {
        dev.parameters.push(ParameterValue {
            id: param.to_string(),
            value: serde_yaml::Value::String(value.to_string()),
        });
    }
    project.sort();
    knx_store::save(dir, &project)?;
    println!("Set {ia} {param} = {value}");
    Ok(())
}

fn cmd_edit(dir: &Path, ia_str: &str, knxprod_override: Option<&Path>) -> Result<()> {
    use std::collections::HashMap;

    let ia: IndividualAddress = ia_str.parse().with_context(|| format!("invalid IA {ia_str}"))?;
    let mut project = knx_store::load(dir)?;

    // Resolve the hardware ref + recorded knxprod from the topology.
    let device_ref = project
        .topology
        .iter()
        .flat_map(|a| a.lines.iter())
        .flat_map(|l| l.devices.iter())
        .find(|d| d.ia == ia)
        .ok_or_else(|| anyhow::anyhow!("device {ia} not found in topology"))?;
    let h2p = device_ref.hardware.clone();

    let knxprod_path = product_store::resolve(
        &h2p,
        Some(dir),
        device_ref.knxprod.as_deref().map(Path::new),
        knxprod_override,
    )?;

    let kp = knx_prod::KnxProd::open(&knxprod_path)
        .with_context(|| format!("reading {}", knxprod_path.display()))?;
    let ap = kp
        .application_program(&h2p)
        .with_context(|| format!("resolving {h2p}"))?;

    let dev_params = project
        .devices
        .iter()
        .find(|d| d.ia == ia)
        .map(|d| d.parameters.clone())
        .unwrap_or_default();
    let mut values: HashMap<String, String> = HashMap::new();
    for p in &dev_params {
        values.insert(p.id.clone(), yaml_to_string(&p.value));
    }

    // Snapshot the knxprod defaults so saving can be minimal/deterministic:
    // any value equal to its default is redundant and is not persisted.
    let defaults = knx_prod::parameter_defaults(&ap);

    let saved = tui::edit_device(&ap, &values)?;
    if let Some(new_values) = saved {
        let dev = project
            .devices
            .iter_mut()
            .find(|d| d.ia == ia)
            .ok_or_else(|| anyhow::anyhow!("device {ia} not found"))?;
        let mut params: Vec<ParameterValue> = new_values
            .into_iter()
            .filter(|(id, value)| defaults.get(id) != Some(value))
            .map(|(id, value)| ParameterValue {
                id,
                value: serde_yaml::Value::String(value),
            })
            .collect();
        params.sort_by(|a, b| a.id.cmp(&b.id));
        dev.parameters = params;
        project.sort();
        knx_store::save(dir, &project)?;
        println!("Saved {ia}");
    } else {
        println!("No changes saved");
    }
    Ok(())
}

fn yaml_to_string(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Bool(b) => {
            if *b { "1".to_string() } else { "0".to_string() }
        }
        _ => String::new(),
    }
}
