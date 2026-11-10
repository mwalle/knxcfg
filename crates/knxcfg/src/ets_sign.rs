//! Project signatures made by ETS's own signing assembly
//! (`Knx.Ets.XmlSigning.dll`), as the alternative to signing with the key.
//!
//! A small helper (`ets_signer/EtsSign.cs`) loads the assembly and calls its
//! internal `XmlSigning.SignDirectory` on a directory written to a temporary
//! folder; the export reads the signature file back. The helper is compiled
//! once into the cache directory:
//!
//! - Windows: with the `csc.exe` of .NET Framework 4, run by .NET Framework.
//! - elsewhere: with Mono's `mcs`, run by `mono`; the assembly calls
//!   `shlwapi.dll!PathRelativePathTo`, which `ets_signer/shlwapi.c` provides as
//!   `libshlwapi.so` (built with `cc`). Mono 6.12 cannot run the obfuscated
//!   ETS 5.6/5.7 assemblies, so this only helps with an assembly Mono accepts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use knx_proj::export::DirectorySigner;

const HELPER_SOURCE: &str = include_str!("ets_signer/EtsSign.cs");
const SHIM_SOURCE: &str = include_str!("ets_signer/shlwapi.c");

pub struct DllSigner {
    dll: PathBuf,
    helper: PathBuf,
    cache: PathBuf,
}

impl DllSigner {
    /// Prepare signing with `dll`, building the helper if needed.
    pub fn new(dll: &Path) -> Result<Self> {
        if !dll.is_file() {
            bail!("{} does not exist", dll.display());
        }
        let cache = cache_dir()?;
        std::fs::create_dir_all(&cache).with_context(|| format!("creating {}", cache.display()))?;
        let helper = build_helper(&cache)?;
        Ok(DllSigner { dll: dll.canonicalize()?, helper, cache })
    }

    fn run(&self, dir: &Path) -> Result<std::process::Output> {
        let mut cmd = if cfg!(windows) {
            Command::new(&self.helper)
        } else {
            let mut c = Command::new("mono");
            c.arg(&self.helper);
            let libs = match std::env::var_os("LD_LIBRARY_PATH") {
                Some(old) => {
                    let mut v = self.cache.clone().into_os_string();
                    v.push(":");
                    v.push(old);
                    v
                }
                None => self.cache.clone().into_os_string(),
            };
            c.env("LD_LIBRARY_PATH", libs);
            c
        };
        cmd.arg(&self.dll).arg(dir);
        cmd.output().with_context(|| format!("running {}", self.helper.display()))
    }
}

impl DirectorySigner for DllSigner {
    fn sign(&self, files: &BTreeMap<String, Vec<u8>>, dir: &str) -> Result<String> {
        let tmp = std::env::temp_dir().join(format!("knxcfg-sign-{}-{dir}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let result = (|| {
            let prefix = format!("{dir}/");
            for (name, data) in files.iter().filter(|(n, _)| n.starts_with(&prefix)) {
                let path = tmp.join(name);
                std::fs::create_dir_all(path.parent().expect("below tmp"))?;
                std::fs::write(&path, data)?;
            }
            let project = tmp.join(dir);
            let out = self.run(&project)?;
            if !out.status.success() {
                // Mono reports a crash on both streams, among a stack dump.
                let text = format!("{}{}", String::from_utf8_lossy(&out.stderr), String::from_utf8_lossy(&out.stdout));
                let lines = || text.lines().map(str::trim).filter(|l| !l.is_empty());
                let first = lines()
                    .find(|l| l.contains("Assertion") || l.contains("Exception"))
                    .or_else(|| lines().next())
                    .unwrap_or("");
                if !cfg!(windows) && (text.contains("heap_strings") || text.contains("Invalid IL")) {
                    bail!(
                        "Mono cannot run {} ({first}); sign with --signing-key, or export on Windows",
                        self.dll.display()
                    );
                }
                bail!("{} failed: {first}", self.helper.display());
            }
            // The assembly writes `<dir>.signature` next to the directory.
            let name = format!("{dir}.signature");
            let path = [tmp.join(&name), project.join(&name)]
                .into_iter()
                .find(|p| p.is_file())
                .ok_or_else(|| anyhow!("the signing assembly wrote no {name}"))?;
            let text = std::fs::read_to_string(&path)?;
            Ok(text.trim_start_matches('\u{feff}').trim().to_string())
        })();
        let _ = std::fs::remove_dir_all(&tmp);
        result
    }
}

fn cache_dir() -> Result<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    };
    Ok(base.ok_or_else(|| anyhow!("no cache directory"))?.join("knxcfg").join("ets-signer"))
}

/// Compile the helper (and on Linux the shim) unless the cache holds a build
/// of the current sources.
fn build_helper(cache: &Path) -> Result<PathBuf> {
    let source = cache.join("EtsSign.cs");
    let exe = cache.join("EtsSign.exe");
    let shim_source = cache.join("shlwapi.c");
    let shim = cache.join("libshlwapi.so");
    let current = |path: &Path, text: &str| std::fs::read_to_string(path).is_ok_and(|t| t == text);

    if !(current(&source, HELPER_SOURCE) && exe.is_file()) {
        std::fs::write(&source, HELPER_SOURCE)?;
        let status = if cfg!(windows) {
            let csc = windows_csc().ok_or_else(|| anyhow!("csc.exe of .NET Framework 4 not found"))?;
            Command::new(&csc).arg("/nologo").arg(format!("/out:{}", exe.display())).arg(&source).status()
        } else {
            Command::new("mcs").arg(format!("-out:{}", exe.display())).arg(&source).status()
        };
        let status = status.context("compiling the signing helper (Debian: apt install mono-mcs)")?;
        if !status.success() {
            bail!("compiling {} failed", source.display());
        }
    }
    if !cfg!(windows) && !(current(&shim_source, SHIM_SOURCE) && shim.is_file()) {
        std::fs::write(&shim_source, SHIM_SOURCE)?;
        let status = Command::new("cc")
            .args(["-shared", "-fPIC", "-O2", "-o"])
            .arg(&shim)
            .arg(&shim_source)
            .status()
            .context("compiling the shlwapi shim with cc")?;
        if !status.success() {
            bail!("compiling {} failed", shim_source.display());
        }
    }
    Ok(exe)
}

fn windows_csc() -> Option<PathBuf> {
    let windir = PathBuf::from(std::env::var_os("WINDIR")?);
    ["Framework64", "Framework"]
        .iter()
        .map(|f| windir.join("Microsoft.NET").join(f).join("v4.0.30319").join("csc.exe"))
        .find(|p| p.is_file())
}
