use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::model::LockPackage;

pub fn store_pkg_dir(id: &str) -> Result<PathBuf> {
    Ok(crate::util::store_dir()?.join(crate::util::pkg_dir_name(id)))
}

/// make sure the package is extracted in the global store; download + verify if missing.
/// returns (store path, bytes downloaded, 0 when already cached).
pub async fn ensure_in_store(reg: &crate::http::Registry, id: &str, pkg: &LockPackage) -> Result<(PathBuf, u64)> {
    let dir = store_pkg_dir(id)?;
    if dir.join(".zap-ok").exists() {
        return Ok((dir, 0));
    }
    let data = reg
        .tarball(&pkg.tarball)
        .await
        .with_context(|| format!("downloading {}", pkg.tarball))?;
    if !pkg.integrity.is_empty() && !crate::util::verify_integrity(&data, &pkg.integrity) {
        bail!("integrity verification failed for {}", id);
    }
    std::fs::create_dir_all(&dir)?;
    if let Err(e) = extract(&data, &dir) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e).with_context(|| format!("extracting {}", id));
    }
    Ok((dir, data.len() as u64))
}

/// extract a gzipped npm tarball (strip the "package/" root) into dest
pub fn extract(data: &[u8], dest: &Path) -> Result<()> {
    let gz = flate2::read::GzDecoder::new(data);
    let mut archive = tar::Archive::new(gz);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let raw = entry.path()?.to_path_buf();
        let comps: Vec<String> = raw
            .components()
            .filter_map(|c| {
                let s = c.as_os_str().to_string_lossy().to_string();
                if s.is_empty() || s == "." || s == "/" || s == "\\" {
                    None
                } else {
                    Some(s)
                }
            })
            .collect();
        if comps.iter().any(|c| c == "..") {
            continue; // tar traversal guard
        }
        let stripped: Vec<String> = if comps.first().map(|c| c.as_str()) == Some("package") {
            comps[1..].to_vec()
        } else {
            comps.clone()
        };
        if stripped.is_empty() {
            continue;
        }
        let out = dest.join(stripped.join("/"));
        let t = entry.header().entry_type();
        if t.is_dir() {
            std::fs::create_dir_all(&out)?;
        } else if t.is_symlink() || t.is_hard_link() {
            continue; // npm tarballs rarely ship symlinks; skip rather than crash
        } else {
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut f = std::fs::File::create(&out)?;
            std::io::copy(&mut entry, &mut f)?;
        }
    }
    std::fs::write(dest.join(".zap-ok"), "ok")?;
    Ok(())
}

/// hardlink every file from the store into the project's virtual store (copy as fallback)
pub fn link_tree(src: &Path, dst: &Path) -> Result<u32> {
    let mut count = 0u32;
    link_rec(src, dst, &mut count)?;
    Ok(count)
}

fn link_rec(src: &Path, dst: &Path, count: &mut u32) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == std::ffi::OsStr::new(".zap-ok") {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        if from.is_dir() {
            link_rec(&from, &to, count)?;
        } else {
            if !to.exists() {
                if std::fs::hard_link(&from, &to).is_err() {
                    std::fs::copy(&from, &to).with_context(|| format!("copying {}", from.display()))?;
                }
            }
            *count += 1;
        }
    }
    Ok(())
}
