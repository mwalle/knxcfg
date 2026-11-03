//! `knxcfg` — a git-friendly KNX configuration tool (ETS replacement).

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use knx_core::{
    Area, DeviceInstance, DeviceRef, GroupAddress, GroupAddressEntry, GroupObjectLink,
    IndividualAddress, Line, ParameterValue,
};
use std::path::{Path, PathBuf};

mod bus;
mod download;
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
    GroupRm {
        ga: String,
        /// Also remove the links that use it
        #[arg(long)]
        force: bool,
    },
    /// Link a device group object to a group address
    Link {
        ia: String,
        /// Group object number
        object: u16,
        /// Group address (3-level 1/2/3, 2-level 1/515 or raw 2563)
        ga: String,
        /// Make this the address the object transmits on
        #[arg(long)]
        send: bool,
        #[arg(long)]
        knxprod: Option<PathBuf>,
    },
    /// Remove a link
    Unlink {
        ia: String,
        object: u16,
        ga: String,
    },
    /// Set a device parameter value (validated against the knxprod)
    DeviceSet {
        ia: String,
        /// Parameter id (`P-3`) or `module-instance/param` (`MD-2_M-2/MD-2_P-26`)
        param: String,
        value: String,
        #[arg(long)]
        knxprod: Option<PathBuf>,
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
        /// Also print the encoded cEMI management frames (M4/M5)
        #[arg(long)]
        frames: bool,
        /// Parameters only (partial download; MergedProcedure applications)
        #[arg(long)]
        partial: bool,
    },
    /// List the hidraw devices; KNX USB interfaces are marked
    Interfaces,
    /// Read a device's descriptor (mask version) over the bus
    DeviceInfo {
        /// Individual address, e.g. 1.1.1
        ia: String,
        /// USB interface (defaults to the only KNX one)
        #[arg(short, long)]
        interface: Option<PathBuf>,
    },
    /// Read device memory over the bus
    MemRead {
        /// Individual address, e.g. 1.1.1
        ia: String,
        /// Start address: 0x4000, 4000h or decimal
        address: String,
        /// Number of octets
        count: usize,
        /// USB interface (defaults to the only KNX one)
        #[arg(short, long)]
        interface: Option<PathBuf>,
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
        Cmd::GroupRm { ga, force } => {
            let dir = resolve_project(project.as_deref())?;
            cmd_group_rm(&dir, &ga, force)
        }
        Cmd::Link { ia, object, ga, send, knxprod } => {
            let dir = resolve_project(project.as_deref())?;
            cmd_link(&dir, &ia, object, &ga, send, knxprod.as_deref())
        }
        Cmd::Unlink { ia, object, ga } => {
            let dir = resolve_project(project.as_deref())?;
            cmd_unlink(&dir, &ia, object, &ga)
        }
        Cmd::DeviceSet { ia, param, value, knxprod } => {
            let dir = resolve_project(project.as_deref())?;
            cmd_device_set(&dir, &ia, &param, &value, knxprod.as_deref())
        }
        Cmd::Edit { ia, knxprod } => {
            let dir = resolve_project(project.as_deref())?;
            cmd_edit(&dir, &ia, knxprod.as_deref())
        }
        Cmd::Export => bail!("`export` is not implemented yet (M3)"),
        Cmd::Import { .. } => bail!("`import` is not implemented yet (M2)"),
        Cmd::Download { ia, knxprod, frames, partial } => {
            let dir = resolve_project(project.as_deref())?;
            cmd_download(&dir, &ia, knxprod.as_deref(), frames, partial)
        }
        Cmd::Interfaces => bus::cmd_interfaces(),
        Cmd::DeviceInfo { ia, interface } => bus::cmd_device_info(interface.as_deref(), &ia),
        Cmd::MemRead { ia, address, count, interface } => {
            bus::cmd_mem_read(interface.as_deref(), &ia, &address, count)
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
        for l in &dev.links {
            println!(
                "    #{:<4} -> {}{}",
                l.object_number,
                l.ga,
                if l.send { "  (sending)" } else { "" }
            );
        }
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

    let ev = knx_prod::Evaluation::new(&ap, &HashMap::new());
    print_warnings(&ev.warnings);
    let image = knx_prod::build_parameter_image_eval(&ev)?;

    println!("Memory image: {} ({} segment(s))", ap.name, image.segments.len());
    for seg in &image.segments {
        println!(
            "  {:<44} {}  {:>5} bytes  {}",
            seg.id,
            segment_address(seg),
            seg.size,
            segment_memory(seg)
        );
    }

    println!("\nParameter placements ({}):", image.placements.len());
    for p in &image.placements {
        println!(
            "  {:<40} {:<40} byte {:<4} bit {:<2} {} bit = {}",
            knx_prod::eval::short_key(&ap, &p.param_id),
            p.segment_id,
            p.offset,
            p.bit_offset,
            p.bit_len,
            p.value
        );
    }

    // The template is the manufacturer's factory image; ETS overwrites every
    // parameter position, so differences are informational.
    let mismatches = knx_prod::check_template(&ap)?;
    if mismatches.is_empty() {
        println!("\nThe default parameters encode to exactly the shipped segment template.");
    } else {
        println!(
            "\n{} default placement(s) differ from the shipped segment template \
             (informational: the template is the factory image, ETS overwrites it):",
            mismatches.len()
        );
        for m in &mismatches {
            println!("  - {m}");
        }
    }

    // Hex dump of the segments that carry parameters.
    let dumped: std::collections::HashSet<&str> =
        image.placements.iter().map(|p| p.segment_id.as_str()).collect();
    for seg in &image.segments {
        if dumped.contains(seg.id.as_str()) {
            println!("\n--- {} ({}) ---", seg.id, segment_memory(seg));
            hex_dump(&seg.data);
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

/// Dump the group address / association / group object tables (M5) for the
/// knxprod defaults and no links: the address and association tables are
/// empty, and every active object has its communication flag cleared (an
/// unlinked object cannot communicate).
fn cmd_tables(file: &PathBuf, h2p: &str) -> Result<()> {
    use std::collections::HashMap;

    let knxprod = knx_prod::KnxProd::open(file)
        .with_context(|| format!("reading {}", file.display()))?;
    let ap = knxprod
        .application_program(h2p)
        .with_context(|| format!("resolving {h2p}"))?;

    let image = knx_prod::build_device_image(&ap, &HashMap::new(), &[])?;
    print_warnings(&image.warnings);
    println!(
        "{}: {} active group object(s), {:?} tables",
        ap.name,
        image.active_group_objects.len(),
        image.format
    );
    for (name, t) in [
        ("Group Address Table", &image.address_table),
        ("Association Table", &image.association_table),
        ("Group Object Table", &image.group_object_table),
    ] {
        match &t.placement {
            Some(p) => println!(
                "\n{name} ({} bytes at {}+{}):",
                t.bytes.len(),
                p.segment_id,
                p.offset
            ),
            None => println!("\n{name} ({} bytes, loaded through its table object):", t.bytes.len()),
        }
        hex_dump(&t.bytes);
    }

    println!("\nActive group objects:");
    print_group_objects(&image.active_group_objects);
    Ok(())
}

/// Build the full device download image and print the plan (M5 dry run).
///
/// The actual bus transfer (`A_Memory_Write` + load-control sequence over the
/// USB cEMI tunnel) is M4 transport work; until then this builds and displays
/// everything the download will send.
fn cmd_download(
    dir: &Path,
    ia_str: &str,
    knxprod_override: Option<&Path>,
    show_frames: bool,
    partial: bool,
) -> Result<()> {
    let ia: IndividualAddress = ia_str.parse().with_context(|| format!("invalid IA {ia_str}"))?;
    let project = knx_store::load(dir)?;
    let device = open_device(dir, &project, ia, knxprod_override)?;
    let ap = &device.ap;

    let values = device.values();
    let links = device.links();
    let ev = knx_prod::Evaluation::new(ap, &values);
    if !ev.unknown_keys.is_empty() {
        bail!(
            "{ia} has values for parameters that {} does not define:\n  {}",
            ap.id,
            ev.unknown_keys
                .iter()
                .map(|k| knx_prod::eval::short_key(ap, k))
                .collect::<Vec<_>>()
                .join("\n  ")
        );
    }
    let image = knx_prod::device_image::build_device_image_eval(&ev, &links)?;

    println!("Download image for {ia} ({})", device.device_ref.name);
    print_warnings(&image.warnings);
    println!("{} segment(s):", image.segments.len());
    for seg in &image.segments {
        println!(
            "  {:<44} {}  {:>5} bytes  {}",
            seg.id,
            segment_address(seg),
            seg.size,
            segment_memory(seg)
        );
    }

    println!("\nActive group objects: {}", image.active_group_objects.len());
    print_group_objects(&image.active_group_objects);

    println!("\nTables ({:?}):", image.format);
    for (name, t) in [
        ("Group Address Table", &image.address_table),
        ("Association Table", &image.association_table),
        ("Group Object Table", &image.group_object_table),
    ] {
        let at = t
            .placement
            .as_ref()
            .map(|p| format!(" at {}+{}", p.segment_id, p.offset))
            .unwrap_or_default();
        println!("\n{name} ({} bytes{at}):", t.bytes.len());
        hex_dump(&t.bytes);
    }

    let kind = if partial {
        knx_prod::DownloadKind::Partial
    } else {
        knx_prod::DownloadKind::Full
    };
    let master = if ap.load_procedure_style == "MergedProcedure" {
        Some(device.knxprod.master_data()?)
    } else {
        None
    };
    let plan = knx_prod::download_plan(&ev, &image, master.as_ref(), kind)?;
    println!("\nDownload plan ({} step(s), {kind:?}):", plan.len());
    for step in &plan {
        println!("  {}", describe_step(step));
    }

    // Decode the image again and compare with the evaluated configuration.
    // This checks that encoder and decoder agree on placements and widths;
    // it cannot show that the bytes are what ETS writes (that needs a
    // comparison with an ETS download).
    let decoded = knx_prod::read_back_device(&ev, &image)?;
    let mut diffs = 0usize;
    for (id, value) in &decoded.parameters {
        if image.shadowed.contains(id) {
            continue; // overwritten by an active alternative
        }
        let expected = ev.value(id).unwrap_or_default();
        if !same_value(expected, value) {
            println!("  param {id}: evaluated {expected} != decoded {value}");
            diffs += 1;
        }
    }
    let mut expected_links: Vec<(u16, u16)> = image.links.iter().map(|l| (l.asap, l.ga)).collect();
    let mut decoded_links: Vec<(u16, u16)> = decoded.links.iter().map(|l| (l.asap, l.ga)).collect();
    expected_links.sort_unstable();
    expected_links.dedup();
    decoded_links.sort_unstable();
    if expected_links != decoded_links {
        println!("  links differ: {expected_links:?} != {decoded_links:?}");
        diffs += 1;
    }
    println!(
        "\nEncoder/decoder consistency: {}",
        if diffs == 0 { "OK".to_string() } else { format!("{diffs} difference(s)") }
    );

    if show_frames {
        let frames = download::plan_to_frames(&plan, ia.to_u16())?;
        println!("\nEncoded cEMI frames ({}):", frames.len());
        for (i, f) in frames.iter().enumerate() {
            let hex: Vec<String> = f.encode().iter().map(|b| format!("{b:02x}")).collect();
            println!("  [{i:03}] {}", hex.join(" "));
        }
    }

    println!("\n(not sent — USB cEMI transport is not implemented yet)");
    Ok(())
}

fn cmd_add(
    dir: &Path,
    knxprod_path: &Path,
    h2p: &str,
    ia_str: &str,
    name: Option<&str>,
) -> Result<()> {
    let ia: IndividualAddress = ia_str
        .parse()
        .with_context(|| format!("invalid individual address {ia_str}"))?;
    if let Some(reason) = ia.reserved_reason() {
        bail!("{ia} cannot be used for a device: {reason}");
    }

    let kp = knx_prod::KnxProd::open(knxprod_path)
        .with_context(|| format!("reading {}", knxprod_path.display()))?;
    let product = kp
        .products
        .iter()
        .find(|p| p.hardware2program_ref_id.as_deref() == Some(h2p))
        .ok_or_else(|| anyhow::anyhow!("{h2p} not found in {}", knxprod_path.display()))?;

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
        knxprod: Some(product_store::record_path(dir, knxprod_path)),
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
    if ga.is_broadcast() {
        bail!("0/0/0 is the broadcast address and cannot be used as a group address");
    }
    let dpt = dpt.map(normalize_dpt).transpose()?;
    let mut project = knx_store::load(dir)?;
    if project.groups.iter().any(|g| g.ga == ga) {
        bail!("group address {ga} already exists");
    }
    let name = name.map(str::to_string).unwrap_or_else(|| ga.to_string());
    project.groups.push(GroupAddressEntry { ga, name, dpt });
    project.sort();
    knx_store::save(dir, &project)?;
    println!("Added group address {ga}");
    Ok(())
}

fn cmd_group_rm(dir: &Path, ga_str: &str, force: bool) -> Result<()> {
    let ga: GroupAddress = ga_str.parse().with_context(|| format!("invalid GA {ga_str}"))?;
    let mut project = knx_store::load(dir)?;
    let before = project.groups.len();
    project.groups.retain(|g| g.ga != ga);
    if project.groups.len() == before {
        bail!("group address {ga} not found");
    }
    let users: Vec<String> = project
        .devices
        .iter()
        .flat_map(|d| {
            d.links
                .iter()
                .filter(|l| l.ga == ga)
                .map(move |l| format!("{} #{}", d.ia, l.object_number))
        })
        .collect();
    if !users.is_empty() {
        if !force {
            bail!(
                "{ga} is linked to {}; unlink first or pass --force to remove the links too",
                users.join(", ")
            );
        }
        for d in &mut project.devices {
            d.links.retain(|l| l.ga != ga);
        }
        println!("Removed {} link(s): {}", users.len(), users.join(", "));
    }
    project.sort();
    knx_store::save(dir, &project)?;
    println!("Removed group address {ga}");
    Ok(())
}

fn cmd_link(
    dir: &Path,
    ia_str: &str,
    object: u16,
    ga_str: &str,
    send: bool,
    knxprod_override: Option<&Path>,
) -> Result<()> {
    let ia: IndividualAddress = ia_str.parse().with_context(|| format!("invalid IA {ia_str}"))?;
    let ga: GroupAddress = ga_str.parse().with_context(|| format!("invalid GA {ga_str}"))?;
    if ga.is_broadcast() {
        bail!("0/0/0 is the broadcast address and cannot be linked");
    }
    let mut project = knx_store::load(dir)?;
    let entry = project
        .groups
        .iter()
        .find(|g| g.ga == ga)
        .ok_or_else(|| anyhow::anyhow!("group address {ga} does not exist; add it with `knxcfg group-add {ga}`"))?
        .clone();

    let device = open_device(dir, &project, ia, knxprod_override)?;
    let objects = knx_prod::tables::group_objects(&device.ap);
    if !objects.iter().any(|(n, _)| *n == object) {
        bail!(
            "{} has no group object #{object} (objects: {})",
            device.ap.name,
            number_ranges(objects.iter().map(|(n, _)| *n))
        );
    }
    let active = knx_prod::tables::active_group_objects(&device.ap, &device.values());
    match active.iter().find(|(n, _)| *n == object) {
        None => eprintln!(
            "warning: group object #{object} is not active with the current parameters; \
             the link is kept but not downloaded"
        ),
        Some((_, obj)) => {
            if let Some(ga_dpt) = &entry.dpt {
                if let Ok(master) = device.knxprod.master_data() {
                    if master.datapoint(ga_dpt).is_none() {
                        eprintln!("warning: {ga} has {ga_dpt}, which knx_master.xml does not define");
                    }
                }
                let wanted: Vec<Option<u32>> =
                    obj.datapoint_type.split_whitespace().map(dpt_main).collect();
                if !wanted.is_empty() && !wanted.contains(&dpt_main(ga_dpt)) {
                    eprintln!(
                        "warning: {ga} has {ga_dpt}, object #{object} expects {}",
                        obj.datapoint_type
                    );
                }
            }
        }
    }

    let dev = project
        .devices
        .iter_mut()
        .find(|d| d.ia == ia)
        .ok_or_else(|| anyhow::anyhow!("device {ia} not found"))?;
    if send {
        for l in dev.links.iter_mut().filter(|l| l.object_number == object) {
            l.send = false;
        }
    }
    match dev.links.iter_mut().find(|l| l.object_number == object && l.ga == ga) {
        Some(l) if send => l.send = true,
        Some(_) => bail!("link {ia} #{object} -> {ga} already exists"),
        None => dev.links.push(GroupObjectLink { object_number: object, ga, send }),
    }
    project.sort();
    knx_store::save(dir, &project)?;
    println!(
        "Linked {ia} object #{object} -> {ga}{}",
        if send { " (sending)" } else { "" }
    );
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
    let before = dev.links.len();
    dev.links.retain(|l| !(l.object_number == object && l.ga == ga));
    if dev.links.len() == before {
        bail!("link {ia} #{object} -> {ga} not found");
    }
    project.sort();
    knx_store::save(dir, &project)?;
    println!("Unlinked {ia} object #{object} -> {ga}");
    Ok(())
}

fn cmd_device_set(
    dir: &Path,
    ia_str: &str,
    param: &str,
    value: &str,
    knxprod_override: Option<&Path>,
) -> Result<()> {
    let ia: IndividualAddress = ia_str.parse().with_context(|| format!("invalid IA {ia_str}"))?;
    let mut project = knx_store::load(dir)?;
    let device = open_device(dir, &project, ia, knxprod_override)?;
    let ap = &device.ap;

    let key = knx_prod::eval::full_key(ap, param);
    knx_form::validate(ap, &key, value).with_context(|| format!("{param} = {value:?}"))?;
    if !knx_form::is_editable(ap, &key) {
        bail!("{param} is not user-editable (Access=None/ReadOnly in the knxprod)");
    }

    let mut values = device.values();
    values.insert(key.clone(), value.to_string());
    let ev = knx_prod::Evaluation::new(ap, &values);
    if ev.is_forced(&key) {
        bail!("{param} is set by the application (Assign/calculation); it cannot be changed");
    }
    if !ev.is_param_active(&key) {
        eprintln!("warning: {param} is not visible with the current parameters");
    }
    let is_default = ev.default_of(&key) == Some(value);

    let short = knx_prod::eval::short_key(ap, &key);
    let dev = project
        .devices
        .iter_mut()
        .find(|d| d.ia == ia)
        .ok_or_else(|| anyhow::anyhow!("device {ia} not found"))?;
    dev.parameters
        .retain(|p| knx_prod::eval::full_key(ap, &p.id) != key);
    if !is_default {
        dev.parameters.push(ParameterValue {
            id: short,
            value: serde_yaml::Value::String(value.to_string()),
        });
    }
    project.sort();
    knx_store::save(dir, &project)?;
    println!(
        "Set {ia} {param} = {value}{}",
        if is_default { " (default, not stored)" } else { "" }
    );
    Ok(())
}

fn cmd_edit(dir: &Path, ia_str: &str, knxprod_override: Option<&Path>) -> Result<()> {
    let ia: IndividualAddress = ia_str.parse().with_context(|| format!("invalid IA {ia_str}"))?;
    let mut project = knx_store::load(dir)?;
    let device = open_device(dir, &project, ia, knxprod_override)?;
    let ap = &device.ap;

    let header = format!("{ia} · {}", device.device_ref.name);
    let saved = tui::edit_device(ap, &device.values(), &header)?;
    let Some(new_values) = saved else {
        println!("No changes saved");
        return Ok(());
    };

    // Keep the project minimal and deterministic: store only values that
    // differ from what the application would use without them.
    let ev = knx_prod::Evaluation::new(ap, &new_values);
    let mut params: Vec<ParameterValue> = new_values
        .into_iter()
        .filter(|(id, value)| {
            !ev.unknown_keys.contains(id)
                && !ev.is_forced(id)
                && ev.default_of(id) != Some(value.as_str())
        })
        .map(|(id, value)| ParameterValue {
            id: knx_prod::eval::short_key(ap, &id),
            value: serde_yaml::Value::String(value),
        })
        .collect();
    params.sort_by(|a, b| a.id.cmp(&b.id));
    let dev = project
        .devices
        .iter_mut()
        .find(|d| d.ia == ia)
        .ok_or_else(|| anyhow::anyhow!("device {ia} not found"))?;
    dev.parameters = params;
    project.sort();
    knx_store::save(dir, &project)?;
    println!("Saved {ia}");
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

/// A project device with its resolved product data.
struct OpenDevice {
    device_ref: DeviceRef,
    instance: DeviceInstance,
    knxprod: knx_prod::KnxProd,
    ap: knx_prod::ApplicationProgram,
}

impl OpenDevice {
    /// The device's stored parameter values, keyed by full parameter key.
    fn values(&self) -> std::collections::HashMap<String, String> {
        self.instance
            .parameters
            .iter()
            .map(|p| (knx_prod::eval::full_key(&self.ap, &p.id), yaml_to_string(&p.value)))
            .collect()
    }

    fn links(&self) -> Vec<knx_prod::tables::Link> {
        self.instance
            .links
            .iter()
            .map(|l| knx_prod::tables::Link {
                asap: l.object_number,
                ga: l.ga.to_u16(),
                send: l.send,
            })
            .collect()
    }
}

fn open_device(
    dir: &Path,
    project: &knx_core::Project,
    ia: IndividualAddress,
    knxprod_override: Option<&Path>,
) -> Result<OpenDevice> {
    let device_ref = project
        .topology
        .iter()
        .flat_map(|a| a.lines.iter())
        .flat_map(|l| l.devices.iter())
        .find(|d| d.ia == ia)
        .ok_or_else(|| anyhow::anyhow!("device {ia} not found in topology"))?
        .clone();
    let instance = project
        .devices
        .iter()
        .find(|d| d.ia == ia)
        .cloned()
        .unwrap_or(DeviceInstance { ia, parameters: vec![], links: vec![] });
    let path = product_store::resolve(
        &device_ref.hardware,
        Some(dir),
        device_ref.knxprod.as_deref().map(Path::new),
        knxprod_override,
    )?;
    let knxprod = knx_prod::KnxProd::open(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let ap = knxprod
        .application_program(&device_ref.hardware)
        .with_context(|| format!("resolving {}", device_ref.hardware))?;
    Ok(OpenDevice { device_ref, instance, knxprod, ap })
}

fn print_warnings(warnings: &[String]) {
    for w in warnings {
        eprintln!("warning: {w}");
    }
}

fn segment_address(seg: &knx_prod::SegmentImage) -> String {
    match (seg.address.parse::<u32>(), seg.load_state_machine) {
        (Ok(a), _) => format!("0x{a:04x}"),
        (_, Some(lsm)) => format!("LSM {lsm}"),
        _ => "-".to_string(),
    }
}

fn segment_memory(seg: &knx_prod::SegmentImage) -> &str {
    if seg.memory_type.is_empty() {
        "-"
    } else {
        &seg.memory_type
    }
}

fn hex_dump(data: &[u8]) {
    for (i, chunk) in data.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        println!("  {:04x}  {}", i * 16, hex.join(" "));
    }
}

fn print_group_objects(objects: &[(u16, knx_prod::tables::GroupObject)]) {
    for (asap, obj) in objects {
        let f = obj.flags;
        let flag = |on: bool, c: char| if on { c } else { '-' };
        println!(
            "  #{:<4} {}{}{}{}{}{} {:<9} {:<14} {} {}",
            asap,
            flag(f.communicate, 'C'),
            flag(f.read, 'R'),
            flag(f.write, 'W'),
            flag(f.transmit, 'T'),
            flag(f.update, 'U'),
            flag(f.read_on_init, 'I'),
            obj.object_size,
            obj.datapoint_type,
            obj.text,
            obj.function_text
        );
    }
}

fn describe_step(step: &knx_prod::DownloadStep) -> String {
    use knx_prod::DownloadStep as S;
    let hex = |d: &[u8]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
    match step {
        S::Connect => "Connect".into(),
        S::Disconnect => "Disconnect".into(),
        S::Restart => "Restart".into(),
        S::CompareProp { obj_idx, prop_id, inline_data } => {
            format!("CompareProp ObjIdx={obj_idx} PropId={prop_id} Data={}", hex(inline_data))
        }
        S::Unload { lsm_idx } => format!("Unload LsmIdx={lsm_idx}"),
        S::Load { lsm_idx } => format!("Load LsmIdx={lsm_idx}"),
        S::LoadCompleted { lsm_idx } => format!("LoadCompleted LsmIdx={lsm_idx}"),
        S::AbsSegment { lsm_idx, seg_type, address, size, access, mem_type, seg_flags, data } => format!(
            "AbsSegment LsmIdx={lsm_idx} SegType={seg_type} Address=0x{address:04x} Size={size} \
             Access={access} MemType={mem_type} SegFlags={seg_flags} ({})",
            if data.is_empty() { "allocate only".to_string() } else { format!("write {} bytes", data.len()) }
        ),
        S::TaskSegment { lsm_idx, address, pei_type, app_id } => format!(
            "TaskSegment LsmIdx={lsm_idx} Address=0x{address:04x} PeiType={pei_type} AppId={}",
            hex(app_id)
        ),
        S::RelSegment { lsm_idx, size, mode, fill } => {
            format!("RelSegment LsmIdx={lsm_idx} Size={size} Mode={mode} Fill={fill}")
        }
        S::WriteRelMem { obj_idx, offset, data, verify } => format!(
            "WriteRelMem ObjIdx={obj_idx} Offset={offset} Size={} Verify={verify}",
            data.len()
        ),
        S::WriteProp { obj_idx, prop_id, data, verify } => {
            format!("WriteProp ObjIdx={obj_idx} PropId={prop_id} Data={} Verify={verify}", hex(data))
        }
        S::ReadProp { obj_idx, prop_id } => format!("ReadProp ObjIdx={obj_idx} PropId={prop_id}"),
        S::Delay { ms } => format!("Delay {ms} ms"),
        S::MapError { original, mapped } => format!("MapError {original} -> {mapped}"),
    }
}

/// Compare an evaluated and a decoded value (numbers numerically, so a
/// float default written as `1.5E+001` matches `15`).
fn same_value(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
        // DPT 9 is lossy (11-bit mantissa).
        (Ok(x), Ok(y)) => (x - y).abs() <= x.abs().max(y.abs()) * 0.005 + 0.01,
        _ => a.eq_ignore_ascii_case(b),
    }
}

/// Normalize a DPT given on the command line: `DPT-9`, `DPST-9-1`, `9`,
/// `9.001` → `DPT-9` / `DPST-9-1`.
fn normalize_dpt(s: &str) -> Result<String> {
    let s = s.trim();
    let bad = || anyhow::anyhow!("invalid DPT {s:?} (expected e.g. DPST-1-1, DPT-9 or 9.001)");
    let (main, sub) = if let Some(r) = s.strip_prefix("DPST-") {
        let (m, x) = r.split_once('-').ok_or_else(bad)?;
        (m, Some(x))
    } else if let Some(r) = s.strip_prefix("DPT-") {
        (r, None)
    } else if let Some((m, x)) = s.split_once('.') {
        (m, Some(x))
    } else {
        (s, None)
    };
    let main: u32 = main.parse().map_err(|_| bad())?;
    match sub {
        None => Ok(format!("DPT-{main}")),
        Some(x) => Ok(format!("DPST-{main}-{}", x.parse::<u32>().map_err(|_| bad())?)),
    }
}

fn dpt_main(s: &str) -> Option<u32> {
    normalize_dpt(s)
        .ok()?
        .split('-')
        .nth(1)
        .and_then(|m| m.parse().ok())
}

/// `0-5, 7, 9-12`
fn number_ranges(numbers: impl Iterator<Item = u16>) -> String {
    let mut v: Vec<u16> = numbers.collect();
    v.sort_unstable();
    v.dedup();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        let start = v[i];
        while i + 1 < v.len() && v[i + 1] == v[i] + 1 {
            i += 1;
        }
        out.push(if v[i] == start { start.to_string() } else { format!("{start}-{}", v[i]) });
        i += 1;
    }
    out.join(", ")
}
