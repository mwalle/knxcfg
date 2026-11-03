//! knxprod (manufacturer product database) reader.
//!
//! A knxprod file is a ZIP archive with the layout
//! (namespace `http://knx.org/xml/project/20`):
//!
//! ```text
//! knx_master.xml                 # MasterData: datapoint types, languages, …
//! M-XXXX/Catalog.xml             # catalog sections + catalog items
//! M-XXXX/Hardware.xml            # hardware definitions
//! M-XXXX/M-XXXX_A-XXXX-XX-XXXX.xml  # one file per application program
//! ```
//!
//! See `docs/knxprod-format.md` for the observed structure.

mod app;
mod catalog;
pub mod device_image;
mod dynamic;
mod hardware;
pub mod memory;
pub mod tables;

use anyhow::{anyhow, Context, Result};
use std::io::Read;
use std::path::{Path, PathBuf};

pub use app::{
    ApplicationProgram, Argument, Arguments, Code, ComObject, ComObjects, ComObjectTable,
    Enumeration, Language, Languages, LdCtrlAbsSegment, LdCtrlCompareProp, LdCtrlLoad,
    LdCtrlLoadCompleted, LdCtrlTaskSegment, LdCtrlUnload, LoadProcedure, LoadProcedures,
    LoadStep, Memory, ModuleDef, ModuleDefs, ParamNode, Parameter, ParameterRef, ParameterRefs,
    ParameterType, ParameterTypes, Parameters, Segment, Static, Table, Translation,
    TranslationElement, TranslationUnit, TypeNumber, TypeRestriction, TypeText, Union,
};
pub use device_image::{
    build_device_image, download_plan, read_back_device, DeviceConfiguration, DeviceImage,
    DownloadStep, TablePlacement,
};
pub use memory::{build_parameter_image, check_template, MemoryImage, Placement, SegmentImage};
pub use dynamic::{
    Arg, Assign, Channel, ChannelIndependentBlock, Choose, ComObjectRefRef, Dynamic,
    ModuleInstance, NumericArg, ParameterBlock, ParameterRefRef, ParameterSeparator, TextArg,
    UiNode, When,
};

/// A single catalog item (a concrete product you can put in a project).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Product {
    pub id: String,
    pub name: String,
    pub number: String,
    pub visible_description: Option<String>,
    pub product_ref_id: Option<String>,
    pub hardware2program_ref_id: Option<String>,
}

/// A parsed knxprod catalog.
#[derive(Debug, Clone)]
pub struct KnxProd {
    pub manufacturer_id: String,
    pub products: Vec<Product>,
    path: PathBuf,
}

impl KnxProd {
    /// Open a knxprod file and parse its catalog.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file =
            std::fs::File::open(&path).with_context(|| format!("opening {}", path.display()))?;
        let mut archive = zip::ZipArchive::new(file).context("reading knxprod ZIP")?;

        let catalog_name = find_entry(&archive, "Catalog.xml")
            .ok_or_else(|| anyhow!("no Catalog.xml found in knxprod"))?;
        let catalog_xml = read_entry(&mut archive, &catalog_name)?;

        let (manufacturer_id, products) = catalog::parse_catalog(&catalog_xml)?;
        Ok(Self {
            manufacturer_id,
            products,
            path,
        })
    }

    /// Resolve a `Hardware2ProgramRefId` (from the catalog) to the full
    /// parsed application program.
    pub fn application_program(&self, h2p_ref: &str) -> Result<ApplicationProgram> {
        let file =
            std::fs::File::open(&self.path).with_context(|| format!("opening {}", self.path.display()))?;
        let mut archive = zip::ZipArchive::new(file).context("reading knxprod ZIP")?;

        let hardware_name = format!("{}/Hardware.xml", self.manufacturer_id);
        let hardware_xml = read_entry_str(&mut archive, &hardware_name)?;
        let app_ref = hardware::resolve_application_program(&hardware_xml, h2p_ref)?;

        let app_name = format!("{}/{app_ref}.xml", self.manufacturer_id);
        let app_xml = read_entry_str(&mut archive, &app_name)?;
        app::parse_application_program(&app_xml)
    }
}

fn find_entry(archive: &zip::ZipArchive<std::fs::File>, suffix: &str) -> Option<String> {
    archive
        .file_names()
        .find(|name| name.ends_with(suffix))
        .map(str::to_owned)
}

fn read_entry(archive: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Result<Vec<u8>> {
    let mut entry = archive
        .by_name(name)
        .with_context(|| format!("reading ZIP entry {name}"))?;
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut buf)?;
    Ok(buf)
}

fn read_entry_str(archive: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Result<String> {
    String::from_utf8(read_entry(archive, name)?)
        .with_context(|| format!("entry {name} is not UTF-8"))
}
