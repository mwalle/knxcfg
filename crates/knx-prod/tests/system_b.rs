//! The System B download plans of the AKU-02UP.03 (mask 07B0h), as ETS 5.7
//! runs them in knx-sim (`docs/sim-plan.md`, step 5).

use std::collections::HashMap;
use std::path::PathBuf;

use knx_prod::{DownloadKind, DownloadStep, KnxProd, Object};

const H2P: &str = "M-0083_H-287-1_HP-00C5-41-204E";

fn plan(kind: DownloadKind) -> Option<Vec<DownloadStep>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../refs/knxprod/MDT_KP_AKU_03_Universal_Actuator_V41a.knxprod");
    if !path.is_file() {
        eprintln!("skipping: {} not found", path.display());
        return None;
    }
    let kp = KnxProd::open(path).unwrap();
    let ap = kp.application_program(H2P).unwrap();
    let values = HashMap::new();
    let ev = knx_prod::Evaluation::new(&ap, &values);
    let image = knx_prod::device_image::build_device_image_eval(&ev, kp.table_format(&ap).unwrap(), &[]).unwrap();
    Some(knx_prod::download_plan(&ev, &image, Some(&kp.master_data().unwrap()), kind).unwrap())
}

fn touches_5(s: &DownloadStep) -> bool {
    format!("{s:?}").contains("Index(5)") || format!("{s:?}").contains("obj_idx: 5") || format!("{s:?}").contains("lsm_idx: 5")
}

#[test]
fn complete_download() {
    let Some(plan) = plan(DownloadKind::Full) else { return };
    // Only application program 1: of LSM 5 only the Unload remains.
    let five: Vec<_> = plan.iter().filter(|s| touches_5(s)).collect();
    assert_eq!(five, [&DownloadStep::Unload { object: Object::Index(5) }]);
    // The group object table covers all 60 objects of the application.
    assert!(plan.contains(&DownloadStep::RelSegment { lsm_idx: 3, size: 2 + 60 * 2, mode: 0, fill: 0 }));
    assert!(plan.contains(&DownloadStep::RelSegment { lsm_idx: 4, size: 348, mode: 1, fill: 0 }));
}

#[test]
fn partial_download() {
    let Some(plan) = plan(DownloadKind::Partial) else { return };
    assert!(!plan.iter().any(touches_5), "{plan:?}");
    // Pre-download verification: application loaded, same id.
    assert!(plan.contains(&DownloadStep::CompareProp { obj_idx: 4, prop_id: 5, inline_data: vec![1] }));
    assert!(plan.contains(&DownloadStep::CompareProp { obj_idx: 4, prop_id: 13, inline_data: vec![0x00, 0x83, 0x00, 0xC5, 0x41] }));
    // The segment address is read before and compared after the allocation.
    let read = plan.iter().position(|s| *s == DownloadStep::ReadProp { obj_idx: 4, prop_id: 7 }).unwrap();
    let alloc = plan.iter().position(|s| matches!(s, DownloadStep::RelSegment { lsm_idx: 4, mode: 0, .. })).unwrap();
    let compare = plan.iter().position(|s| *s == DownloadStep::CompareLoadedProp { obj_idx: 4, prop_id: 7 }).unwrap();
    assert!(read < alloc && alloc < compare);
}
