//! Replays ETS 5.7's download of a real BE-02001.02 (frames from ETS5.log,
//! `refs/ets5/be-02001-download.txt`, see `docs/sim-plan.md`) against the
//! simulated device and compares every frame the simulation sends with the
//! real device's.

use knx_sim::device::{pid, Device};
use knx_transport::cemi::LData;
use knx_transport::CemiFrame;
use std::path::PathBuf;

const H2P: &str = "M-0083_H-182-0_HP-00B6-32-9642";
/// ETS's address in the capture (the interface filled in source 0000h).
const ETS: u16 = 0x11F0;

fn refs(rel: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../refs").join(rel);
    if !p.is_file() {
        eprintln!("skipping: {} not found", p.display());
        return None;
    }
    Some(p)
}

fn frame(hex: &str) -> LData {
    let b: Vec<u8> = hex.split_whitespace().map(|x| u8::from_str_radix(x, 16).unwrap()).collect();
    LData::parse(&CemiFrame::decode(&b).unwrap()).unwrap()
}

#[test]
fn replay_ets5_download() {
    let (Some(knxprod), Some(capture)) =
        (refs("knxprod/MDT_KP_BE_02_Universal_Interface_V32a.knxprod"), refs("ets5/be-02001-download.txt"))
    else {
        return;
    };
    let mut dev: Device = knx_sim::profile::device(&knxprod, H2P, 0x1103).unwrap();
    // The real device's maximum APDU length (not in the knxprod).
    dev.set_property(0, pid::MAX_APDULENGTH, &[0x00, 0xFE]);

    let text = std::fs::read_to_string(capture).unwrap();
    let mut real = Vec::new();
    let mut sim = Vec::new();
    // Whether the frame answers a read of the serial number, which differs.
    let mut serial = Vec::new();
    for line in text.lines() {
        if let Some(hex) = line.strip_prefix("> ") {
            let mut req = frame(hex);
            req.src = ETS;
            let reads_serial = req.apdu().is_some_and(|a| a.len() >= 4 && a[1] == 0xD5 && a[3] == pid::SERIAL_NUMBER);
            let mut notes = Vec::new();
            for o in dev.receive(&req, &mut notes) {
                sim.push((o.dst, o.tpci, o.apdu.clone()));
                serial.push(reads_serial && !o.apdu.is_empty());
            }
            assert!(notes.iter().all(|n| !n.contains("Error")), "{notes:?}");
        } else if let Some(hex) = line.strip_prefix("< ") {
            let ld = frame(hex);
            assert_eq!(ld.src, 0x1103);
            real.push((ld.dst, ld.tpci(), ld.apdu().unwrap_or_default()));
        }
    }
    assert_eq!(sim.len(), real.len(), "number of frames sent by the device");
    assert_eq!(real.len(), 79);
    for (i, (s, r)) in sim.iter().zip(&real).enumerate() {
        if serial[i] {
            assert_eq!((s.0, s.1, &s.2[..6]), (r.0, r.1, &r.2[..6]), "frame {i}");
        } else {
            assert_eq!(s, r, "frame {i}");
        }
    }

    // The tables of the capture (`docs/ets5-findings.md`) are in memory and
    // all three loadable parts are loaded.
    let dump = dev.dump();
    for expected in [
        "object 1 type 1\n  load state 1 Loaded\n",
        "object 2 type 2\n  load state 1 Loaded\n",
        "object 3 type 3\n  load state 1 Loaded\n",
        "  run state running\n",
        "  pid 13: 00 83 00 B6 32\n",
        "  4000: 04 -- -- 00 0A 00 0B 00 0C",
        "  4200: -- 03 01 00 02 06 03 17 --",
        "  4400: 19 07 00 07 1A 4B 00 07 22 DB",
    ] {
        assert!(dump.contains(expected), "missing {expected:?} in\n{dump}");
    }
}
