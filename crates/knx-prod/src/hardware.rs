//! `Hardware.xml` parsing (h2p ref → application program ref).

use anyhow::{anyhow, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(crate) struct HardwareDoc {
    #[serde(rename = "ManufacturerData")]
    pub manufacturer_data: HardwareManufacturerData,
}

#[derive(Debug, Deserialize)]
pub(crate) struct HardwareManufacturerData {
    #[serde(rename = "Manufacturer")]
    pub manufacturer: HardwareManufacturer,
}

#[derive(Debug, Deserialize)]
pub(crate) struct HardwareManufacturer {
    #[serde(rename = "Hardware", default)]
    pub hardware: Option<Hardware>,
}

/// `<Hardware>` is recursive in knxprod: an outer container holds inner
/// `<Hardware>` elements, each of which holds `<Hardware2Programs>`.
#[derive(Debug, Deserialize, Default)]
pub(crate) struct Hardware {
    #[serde(rename = "Hardware", default)]
    pub nested: Vec<Hardware>,
    #[serde(rename = "Hardware2Programs", default)]
    pub hardware2programs: Option<Hardware2Programs>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct Hardware2Programs {
    #[serde(rename = "Hardware2Program", default)]
    pub items: Vec<Hardware2Program>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct Hardware2Program {
    #[serde(rename = "@Id", default)]
    pub id: String,
    #[serde(rename = "ApplicationProgramRef", default)]
    pub application_program_ref: Option<ApplicationProgramRef>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct ApplicationProgramRef {
    #[serde(rename = "@RefId", default)]
    pub ref_id: String,
}

/// Parse `Hardware.xml` and return the application-program ref id for a
/// given Hardware2Program ref id (the `Hardware2ProgramRefId` from the
/// catalog).
pub(crate) fn resolve_application_program(xml: &str, h2p_ref: &str) -> Result<String> {
    let doc: HardwareDoc = quick_xml::de::from_str(xml).map_err(|e| anyhow!("Hardware.xml: {e}"))?;
    let root = doc.manufacturer_data.manufacturer.hardware;

    for h2p in flatten_hardware2programs(root.as_ref()) {
        if h2p.id == h2p_ref {
            if let Some(r) = &h2p.application_program_ref {
                return Ok(r.ref_id.clone());
            }
        }
    }
    Err(anyhow!("no application program for Hardware2Program {h2p_ref}"))
}

fn flatten_hardware2programs(hardware: Option<&Hardware>) -> Vec<&Hardware2Program> {
    let mut out = Vec::new();
    if let Some(h) = hardware {
        if let Some(p) = &h.hardware2programs {
            out.extend(p.items.iter());
        }
        for nested in &h.nested {
            out.extend(flatten_hardware2programs(Some(nested)));
        }
    }
    out
}
