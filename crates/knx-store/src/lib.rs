//! Git-friendly YAML persistence for a KNX project directory.
//!
//! Layout (see `docs/plan.md` §6):
//!
//! ```text
//! project/
//! ├─ knx.yaml          # ProjectMeta
//! ├─ topology.yaml     # Vec<Area>
//! ├─ groups.yaml       # Vec<GroupAddressEntry>
//! └─ devices/
//!    └─ <ia>.yaml      # DeviceInstance
//! ```
//!
//! All files are written in a canonical order so that identical projects
//! produce byte-identical files (clean git diffs).

use anyhow::{Context, Result};
use knx_core::{Area, DeviceInstance, GroupAddressEntry, Project, ProjectMeta};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs;
use std::path::Path;

const META_FILE: &str = "knx.yaml";
const TOPOLOGY_FILE: &str = "topology.yaml";
const GROUPS_FILE: &str = "groups.yaml";
const DEVICES_DIR: &str = "devices";

/// Load a project directory into the typed model.
pub fn load(dir: &Path) -> Result<Project> {
    let meta: ProjectMeta = read_yaml(&dir.join(META_FILE))?;
    let topology: Vec<Area> = read_yaml_opt(&dir.join(TOPOLOGY_FILE))?;
    let groups: Vec<GroupAddressEntry> = read_yaml_opt(&dir.join(GROUPS_FILE))?;
    let devices = load_devices(dir)?;

    let mut project = Project {
        meta,
        topology,
        groups,
        devices,
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

    let devices_dir = dir.join(DEVICES_DIR);
    fs::create_dir_all(&devices_dir)?;
    for dev in &p.devices {
        write_yaml(&devices_dir.join(format!("{}.yaml", dev.ia)), dev)?;
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

fn write_yaml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = serde_yaml::to_string(value)?;
    fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use knx_core::{Area, DeviceRef, GroupObjectLink, Line, ParameterValue};

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
                devices: vec![DeviceRef {
                    ia: "2.1.3".parse().unwrap(),
                    name: "Actuator".into(),
                    hardware: "M-0083_H-180-0_HP-00B4-32-3919".into(),
                    application: None,
                    knxprod: None,
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
            }],
        });

        save(&dir, &project).unwrap();
        let loaded = load(&dir).unwrap();

        let mut expected = project.clone();
        expected.sort();
        assert_eq!(loaded, expected);

        let _ = fs::remove_dir_all(&dir);
    }
}
