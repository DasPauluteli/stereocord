// ----------------------------------------------------------------------------
// stereocord - Copyright (c) 2026 Paul Neri
// <67437654+DasPauluteli@users.noreply.github.com>
//
// Licensed under CC BY-NC-SA 4.0: non-commercial use, share alike, keep this
// notice, no patent grant. See LICENSE, or
// https://creativecommons.org/licenses/by-nc-sa/4.0/
//
// SPDX-License-Identifier: CC-BY-NC-SA-4.0
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
    if let Some(key) = &install.key {
        return key.clone();
    }
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

/// Forget the backup for one install.
///
/// Only ever the tool's own copy under `XDG_STATE_HOME`; the module itself is
/// never touched here. Deleting a backup makes that install unrestorable, so
/// every caller asks first.
pub fn remove(install: &Install) -> io::Result<PathBuf> {
    let path = path_for(install);
    fs::remove_file(&path)?;
    Ok(path)
}

/// Keep a copy of Discord's updater database before it is removed.
///
/// It holds no user data — an install id, and the versions and file manifests
/// of what is installed — but it is Discord's, not ours, and deleting someone
/// else's file without keeping a copy is not a thing to do on a hunch. Kept
/// under this tool's own directory rather than beside the original, so that
/// Discord never sees a file it did not put there.
pub fn keep_registry(install: &Install, db: &Path) -> io::Result<PathBuf> {
    let dest = dir().join(format!("{}-installer.db", slug(install)));
    fs::create_dir_all(dir())?;
    let tmp = dest.with_extension("db.part");
    fs::copy(db, &tmp)?;
    fs::rename(&tmp, &dest)?;
    Ok(dest)
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
