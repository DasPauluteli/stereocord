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

//! Making Discord fetch a fresh voice module.
//!
//! This is the last resort for when restoring is not possible: the module was
//! patched by something that kept no backup, or a delta update is wedged
//! because Discord will not patch over a file whose hash it does not
//! recognise.
//!
//! Deleting the module directory is *not* enough, and getting that wrong is
//! worth writing down. Discord's updater keeps its record of what is installed
//! in `installer.db`, next to the `app-<version>` directories, and it trusts
//! that record without checking the files are still there. Delete only the
//! directory and the next launch logs
//!
//! ```text
//! Updater received command 12: InstallModule { name: "discord_voice" }
//! Install of module discord_voice finished successfully.
//! ```
//!
//! having downloaded nothing — and then the client reports itself corrupt,
//! because the module it was told exists cannot be loaded. Voice stays broken,
//! and restarting does not help, because every restart reaches the same
//! conclusion.
//!
//! So the record has to go with it. That database holds an install id and the
//! versions and file manifests of what is installed, and nothing else; logins,
//! servers, settings and cache all live in sibling files that are not touched.
//! Removing it makes the updater treat every module as not installed and fetch
//! them again on the next start.
//!
//! Launching still works with the database gone. The launcher script runs
//! `$XDG_CONFIG_HOME/<channel>/<Exe>`, a symlink into the `app-<version>`
//! directory, and only falls back to the bootstrap downloader when *that* is
//! missing; it never consults the database to decide what to start.

use crate::backup;
use crate::discovery::{self, Install};
use std::fs;
use std::path::{Path, PathBuf};

/// What a reinstall removed, so the caller can say so afterwards.
pub struct Cleared {
    /// The module directory, if there was still one.
    pub module_dir: Option<PathBuf>,
    /// The updater's database, and the copy taken before it was removed.
    pub registry: Option<(PathBuf, PathBuf)>,
}

/// Delete the voice module and make Discord's updater forget it was installed.
///
/// `node` is optional. An install whose module has already been deleted — by an
/// earlier version of this tool, or by hand — still has the stale record, and
/// clearing it is the only way back.
pub fn force_reinstall(install: &Install, node: Option<&Path>) -> Result<Cleared, String> {
    let module_dir = match node {
        Some(node) => Some(delete_module(node)?),
        None => None,
    };

    let registry = match discovery::installer_db(&install.app_dir) {
        Some(db) if db.is_file() => {
            let kept = backup::keep_registry(install, &db)
                .map_err(|e| format!("copying {} aside: {e}", db.display()))?;
            fs::remove_file(&db).map_err(|e| format!("removing {}: {e}", db.display()))?;
            Some((db, kept))
        }
        // No database at all is the state a fresh install starts in, so there
        // is nothing to clear and nothing has gone wrong.
        _ => None,
    };

    if module_dir.is_none() && registry.is_none() {
        return Err(
            "nothing to do here: there is no voice module and no updater database to \
             clear. Start Discord and let it finish installing."
                .to_string(),
        );
    }
    Ok(Cleared { module_dir, registry })
}

/// Remove the `discord_voice-<n>` directory.
///
/// The whole directory rather than just the `.node`: the directory is the unit
/// the module updater installs, and one left behind with a file missing is
/// exactly the half-state this module exists to avoid.
fn delete_module(node: &Path) -> Result<PathBuf, String> {
    let dir = discovery::module_dir(node).ok_or_else(|| {
        format!(
            "{} is not inside a discord_voice-<n> module directory; refusing to \
             delete anything",
            node.display()
        )
    })?;
    // `module_dir` has already vouched for the directory's name. This is the
    // second half of the same question: that it sits under a `modules/`
    // directory, so that a `discord_voice-1` folder somewhere else entirely
    // cannot be removed by passing its path in.
    let under_modules = dir
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n == "modules")
        .unwrap_or(false);
    if !under_modules {
        return Err(format!(
            "{} is not under a modules/ directory; refusing to delete it",
            dir.display()
        ));
    }
    fs::remove_dir_all(&dir).map_err(|e| format!("removing {}: {e}", dir.display()))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_outside_a_module_directory_is_refused() {
        let err = delete_module(Path::new("/tmp/somewhere/discord_voice.node"))
            .expect_err("should refuse");
        assert!(err.contains("refusing"), "{err}");
    }

    #[test]
    fn a_module_directory_not_under_modules_is_refused() {
        let err = delete_module(Path::new(
            "/tmp/elsewhere/discord_voice-1/discord_voice/discord_voice.node",
        ))
        .expect_err("should refuse");
        assert!(err.contains("modules/"), "{err}");
    }
}
