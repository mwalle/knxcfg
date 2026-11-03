//! Canonical typed KNX project model.
//!
//! This crate is the single source of truth for a KNX project in memory.
//! Every file format (our YAML store, `.knxproj`, knxprod) is a
//! serialization boundary around these types — see `docs/plan.md` §5.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

pub const FORMAT_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Addresses
// ---------------------------------------------------------------------------

/// 16-bit Individual Address (physical address), `Area.Line.Device`
/// (e.g. `1.1.1`). Area and line are 0..=15, device is 0..=255.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IndividualAddress {
    pub area: u8,
    pub line: u8,
    pub device: u8,
}

impl IndividualAddress {
    pub fn new(area: u8, line: u8, device: u8) -> Result<Self, AddressError> {
        if area > 15 || line > 15 {
            return Err(AddressError::Range("area and line must be 0..=15"));
        }
        Ok(Self { area, line, device })
    }

    /// The broadcast/programming-mode address `0.0.0`.
    pub fn is_zero(&self) -> bool {
        self.area == 0 && self.line == 0 && self.device == 0
    }

    /// Raw 16-bit individual address: `area(4) << 12 | line(4) << 8 | device(8)`.
    pub fn to_u16(self) -> u16 {
        ((self.area as u16) << 12) | ((self.line as u16) << 8) | self.device as u16
    }

    /// Why this address cannot be given to an ordinary device, if so:
    /// `x.y.0` belongs to the line/area coupler and `15.15.255` is the
    /// factory default of unprogrammed devices.
    pub fn reserved_reason(&self) -> Option<&'static str> {
        if self.device == 0 {
            Some("device address 0 is reserved for line and area couplers")
        } else if self.area == 15 && self.line == 15 && self.device == 255 {
            Some("15.15.255 is the factory default address of unprogrammed devices")
        } else {
            None
        }
    }
}

impl fmt::Display for IndividualAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.area, self.line, self.device)
    }
}

impl FromStr for IndividualAddress {
    type Err = AddressError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() != 3 {
            return Err(AddressError::Format(s.to_string()));
        }
        let area = parts[0]
            .parse::<u8>()
            .map_err(|_| AddressError::Format(s.to_string()))?;
        let line = parts[1]
            .parse::<u8>()
            .map_err(|_| AddressError::Format(s.to_string()))?;
        let device = parts[2]
            .parse::<u8>()
            .map_err(|_| AddressError::Format(s.to_string()))?;
        Self::new(area, line, device)
    }
}

impl Serialize for IndividualAddress {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for IndividualAddress {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// 16-bit Group Address (3-level style), `Main/Middle/Sub` (e.g. `1/2/3`).
/// Main is 0..=31, middle is 0..=7, sub is 0..=255.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GroupAddress {
    pub main: u8,
    pub middle: u8,
    pub sub: u8,
}

impl GroupAddress {
    pub fn new(main: u8, middle: u8, sub: u8) -> Result<Self, AddressError> {
        if main > 31 {
            return Err(AddressError::Range("main group must be 0..=31"));
        }
        if middle > 7 {
            return Err(AddressError::Range("middle group must be 0..=7"));
        }
        Ok(Self { main, middle, sub })
    }

    /// Raw 16-bit group address as carried in the Group Address Table and
    /// cEMI `GroupAddress` field (3-level style: main is 5 bits, middle is
    /// 3 bits, sub is 8 bits).
    pub fn to_u16(self) -> u16 {
        ((self.main as u16) << 11) | ((self.middle as u16) << 8) | self.sub as u16
    }

    pub fn from_u16(raw: u16) -> Self {
        Self {
            main: (raw >> 11) as u8,
            middle: ((raw >> 8) & 0x07) as u8,
            sub: raw as u8,
        }
    }

    /// `0/0/0` is the broadcast address; it cannot be linked to an object.
    pub fn is_broadcast(&self) -> bool {
        self.to_u16() == 0
    }

    /// Two-level notation `main/sub` (sub = 11 bits).
    pub fn to_two_level(self) -> String {
        format!("{}/{}", self.main, self.to_u16() & 0x07FF)
    }
}

impl fmt::Display for GroupAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.main, self.middle, self.sub)
    }
}

/// Accepts the three notations ETS uses: three-level `main/middle/sub`,
/// two-level `main/sub` (sub 0..=2047) and free `raw` (0..=65535). The
/// address is always stored and printed three-level.
impl FromStr for GroupAddress {
    type Err = AddressError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || AddressError::Format(s.to_string());
        let parts: Vec<&str> = s.trim().split('/').collect();
        match parts.as_slice() {
            [main, middle, sub] => Self::new(
                main.parse().map_err(|_| bad())?,
                middle.parse().map_err(|_| bad())?,
                sub.parse().map_err(|_| bad())?,
            ),
            [main, sub] => {
                let main: u8 = main.parse().map_err(|_| bad())?;
                let sub: u16 = sub.parse().map_err(|_| bad())?;
                if main > 31 {
                    return Err(AddressError::Range("main group must be 0..=31"));
                }
                if sub > 2047 {
                    return Err(AddressError::Range("two-level sub group must be 0..=2047"));
                }
                Ok(Self::from_u16(((main as u16) << 11) | sub))
            }
            [raw] => Ok(Self::from_u16(raw.parse().map_err(|_| bad())?)),
            _ => Err(bad()),
        }
    }
}

