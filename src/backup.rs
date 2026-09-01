// ----------------------------------------------------------------------------
// "THE BIERWARE LICENSE" (Revision 1):
// <67437654+DasPauluteli@users.noreply.github.com> wrote this file. As long as
// you retain this notice you can do whatever you want with this stuff. If we
// meet some day you have to buy me a beer in return - Paul Neri
// ----------------------------------------------------------------------------

//! Backups of the untouched module, kept per install.
//!
//! The backup is what makes the whole thing reversible, so it is taken before
//! the first write and never overwritten by a later, already-patched copy —
//! backing up a patched file is how a "restore" quietly stops restoring
//! anything.

use crate::discovery::Install;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub fn dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("stereocord/backups")
}

fn slug(install: &Install) -> String {
    format!(
        "{}-{}",
        install.channel.replace(' ', "_").to_lowercase(),
        install.version
    )
}

pub fn path_for(install: &Install) -> PathBuf {
    dir().join(format!("{}.node", slug(install)))
}

pub fn exists(install: &Install) -> bool {
    path_for(install).is_file()
}

/// Copy the current module aside, unless a backup is already on record.
///
/// Returns the backup path and whether it was created by this call.
pub fn ensure(install: &Install, node: &Path) -> io::Result<(PathBuf, bool)> {
    let dest = path_for(install);
    if dest.is_file() {
        return Ok((dest, false));
    }
    fs::create_dir_all(dir())?;
    let tmp = dest.with_extension("node.part");
    fs::copy(node, &tmp)?;
    fs::rename(&tmp, &dest)?;
    Ok((dest, true))
}

pub fn restore(install: &Install, node: &Path) -> io::Result<PathBuf> {
    let src = path_for(install);
    if !src.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no backup on record at {}", src.display()),
        ));
    }
    let tmp = node.with_extension("node.part");
    fs::copy(&src, &tmp)?;
    fs::rename(&tmp, node)?;
    Ok(src)
}

pub struct Entry {
    pub path: PathBuf,
    pub size: u64,
}

pub fn list() -> Vec<Entry> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir()) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e == "node").unwrap_or(false) {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            out.push(Entry { path, size });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}
