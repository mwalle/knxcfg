//! Regression test against the ETS5 download captured in
//! `docs/ets5-findings.md` (BE-02001.02, `M-0083_A-00B6-32-9642`, mask 0705).
//!
//! The oracle project linked `0/0/10`→GO0, `0/0/11`→GO6, `0/0/12`→GO23. GO23
//! (Logic 4 output) and GO24 ("Operation", written as `4b`) are only active
//! with Logic 4 enabled and cyclic "Operation" sending on, so the project had
//! those set; the test does the same.

use std::collections::HashMap;
use std::path::PathBuf;

use knx_prod::tables::Link;
use knx_prod::KnxProd;

const H2P: &str = "M-0083_H-182-0_HP-00B6-32-9642";

fn knxprod() -> Option<KnxProd> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../refs/knxprod/MDT_KP_BE_02_Universal_Interface_V32a.knxprod");
    if !path.is_file() {
        eprintln!("skipping: {} not found", path.display());
        return None;
    }
    Some(KnxProd::open(path).unwrap())
}

/// The table format follows the flavours in the real master data, not a list
/// of mask numbers.
#[test]
fn table_format_of_the_real_masks() {
    use knx_prod::tables::TableFormat::{self, SystemB, System7};

    let Some(kp) = knxprod() else { return };
    let master = kp.master_data().unwrap();
    let format = |id: &str| TableFormat::of(master.mask_version(id).unwrap());

    for id in ["MV-0700", "MV-0701", "MV-0705", "MV-2705", "MV-5705"] {
        assert_eq!(format(id).unwrap(), System7, "{id}");
    }
    for id in ["MV-07B0", "MV-17B0", "MV-27B0", "MV-57B0"] {
        assert_eq!(format(id).unwrap(), SystemB, "{id}");
    }
    for id in ["MV-0012", "MV-0020"] {
        assert!(format(id).unwrap_err().to_string().contains("cannot encode"), "{id}");
    }
    for id in ["MV-0912", "MV-091A"] {
        assert_eq!(format(id).unwrap(), TableFormat::Coupler, "{id}");
    }
    for id in ["MV-0900", "MV-0910", "MV-0911"] {
        assert!(format(id).unwrap_err().to_string().contains("3584 octets"), "{id}");
    }
    assert!(format("MV-2920").unwrap_err().to_string().contains("Coupler Model 2.0"));
    assert!(format("MV-0310").unwrap_err().to_string().contains("no group tables"));
}

#[test]
fn tables_match_the_ets5_download() {
    let Some(kp) = knxprod() else { return };
    let ap = kp.application_program(H2P).unwrap();

    let mut values = HashMap::new();
    values.insert(knx_prod::eval::full_key(&ap, "MD-3_M-52/MD-3_P-1"), "1".to_string());
    values.insert(knx_prod::eval::full_key(&ap, "P-4"), "1".to_string());
    let links = [Link::new(0, 0x000A), Link::new(6, 0x000B), Link::new(23, 0x000C)];

    let ev = knx_prod::Evaluation::new(&ap, &values);
    let format = kp.table_format(&ap).unwrap();
    let image = knx_prod::device_image::build_device_image_eval(&ev, format, &links).unwrap();
    assert!(image.warnings.is_empty(), "{:?}", image.warnings);

    assert_eq!(image.address_table.bytes, [0x04, 0x00, 0x00, 0x00, 0x0A, 0x00, 0x0B, 0x00, 0x0C]);
    assert_eq!(image.association_table.bytes, [0x03, 0x01, 0x00, 0x02, 0x06, 0x03, 0x17]);

    let got = &image.group_object_table.bytes;
    assert_eq!(&got[..12], &[0x19, 0x07, 0x00, 0x07, 0x1A, 0x4F, 0x00, 0x07, 0x22, 0xDB, 0x00, 0x07]);
    let config = |asap: usize| got[3 + asap * 4 + 2];
    assert_eq!(config(6), 0x4F);
    assert_eq!(config(23), 0x4F);
    assert_eq!(config(24), 0x4B); // active, unlinked: communication cleared

    let master = kp.master_data().unwrap();
    let plan = knx_prod::download_plan(&ev, &image, Some(&master), knx_prod::DownloadKind::Full).unwrap();
    assert!(plan.iter().any(|s| matches!(
        s,
        knx_prod::DownloadStep::TaskSegment { lsm_idx: 3, app_id: [0x00, 0x83, 0x00, 0xB6, 0x32], pei_type: 1, .. }
    )));
}
