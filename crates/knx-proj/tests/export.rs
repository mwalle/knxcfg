//! Exports a project with devices from two knxprods (`refs/knxprod`) and
//! imports it again: everything the model holds comes back. Whether ETS takes
//! the file was checked in the VM (`docs/knxproj-format.md`, "Export").

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use knx_core::{
    Area, DeviceInstance, DeviceRef, GroupAddressEntry, GroupObjectLink, Line, ObjectSettings, ParameterValue,
    Priority, Project, Space,
};
use knx_proj::export::{export, ExportDevice};
use knx_proj::{ProjectFile, ResolvedProduct};

const BE: (&str, &str) = ("MDT_KP_BE_02_Universal_Interface_V32a.knxprod", "M-0083_H-182-0_HP-00B6-32-9642");
const AKU: (&str, &str) = ("MDT_KP_AKU_03_Universal_Actuator_V41a.knxprod", "M-0083_H-287-1_HP-00C5-41-204E");

fn knxprod(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../refs/knxprod").join(name);
    p.is_file().then_some(p)
}

fn device_ref(ia: &str, h2p: &str) -> DeviceRef {
    DeviceRef {
        ia: ia.parse().unwrap(),
        name: format!("device {ia}"),
        hardware: h2p.into(),
        application: None,
        knxprod: None,
        product: None,
        description: Some("on the wall".into()),
        comment: None,
        serial_number: Some("0083:00ABCDEF".into()),
    }
}

fn project() -> Project {
    let ga = |s: &str| s.parse().unwrap();
    let mut p = Project::default();
    p.meta.name = "export test".into();
    p.topology = vec![Area {
        number: 1,
        name: "Area 1".into(),
        lines: vec![Line {
            number: 1,
            name: "Line 1".into(),
            medium: None,
            description: Some("first line".into()),
            devices: vec![device_ref("1.1.1", BE.1), device_ref("1.1.2", AKU.1)],
        }],
    }];
    p.groups = vec![
        GroupAddressEntry {
            ga: ga("1/2/3"),
            name: "Light".into(),
            dpt: Some("DPST-1-1".into()),
            unfiltered: true,
            description: Some("kitchen <ceiling> & wall".into()),
            comment: None,
        },
        GroupAddressEntry {
            ga: ga("3/1/0"),
            name: "Central".into(),
            dpt: None,
            unfiltered: false,
            description: None,
            comment: None,
        },
    ];
    p.devices = vec![
        DeviceInstance {
            ia: "1.1.1".parse().unwrap(),
            parameters: vec![
                ParameterValue { id: "P-7".into(), value: serde_yaml::Value::String("50".into()) },
                ParameterValue {
                    id: "MD-2_M-2/MD-2_P-23".into(),
                    value: serde_yaml::Value::String("Kitchen".into()),
                },
            ],
            links: vec![
                GroupObjectLink { object_number: 0, ga: ga("1/2/3"), send: false },
                GroupObjectLink { object_number: 0, ga: ga("3/1/0"), send: true },
            ],
            objects: vec![ObjectSettings {
                object_number: 0,
                read: Some(false),
                priority: Some(Priority::High),
                text: Some("my own text".into()),
                ..Default::default()
            }],
        },
        DeviceInstance {
            ia: "1.1.2".parse().unwrap(),
            parameters: vec![],
            links: vec![GroupObjectLink { object_number: 8, ga: ga("1/2/3"), send: false }],
            objects: vec![],
        },
    ];
    p.locations = vec![Space {
        kind: "Building".into(),
        name: "House".into(),
        number: None,
        usage: None,
        description: None,
        devices: vec![],
        spaces: vec![Space {
            kind: "Room".into(),
            name: "Kitchen".into(),
            number: Some("1".into()),
            usage: None,
            description: None,
            devices: vec!["1.1.1".parse().unwrap(), "1.1.2".parse().unwrap()],
            spaces: vec![],
        }],
    }];
    p.sort();
    p
}

fn resolve(file: &ProjectFile, dir: &Path, h2p: &str) -> anyhow::Result<ResolvedProduct> {
    let dest = dir.join("M-0083.knxprod");
    if !dest.is_file() {
        file.extract_product("M-0083", &dest)?;
    }
    let kp = knx_prod::KnxProd::open(&dest)?;
    let application = kp.application_program(h2p)?;
    let table_format = kp.table_format(&application).map_err(|e| format!("{e:#}"));
    let name = kp.products.iter().find(|p| p.hardware2program_ref_id.as_deref() == Some(h2p)).unwrap().name.clone();
    Ok(ResolvedProduct { application, table_format, name, knxprod: None })
}

#[test]
fn export_and_import_again() {
    let (Some(be), Some(aku)) = (knxprod(BE.0), knxprod(AKU.0)) else {
        eprintln!("skipping: knxprods not found");
        return;
    };
    let mut kps = HashMap::new();
    for (path, h2p) in [(&be, BE.1), (&aku, AKU.1)] {
        let kp = knx_prod::KnxProd::open(path).unwrap();
        let ap = kp.application_program(h2p).unwrap();
        let product = kp.products.iter().find(|p| p.hardware2program_ref_id.as_deref() == Some(h2p)).unwrap().clone();
        kps.insert(h2p, (path.clone(), ap, product));
    }
    let project = project();
    let devices: Vec<ExportDevice> = project
        .topology
        .iter()
        .flat_map(|a| &a.lines)
        .flat_map(|l| &l.devices)
        .map(|d| {
            let (path, ap, product) = &kps[d.hardware.as_str()];
            ExportDevice {
                ia: d.ia,
                ap,
                knxprod: path.clone(),
                product: product.product_ref_id.clone().unwrap(),
                product_name: product.name.clone(),
            }
        })
        .collect();

    let dir = std::env::temp_dir().join(format!("knx-proj-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("export.knxproj");
    let warnings = export(&project, &devices, None, &file).unwrap();
    assert_eq!(warnings, ["no signing key: ETS does not import an unsigned project"]);

    let opened = ProjectFile::open(&file).unwrap();
    let imported = opened.convert(|h2p| resolve(&opened, &dir, h2p)).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(imported.warnings, Vec::<String>::new());

    let back = imported.project;
    assert_eq!(back.meta.name, project.meta.name);
    assert_eq!(back.groups, project.groups);
    assert_eq!(back.devices, project.devices);
    assert_eq!(back.locations, project.locations);
    // The export adds the backbone and the main line ETS needs, and the main
    // and middle groups of the addresses.
    let line = &back.topology.iter().find(|a| a.number == 1).unwrap().lines.iter().find(|l| l.number == 1).unwrap();
    for (d, orig) in line.devices.iter().zip(&project.topology[0].lines[0].devices) {
        assert_eq!((d.ia, &d.name, &d.description, &d.serial_number), (orig.ia, &orig.name, &orig.description, &orig.serial_number));
        assert!(d.product.is_some());
    }
    assert_eq!(line.description, project.topology[0].lines[0].description);
    assert!(back.topology.iter().any(|a| a.number == 0 && a.lines.iter().any(|l| l.number == 0)));
    let ranges: Vec<String> = back.group_ranges.iter().map(|r| format!("{}-{}", r.start, r.end)).collect();
    assert_eq!(ranges, ["1/0/0-1/7/255", "3/0/0-3/7/255"]);
}
