//! Product store: a set of directories holding `.knxprod` files that the CLI
//! searches automatically so commands only need a `Hardware2ProgramRefId`
//! (h2p), not a file path.
//!
//! Search order for `resolve`:
//! 1. an explicit `--knxprod <file>` override (validated to provide the h2p),
//! 2. the device's recorded knxprod path (D13 fast path, if the file exists;
//!    relative paths are relative to the project),
//! 3. the project's `vendor/` directory,
//! 4. the global store directory.
//!
//! The global store lives at `$XDG_DATA_HOME/knxcfg/knxprod` (fallback
//! `~/.local/share/knxcfg/knxprod`). Files can be copied into either store by
//! hand or with `knxcfg import-product <file>` (project `vendor/` by default,
//! `--global` for the global store).

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

/// The global product-store directory (auto-scanned).
pub fn global_store_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("knxcfg").join("knxprod")
}

/// The project-local vendored knxprod directory (`<project>/vendor`).
pub fn vendor_dir(project: &Path) -> PathBuf {
    project.join("vendor")
}

/// The knxprod path to record for a device: relative to the project when the
/// file lives inside it (e.g. `vendor/…`), so the project stays portable
/// through git; otherwise the absolute path (the h2p lookup in the stores is
/// the fallback when it does not exist on another machine).
pub fn record_path(project: &Path, file: &Path) -> String {
    let abs = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    if let Ok(root) = std::fs::canonicalize(project) {
        if let Ok(rel) = abs.strip_prefix(&root) {
            return rel.to_string_lossy().into_owned();
        }
    }
    abs.to_string_lossy().into_owned()
}

/// Copy knxprod files into a store directory (global or project vendor).
pub fn import_files(files: &[PathBuf], dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    for f in files {
        let name = f
            .file_name()
            .ok_or_else(|| anyhow!("{} has no file name", f.display()))?;
        let dst = dest.join(name);
        if dst.exists() {
            bail!(
                "{} already exists (remove it first or import into a different store)",
                dst.display()
            );
        }
        std::fs::copy(f, &dst)
            .with_context(|| format!("copying {} to {}", f.display(), dst.display()))?;
        println!("Imported {}", dst.display());
    }
    Ok(())
}

/// Find the knxprod file that provides `h2p`.
///
/// * `project` — project root (its `vendor/` dir is searched).
/// * `recorded` — knxprod path recorded on the device at `add` time.
/// * `explicit` — `--knxprod <file>` override.
pub fn resolve(
    h2p: &str,
    project: Option<&Path>,
    recorded: Option<&Path>,
    explicit: Option<&Path>,
) -> Result<PathBuf> {
    if let Some(p) = explicit {
        if !provides(p, h2p)? {
            bail!("{} does not provide {h2p}", p.display());
        }
        return Ok(p.to_path_buf());
    }

    if let Some(p) = recorded {
        // Recorded paths inside the project are stored relative to it.
        let p = match project {
            Some(dir) if p.is_relative() => dir.join(p),
            _ => p.to_path_buf(),
        };
        if p.is_file() {
            return Ok(p);
        }
    }

    if let Some(dir) = project {
        if let Some(p) = find_in_dir(&vendor_dir(dir), h2p)? {
            return Ok(p);
        }
    }

    if let Some(p) = find_in_dir(&global_store_dir(), h2p)? {
        return Ok(p);
    }

    bail!(
        "no knxprod provides {h2p}; import one with `knxcfg import-product <file>` \
         or pass --knxprod <file>"
    )
}

/// Sorted `.knxprod` files in a directory (empty if it does not exist).
pub fn knxprod_files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        let is_knxprod = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("knxprod"))
            .unwrap_or(false);
        if is_knxprod {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

/// Search a directory for a knxprod file that provides `h2p`.
fn find_in_dir(dir: &Path, h2p: &str) -> Result<Option<PathBuf>> {
    let mut matches = Vec::new();
    for f in knxprod_files(dir)? {
        if provides(&f, h2p)? {
            matches.push(f);
        }
    }
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.into_iter().next()),
        n => bail!(
            "{h2p} is provided by {n} knxprod files in {}; \
             disambiguate with --knxprod <file>",
            dir.display()
        ),
    }
}

/// Whether a knxprod file's catalog contains the given Hardware2ProgramRefId.
fn provides(path: &Path, h2p: &str) -> Result<bool> {
    let kp = knx_prod::KnxProd::open(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(kp
        .products
        .iter()
        .any(|p| p.hardware2program_ref_id.as_deref() == Some(h2p)))
}
