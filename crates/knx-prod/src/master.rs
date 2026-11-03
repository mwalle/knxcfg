//! `knx_master.xml`: KNX master data shipped inside every knxprod.
//!
//! Two parts are used:
//!
//! - **mask versions** with their load procedures — applications with
//!   `LoadProcedureStyle="MergedProcedure"` only carry fragments that are
//!   merged into the mask's procedure (`<LdCtrlMerge MergeId>`);
//! - **datapoint types** (`DPT-1`, `DPST-1-1`, …) for validating and naming
//!   the DPTs of group addresses and group objects.

use anyhow::{anyhow, Result};
use serde::Deserialize;

use crate::LoadStep;

#[derive(Debug, Deserialize)]
struct MasterDoc {
    #[serde(rename = "MasterData")]
    master_data: MasterXml,
}

#[derive(Debug, Deserialize, Default)]
struct MasterXml {
    #[serde(rename = "DatapointTypes", default)]
    datapoint_types: Option<DatapointTypes>,
    #[serde(rename = "MaskVersions", default)]
    mask_versions: Option<MaskVersions>,
}

#[derive(Debug, Deserialize, Default)]
struct DatapointTypes {
    #[serde(rename = "DatapointType", default)]
    items: Vec<DatapointType>,
}

#[derive(Debug, Deserialize, Default)]
struct MaskVersions {
    #[serde(rename = "MaskVersion", default)]
    items: Vec<MaskVersion>,
}

/// Parsed master data.
#[derive(Debug, Clone, Default)]
pub struct MasterData {
    pub datapoint_types: Vec<DatapointType>,
    pub mask_versions: Vec<MaskVersion>,
}

impl MasterData {
    pub fn mask_version(&self, id: &str) -> Option<&MaskVersion> {
        self.mask_versions.iter().find(|m| m.id == id)
    }