impl Serialize for GroupAddress {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for GroupAddress {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Error while parsing or constructing an address.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AddressError {
    #[error("address out of range: {0}")]
    Range(&'static str),
    #[error("invalid address format: {0}")]
    Format(String),
}

// ---------------------------------------------------------------------------
// Project model
// ---------------------------------------------------------------------------

/// The aggregate project, as loaded from / saved to a project directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub meta: ProjectMeta,
    #[serde(default)]
    pub topology: Vec<Area>,
    #[serde(default)]
    pub groups: Vec<GroupAddressEntry>,
    #[serde(default)]
    pub devices: Vec<DeviceInstance>,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            meta: ProjectMeta {
                format_version: FORMAT_VERSION,
                name: "Unnamed project".to_string(),
            },
            topology: Vec::new(),
            groups: Vec::new(),
            devices: Vec::new(),
        }
    }
}

impl Project {
    /// Normalize all collections into a canonical, deterministic order so
    /// that saving the same project always yields byte-identical files.
    pub fn sort(&mut self) {
        self.topology.sort_by_key(|a| a.number);
        for area in &mut self.topology {
            area.lines.sort_by_key(|l| l.number);
            for line in &mut area.lines {
                line.devices.sort_by_key(|d| d.ia);
            }
        }
        self.groups.sort_by_key(|g| g.ga);
        self.devices.sort_by_key(|d| d.ia);
        for dev in &mut self.devices {
            dev.parameters.sort_by(|a, b| a.id.cmp(&b.id));
            dev.links
                .sort_by(|a, b| (a.object_number, a.ga).cmp(&(b.object_number, b.ga)));
        }
    }
}

/// Metadata stored in `knx.yaml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub format_version: u32,
    pub name: String,
}

/// A topological area, stored in `topology.yaml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Area {
    pub number: u8,
    pub name: String,
    #[serde(default)]
    pub lines: Vec<Line>,
}

/// A line inside an area.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub number: u8,
    pub name: String,
    #[serde(default)]
    pub devices: Vec<DeviceRef>,
}

/// Reference to a device in the topology (identity + hardware only).
/// Parameters and group-object links live in `devices/<ia>.yaml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceRef {
    pub ia: IndividualAddress,
    pub name: String,
    /// knxprod Hardware2ProgramRefId (e.g. `M-0083_H-180-0_HP-00B4-32-3919`).
    pub hardware: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application: Option<String>,
    /// Path to the knxprod file providing this device's product data.
    /// Recorded at `add` time so `edit`/`download` don't need it again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knxprod: Option<String>,
}

/// A group address entry, stored in `groups.yaml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupAddressEntry {
    pub ga: GroupAddress,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpt: Option<String>,
}

/// Device instance data (parameters + links), stored in `devices/<ia>.yaml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceInstance {
    pub ia: IndividualAddress,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<ParameterValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<GroupObjectLink>,
}

/// A single parameter, stored by symbolic id + value.
///
/// M0 note: `value` is an untyped YAML value. M1 ties it to the parameter
/// type declared in the knxprod (`docs/plan.md` §6, decision D8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParameterValue {
    pub id: String,
    pub value: serde_yaml::Value,
}

/// A link between a group object (identified by its number within the
/// application program) and a group address.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GroupObjectLink {
    pub object_number: u16,
    pub ga: GroupAddress,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ia_string_roundtrip() {
        let ia: IndividualAddress = "1.2.3".parse().unwrap();
        assert_eq!(ia.to_string(), "1.2.3");
        assert_eq!(ia.area, 1);
        assert_eq!(ia.line, 2);
        assert_eq!(ia.device, 3);
    }

    #[test]
    fn ga_string_roundtrip() {
        let ga: GroupAddress = "3/4/200".parse().unwrap();
        assert_eq!(ga.to_string(), "3/4/200");
    }

    #[test]
    fn ga_notations() {
        let three: GroupAddress = "1/2/3".parse().unwrap();
        assert_eq!("1/515".parse::<GroupAddress>().unwrap(), three);
        assert_eq!("2563".parse::<GroupAddress>().unwrap(), three);
        assert_eq!(three.to_two_level(), "1/515");
        assert!("1/2048".parse::<GroupAddress>().is_err());
        assert!("65536".parse::<GroupAddress>().is_err());
        assert!("0/0/0".parse::<GroupAddress>().unwrap().is_broadcast());
    }

    #[test]
    fn reserved_individual_addresses() {
        assert!("1.1.0".parse::<IndividualAddress>().unwrap().reserved_reason().is_some());
        assert!("15.15.255".parse::<IndividualAddress>().unwrap().reserved_reason().is_some());
        assert!("1.1.1".parse::<IndividualAddress>().unwrap().reserved_reason().is_none());
    }

    #[test]
    fn ia_range_checked() {
        assert!("16.0.1".parse::<IndividualAddress>().is_err());
        assert!("1.16.1".parse::<IndividualAddress>().is_err());
    }

    #[test]
    fn ordering() {
        let mut v = vec![
            GroupAddress::new(2, 0, 0).unwrap(),
            GroupAddress::new(1, 7, 255).unwrap(),
            GroupAddress::new(1, 0, 0).unwrap(),
        ];
        v.sort();
        assert_eq!(v[0].to_string(), "1/0/0");
        assert_eq!(v[2].to_string(), "2/0/0");
    }

    #[test]
    fn group_address_raw_encoding() {
        // 0/0/10 → main=0 (5 bits), middle=0 (3 bits), sub=10 (8 bits).
        assert_eq!(GroupAddress::new(0, 0, 10).unwrap().to_u16(), 0x000A);
        // 1/2/3 → 1<<11 | 2<<8 | 3 = 0x0800 | 0x0200 | 0x03 = 0x0A03.
        assert_eq!(GroupAddress::new(1, 2, 3).unwrap().to_u16(), 0x0A03);
    }
}
