//! Imports the real ETS projects in `refs/knxproj` (untracked) and compares the
//! result with what ETS itself recorded in them. Skipped when they are absent.

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use knx_proj::{Imported, ProjectFile, ResolvedProduct};

fn import(sample: &str) -> Option<Imported> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../refs/knxproj");
    let path = root.join(sample);
    if !path.is_file() {
        eprintln!("skipping: {} not found", path.display());
        return None;
    }
    let file = ProjectFile::open(&path).unwrap();
    let dir = std::env::temp_dir().join(format!("knx-proj-{}-{sample}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let imported = file.convert(|h2p| resolve(&file, &dir, h2p)).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    Some(imported)
}

fn resolve(file: &ProjectFile, dir: &std::path::Path, h2p: &str) -> Result<ResolvedProduct> {
    let manufacturer = h2p.split('_').next().ok_or_else(|| anyhow!("bad h2p {h2p}"))?;
    let dest = dir.join(format!("{manufacturer}.knxprod"));
    file.extract_product(manufacturer, &dest)?;
    let kp = knx_prod::KnxProd::open(&dest)?;
    Ok(ResolvedProduct {
        application: kp.application_program(h2p)?,
        name: h2p.to_string(),
        knxprod: None,
    })
}

fn assert_matches_ets(imported: &Imported) {
    assert_eq!(imported.warnings, Vec::<String>::new());
    for check in &imported.checks {
        assert!(
            check.visibility_mismatches.is_empty(),
            "{}: {:#?}",
            check.ia,
            check.visibility_mismatches
        );
        assert_eq!(check.image_error, None, "{}", check.ia);
    }
}

#[test]
fn in16_project() {
    let Some(imported) = import("in16-tests.knxproj") else { return };
    let p = &imported.project;
    assert_eq!(p.meta.name, "in16-tests");
    assert_eq!(p.groups.len(), 17);
    assert_eq!(p.topology.len(), 2);
    assert_eq!(p.devices.len(), 2);

    let dev = p.devices.iter().find(|d| d.ia.to_string() == "1.1.2").unwrap();
    assert_eq!(dev.parameters.len(), 45);
    assert_eq!(dev.links.len(), 17);
    assert!(dev.links.iter().all(|l| l.send));
    assert_matches_ets(&imported);
    assert!(imported.checks.iter().any(|c| c.compared_objects == 39));
}

#[test]
fn jalo_project_takes_the_value_of_the_active_ref() {
    let Some(imported) = import("bim112-jalo.knxproj") else { return };
    let p = &imported.project;
    assert_eq!(p.meta.name, "bim112 Jalo");
    assert_eq!(p.groups.len(), 40);
    let dev = &p.devices[0];
    assert_eq!(dev.links.len(), 36);
    assert_matches_ets(&imported);
    let v = dev.parameters.iter().find(|v| v.id == "P-20008").unwrap();
    assert_eq!(v.value.as_str(), Some("10")); // R-123, not the inactive R-20018
}

/// A real installation (ETS 5.7, modules, several manufacturers), if present.
/// Devices may have warnings, but nothing may be lost or contradict ETS.
#[test]
fn real_installation() {
    let Some(imported) = import("Haus.knxprojarchive") else { return };
    for w in &imported.warnings {
        assert!(!w.contains("is not in "), "{w}");
        assert!(!w.contains("the value is lost"), "{w}");
        assert!(!w.contains("unknown group address"), "{w}");
    }
    let mut objects = 0;
    let mut arguments = 0;
    for check in &imported.checks {
        assert_eq!(check.visibility_mismatches, Vec::<String>::new(), "{}", check.ia);
        assert_eq!(check.argument_mismatches, Vec::<String>::new(), "{}", check.ia);
        objects += check.compared_objects;
        arguments += check.compared_arguments;
    }
    assert!(objects > 0 && arguments > 0, "nothing was compared with ETS");
}
