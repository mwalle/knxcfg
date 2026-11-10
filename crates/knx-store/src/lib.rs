//! Git-friendly YAML persistence for a KNX project directory.
//!
//! Layout (see `docs/plan.md` §6):
//!
//! ```text
//! project/
//! ├─ knx.yaml          # ProjectMeta
//! ├─ topology.yaml     # Vec<Area>
//! ├─ groups.yaml       # Vec<GroupAddressEntry>
//! ├─ ranges.yaml       # Vec<GroupRange> (only if there are any)
//! ├─ locations.yaml    # Vec<Space> (only if there are any)
//! └─ devices/
//!    └─ <ia>.yaml      # DeviceInstance
//! ```
//!
//! All files are written in a canonical order so that identical projects
//! produce byte-identical files (clean git diffs).

use anyhow::{Context, Result};
use knx_core::{Area, DeviceInstance, GroupAddressEntry, GroupRange, Project, ProjectMeta, Space};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs;
use std::path::Path;

const META_FILE: &str = "knx.yaml";
const TOPOLOGY_FILE: &str = "topology.yaml";
const GROUPS_FILE: &str = "groups.yaml";
const RANGES_FILE: &str = "ranges.yaml";
const LOCATIONS_FILE: &str = "locations.yaml";
const DEVICES_DIR: &str = "devices";

/// Load a project directory into the typed model.
pub fn load(dir: &Path) -> Result<Project> {
    let meta: ProjectMeta = read_yaml(&dir.join(META_FILE))?;
    let topology: Vec<Area> = read_yaml_opt(&dir.join(TOPOLOGY_FILE))?;
    let groups: Vec<GroupAddressEntry> = read_yaml_opt(&dir.join(GROUPS_FILE))?;
    let devices = load_devices(dir)?;
    let group_ranges: Vec<GroupRange> = read_yaml_opt(&dir.join(RANGES_FILE))?;
    let locations: Vec<Space> = read_yaml_opt(&dir.join(LOCATIONS_FILE))?;

    let mut project = Project {
        meta,
        topology,
        groups,
        devices,
        group_ranges,
        locations,
    };
    project.sort();
    Ok(project)
}

/// Save the typed model to a project directory (creating it if needed).
pub fn save(dir: &Path, project: &Project) -> Result<()> {
    let mut p = project.clone();
    p.sort();

    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    write_yaml(&dir.join(META_FILE), &p.meta)?;
    write_yaml(&dir.join(TOPOLOGY_FILE), &p.topology)?;
    write_yaml(&dir.join(GROUPS_FILE), &p.groups)?;
    write_yaml_unless_empty(&dir.join(RANGES_FILE), &p.group_ranges)?;
    write_yaml_unless_empty(&dir.join(LOCATIONS_FILE), &p.locations)?;

    let devices_dir = dir.join(DEVICES_DIR);
    fs::create_dir_all(&devices_dir)?;
    let mut keep = std::collections::HashSet::new();
    for dev in &p.devices {
        let name = format!("{}.yaml", dev.ia);
        write_yaml(&devices_dir.join(&name), dev)?;
        keep.insert(name);
    }
    // Remove the files of devices that are no longer in the project, so a
    // removed device does not come back on the next load.
    for entry in fs::read_dir(&devices_dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.ends_with(".yaml") && !keep.contains(name) {
            fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        }
    }
    Ok(())
}

fn load_devices(dir: &Path) -> Result<Vec<DeviceInstance>> {
    let devices_dir = dir.join(DEVICES_DIR);
    if !devices_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut devices = Vec::new();
    for entry in fs::read_dir(&devices_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let dev: DeviceInstance = read_yaml(&path)?;
        devices.push(dev);
    }
    Ok(devices)
}

fn read_yaml<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn read_yaml_opt<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    if path.is_file() {
        read_yaml(path)
    } else {
        Ok(T::default())
    }
}

/// Write `items`, or remove the file when there are none, so that projects
/// without them keep their four files.
fn write_yaml_unless_empty<T: Serialize>(path: &Path, items: &[T]) -> Result<()> {
    if items.is_empty() {
        if path.exists() {
            fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
        }
        return Ok(());
    }
    write_yaml(path, &items)
}

fn write_yaml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = serde_yaml::to_string(value)?;
    fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use knx_core::{Area, DeviceRef, GroupObjectLink, GroupRange, Line, Medium, ObjectSettings, ParameterValue, Space};

    #[test]
    fn roundtrip_preserves_data_and_order() {
        let dir = std::env::temp_dir().join("knx-store-roundtrip");
        let _ = fs::remove_dir_all(&dir);

        let mut project = Project::default();
        project.meta.name = "test".to_string();
        // Insert out of order to verify sorting on save.
        project.topology.push(Area {
            number: 2,
            name: "Area 2".into(),
            lines: vec![Line {
                number: 1,
                name: "Line 1".into(),
                medium: Some(Medium::Ip),
                description: None,
                devices: vec![DeviceRef {
                    ia: "2.1.3".parse().unwrap(),
                    name: "Actuator".into(),
                    hardware: "M-0083_H-180-0_HP-00B4-32-3919".into(),
                    application: None,
                    knxprod: None,
                    product: None,
                    description: Some("Kitchen".into()),
                    comment: None,
                    serial_number: Some("0083:12345678".into()),
                }],
            }],
        });
        project.topology.push(Area {
            number: 1,
            name: "Area 1".into(),
            lines: vec![],
        });
        project.groups.push(GroupAddressEntry {
            ga: "1/2/3".parse().unwrap(),
            name: "Light".into(),
            dpt: Some("DPST-1-1".into()),
            unfiltered: false,
            description: None,
            comment: None,
        });
        project.group_ranges.push(GroupRange {
            start: "1/0/0".parse().unwrap(),
            end: "1/7/255".parse().unwrap(),
            name: "Lights".into(),
            description: None,
            ranges: vec![],
        });
        project.locations.push(Space {
            kind: "Building".into(),
            name: "House".into(),
            number: None,
            usage: None,
            description: None,
            devices: vec!["2.1.3".parse().unwrap()],
            spaces: vec![],
        });
        project.devices.push(DeviceInstance {
            ia: "2.1.3".parse().unwrap(),
            parameters: vec![ParameterValue {
                id: "param-b".into(),
                value: serde_yaml::Value::Bool(true),
            }],
            links: vec![GroupObjectLink {
                object_number: 1,
                ga: "1/2/3".parse().unwrap(),
                send: true,
            }],
            objects: vec![ObjectSettings { object_number: 1, transmit: Some(false), ..Default::default() }],
        });

        save(&dir, &project).unwrap();
        let loaded = load(&dir).unwrap();

        let mut expected = project.clone();
        expected.sort();
        assert_eq!(loaded, expected);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn removed_devices_are_deleted() {
        let dir = std::env::temp_dir().join("knx-store-remove");
        let _ = fs::remove_dir_all(&dir);
        let mut project = Project::default();
        project.devices.push(DeviceInstance {
            ia: "1.1.3".parse().unwrap(),
            parameters: vec![],
            links: vec![],
            objects: vec![],
        });
        save(&dir, &project).unwrap();
        assert!(dir.join("devices/1.1.3.yaml").is_file());
        project.devices.clear();
        save(&dir, &project).unwrap();
        assert!(!dir.join("devices/1.1.3.yaml").exists());
        assert!(load(&dir).unwrap().devices.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
