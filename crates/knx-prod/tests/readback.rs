//! Reading a configuration back from device memory gives what was
//! downloaded: parameters, links and object settings (System 7 and B).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use knx_prod::device_image::{build_device_image_with, decode_device, DeviceMemory};
use knx_prod::eval::full_key;
use knx_prod::tables::{Link, ObjectOverride};
use knx_prod::{Evaluation, KnxProd};

fn knxprod(name: &str) -> Option<KnxProd> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../refs/knxprod").join(name);
    p.is_file().then(|| KnxProd::open(p).unwrap())
}

/// `unrecoverable`: parameters kept nowhere in the device.
fn round_trip(
    kp: &KnxProd,
    h2p: &str,
    values: &[(&str, &str)],
    links: &[Link],
    overrides: &[ObjectOverride],
    unrecoverable: &[&str],
) {
    let ap = kp.application_program(h2p).unwrap();
    let format = kp.table_format(&ap).unwrap();
    let values: HashMap<String, String> = values.iter().map(|(k, v)| (full_key(&ap, k), v.to_string())).collect();
    let ev = Evaluation::new(&ap, &values);
    let image = build_device_image_with(&ev, format, links, overrides).unwrap();
    let placed = |t: &knx_prod::TableImage| if t.placement.is_some() { Vec::new() } else { t.bytes.clone() };
    let memory = DeviceMemory {
        segments: image.segments.clone(),
        address_table: placed(&image.address_table),
        association_table: placed(&image.association_table),
        group_object_table: placed(&image.group_object_table),
        table_segments: Vec::new(),
    };
    // With the project's values for the parameters that are not in memory.
    let read = decode_device(&ap, format, &memory, &values).unwrap();
    assert_eq!(read.warnings, Vec::<String>::new());
    let expected: BTreeMap<String, String> = values.clone().into_iter().collect();
    assert_eq!(read.parameters, expected);
    let mut want = links.to_vec();
    want.sort_by_key(|l| (l.asap, !l.send, l.ga));
    let mut got = read.links.clone();
    got.sort_by_key(|l| (l.asap, !l.send, l.ga));
    assert_eq!(got, want);
    assert_eq!(read.overrides, overrides);

    // Without them the search finds the choices that decide what is active;
    // values kept nowhere at all (texts) stay unknown.
    let blind = decode_device(&ap, format, &memory, &HashMap::new()).unwrap();
    assert_eq!(blind.warnings, Vec::<String>::new());
    let mut expected = expected;
    expected.retain(|k, _| !unrecoverable.contains(&knx_prod::eval::short_key(&ap, k).as_str()));
    assert_eq!(blind.parameters, expected);
    assert_eq!(blind.links.len(), links.len());
}

#[test]
fn system_7_read_back() {
    let Some(kp) = knxprod("MDT_KP_BE_02_Universal_Interface_V32a.knxprod") else { return };
    round_trip(
        &kp,
        "M-0083_H-182-0_HP-00B6-32-9642",
        &[("P-7", "50"), ("MD-2_M-2/MD-2_P-23", "Kueche"), ("MD-2_M-3/MD-2_P-24", "1")],
        &[Link { asap: 0, ga: 0x0A03, send: true }, Link { asap: 0, ga: 0x1900, send: false }],
        &[
            ObjectOverride { asap: 0, read: Some(false), priority: Some(0b01), ..Default::default() },
            ObjectOverride { asap: 6, transmit: Some(false), ..Default::default() },
        ],
        &["MD-2_M-2/MD-2_P-23"],
    );
}

#[test]
fn system_b_read_back() {
    let Some(kp) = knxprod("MDT_KP_AKU_03_Universal_Actuator_V41a.knxprod") else { return };
    round_trip(
        &kp,
        "M-0083_H-287-1_HP-00C5-41-204E",
        &[("P-12", "5")],
        &[Link { asap: 8, ga: 0x0A03, send: true }],
        &[
            ObjectOverride { asap: 1, read: Some(true), read_on_init: Some(true), priority: Some(0b10), ..Default::default() },
            ObjectOverride { asap: 3, write: Some(false), update: Some(true), ..Default::default() },
        ],
        &[],
    );
}