    /// Resolve a DPT reference (`DPT-9`, `DPST-9-1`, `9`, `9.001`) to
    /// `(type, subtype)`.
    pub fn datapoint(&self, dpt: &str) -> Option<(&DatapointType, Option<&DatapointSubtype>)> {
        let dpt = dpt.trim();
        let (main, sub) = if let Some(rest) = dpt.strip_prefix("DPST-") {
            let (m, s) = rest.split_once('-')?;
            (m.parse::<u32>().ok()?, Some(s.parse::<u32>().ok()?))
        } else if let Some(rest) = dpt.strip_prefix("DPT-") {
            (rest.parse::<u32>().ok()?, None)
        } else if let Some((m, s)) = dpt.split_once('.') {
            (m.parse::<u32>().ok()?, Some(s.parse::<u32>().ok()?))
        } else {
            (dpt.parse::<u32>().ok()?, None)
        };
        let t = self
            .datapoint_types
            .iter()
            .find(|t| t.number.parse::<u32>().ok() == Some(main))?;
        match sub {
            None => Some((t, None)),
            Some(s) => {
                let st = t
                    .subtypes
                    .as_ref()?
                    .items
                    .iter()
                    .find(|st| st.number.parse::<u32>().ok() == Some(s))?;
                Some((t, Some(st)))
            }
        }
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct DatapointType {
    #[serde(rename = "@Id", default)]
    pub id: String,
    #[serde(rename = "@Number", default)]
    pub number: String,
    #[serde(rename = "@Name", default)]
    pub name: String,
    #[serde(rename = "@Text", default)]
    pub text: String,
    #[serde(rename = "@SizeInBit", default)]
    pub size_in_bit: String,
    #[serde(rename = "DatapointSubtypes", default)]
    pub subtypes: Option<DatapointSubtypes>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct DatapointSubtypes {
    #[serde(rename = "DatapointSubtype", default)]
    pub items: Vec<DatapointSubtype>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct DatapointSubtype {
    #[serde(rename = "@Id", default)]
    pub id: String,
    #[serde(rename = "@Number", default)]
    pub number: String,
    #[serde(rename = "@Name", default)]
    pub name: String,
    #[serde(rename = "@Text", default)]
    pub text: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct MaskVersion {
    #[serde(rename = "@Id", default)]
    pub id: String,
    #[serde(rename = "@Name", default)]
    pub name: String,
    #[serde(rename = "@ManagementModel", default)]
    pub management_model: String,
    /// The procedures live in one or more `<HawkConfigurationData>` blocks.
    #[serde(rename = "HawkConfigurationData", default)]
    hawk: Vec<HawkConfigurationData>,
}

#[derive(Debug, Deserialize, Default, Clone)]
struct HawkConfigurationData {
    #[serde(rename = "Procedures", default)]
    procedures: Option<Procedures>,
}

impl MaskVersion {
    /// All procedures of the mask, in document order.
    pub fn procedures(&self) -> impl Iterator<Item = &MaskProcedure> {
        self.hawk
            .iter()
            .filter_map(|h| h.procedures.as_ref())
            .flat_map(|p| p.items.iter())
    }

    /// The remote `Load` procedure of a sub type (`all`, `par`, `grp`, …).
    pub fn load_procedure(&self, sub_type: &str) -> Option<&MaskProcedure> {
        self.procedures().find(|p| {
            p.procedure_type == "Load"
                && p.procedure_sub_type == sub_type
                && p.access.split_whitespace().any(|a| a == "remote")
        })
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
struct Procedures {
    #[serde(rename = "Procedure", default)]
    items: Vec<MaskProcedure>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct MaskProcedure {
    #[serde(rename = "@ProcedureType", default)]
    pub procedure_type: String,
    #[serde(rename = "@ProcedureSubType", default)]
    pub procedure_sub_type: String,
    #[serde(rename = "@Access", default)]
    pub access: String,
    #[serde(rename = "$value", default)]
    pub steps: Vec<LoadStep>,
}

pub(crate) fn parse_master(xml: &str) -> Result<MasterData> {
    let xml = xml.strip_prefix('\u{feff}').unwrap_or(xml);
    let doc: MasterDoc = quick_xml::de::from_str(xml).map_err(|e| anyhow!("knx_master.xml: {e}"))?;
    Ok(MasterData {
        datapoint_types: doc
            .master_data
            .datapoint_types
            .map(|d| d.items)
            .unwrap_or_default(),
        mask_versions: doc.master_data.mask_versions.map(|m| m.items).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mask_procedures_and_dpts() {
        let xml = r#"<KNX><MasterData>
          <DatapointTypes>
            <DatapointType Id="DPT-9" Number="9" Name="9.xxx" Text="2-byte float" SizeInBit="16">
              <DatapointSubtypes>
                <DatapointSubtype Id="DPST-9-1" Number="1" Name="DPT_Value_Temp" Text="temperature (°C)" />
              </DatapointSubtypes>
            </DatapointType>
          </DatapointTypes>
          <MaskVersions>
            <MaskVersion Id="MV-07B0" MaskVersion="1968" Name="System B" ManagementModel="SystemB">
              <HawkConfigurationData><Procedures>
                <Procedure ProcedureType="Load" ProcedureSubType="all" Access="remote local2">
                  <LdCtrlConnect />
                  <LdCtrlMerge MergeId="2" />
                  <LdCtrlMapError OriginalError="1" MappedError="0" />
                  <LdCtrlRestart />
                </Procedure>
              </Procedures></HawkConfigurationData>
            </MaskVersion>
          </MaskVersions>
        </MasterData></KNX>"#;
        let m = parse_master(xml).unwrap();
        let mv = m.mask_version("MV-07B0").unwrap();
        let p = mv.load_procedure("all").unwrap();
        assert_eq!(p.steps.len(), 4);
        assert!(matches!(&p.steps[1], LoadStep::Merge(x) if x.merge_id == "2"));
        let (t, st) = m.datapoint("DPST-9-1").unwrap();
        assert_eq!(t.size_in_bit, "16");
        assert_eq!(st.unwrap().name, "DPT_Value_Temp");
        assert!(m.datapoint("9.001").is_some());
        assert!(m.datapoint("DPT-99").is_none());
    }
}
