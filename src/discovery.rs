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

//! Finding the Discord installations on this machine.
//!
//! Discord keeps each app version in its own `app-<version>` directory and
//! downloads native modules into it separately. That detail matters: a client
//! that has just staged an update has a *new* `app-` directory whose voice
//! module is still empty or still stock, so a patch applied to the previous
//! directory stops taking effect the next time Discord restarts, with no error
//! anywhere. Every install is reported, newest first, along with whether it
//! looks like the one Discord will actually run.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Install {
    /// "Discord", "Discord PTB", ...
    pub channel: String,
    /// e.g. "1.0.155"
    pub version: Version,
    pub app_dir: PathBuf,
    /// `None` when the module directory exists but Discord has not finished
    /// downloading the voice module into it yet.
    pub node: Option<PathBuf>,
    /// Overrides the name the backup is filed under. Set only for modules the
    /// user pointed at by path: those have no channel and no version to be
    /// named after, and two of them must not end up sharing one backup.
    pub key: Option<String>,
}

impl Install {
    pub fn label(&self) -> String {
        if self.version.0.is_empty() {
            self.channel.clone()
        } else {
            format!("{} {}", self.channel, self.version)
        }
    }

    /// Stand in for an install when the user has named a module by path.
    ///
    /// Custom clients ship Discord's own voice module, so patching one is the
    /// same operation — but the path is all there is to identify it by, so the
    /// backup is filed under a digest of that path rather than under a channel
    /// and version that would collide with every other such module.
    pub fn for_path(node: &Path) -> Install {
        let canonical = node.canonicalize().unwrap_or_else(|_| node.to_path_buf());
        let digest = crate::md5::hex(canonical.to_string_lossy().as_bytes());
        // An `app-<version>` component means this is a Discord tree after all,
        // just one reached by path; carrying the version through makes the
        // readout say something useful instead of nothing.
        let version = canonical
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .find_map(|c| c.strip_prefix("app-"))
            .map(Version::parse)
            .unwrap_or(Version(Vec::new()));
        Install {
            channel: "Custom module".to_string(),
            version,
            app_dir: canonical
                .ancestors()
                .find(|a| a.join("modules").is_dir())
                .unwrap_or_else(|| canonical.parent().unwrap_or(&canonical))
                .to_path_buf(),
            node: Some(canonical),
            key: Some(format!("custom-{}", &digest[..12])),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub Vec<u32>);

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let parts: Vec<String> = self.0.iter().map(|n| n.to_string()).collect();
        write!(f, "{}", parts.join("."))
    }
}

impl Version {
    pub fn parse(s: &str) -> Version {
        Version(s.split('.').filter_map(|p| p.parse().ok()).collect())
    }
}

const CHANNELS: &[(&str, &str)] = &[
    ("discord", "Discord"),
    ("discordptb", "Discord PTB"),
    ("discordcanary", "Discord Canary"),
    ("discorddevelopment", "Discord Development"),
];

/// The display names of every channel this tool knows about, in the order they
/// should be offered. Channels with nothing installed are still worth showing —
/// "Canary: not installed" answers the question, where a list that silently
/// omits it does not.
pub fn known_channels() -> Vec<&'static str> {
    CHANNELS.iter().map(|(_, name)| *name).collect()
}

fn config_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        roots.push(PathBuf::from(xdg));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        roots.push(home.join(".config"));
        // Flatpak keeps a private config tree per application id.
        for (dir, _) in CHANNELS {
            let id = match *dir {
                "discord" => "com.discordapp.Discord",
                "discordptb" => "com.discordapp.DiscordPTB",
                "discordcanary" => "com.discordapp.DiscordCanary",
                _ => continue,
            };
            roots.push(home.join(".var/app").join(id).join("config"));
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

/// Every install found, newest version first within each channel.
pub fn find_installs() -> Vec<Install> {
    let mut out = Vec::new();
    for root in config_roots() {
        for (dir, channel) in CHANNELS {
            let base = root.join(dir);
            if !base.is_dir() {
                continue;
            }
            let entries = match fs::read_dir(&base) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let Some(ver) = name.strip_prefix("app-") else { continue };
                let app_dir = entry.path();
                if !app_dir.is_dir() {
                    continue;
                }
                out.push(Install {
                    channel: channel.to_string(),
                    version: Version::parse(ver),
                    node: voice_node_in(&app_dir),
                    app_dir,
                    key: None,
                });
            }
        }
    }
    out.sort_by(|a, b| {
        a.channel
            .cmp(&b.channel)
            .then(b.version.cmp(&a.version))
    });
    out
}

/// `modules/discord_voice-<n>/discord_voice/discord_voice.node`, whichever
/// module revision is present.
fn voice_node_in(app_dir: &Path) -> Option<PathBuf> {
    let modules = app_dir.join("modules");
    let entries = fs::read_dir(&modules).ok()?;
    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("discord_voice-") {
            continue;
        }
        let node = entry.path().join("discord_voice").join("discord_voice.node");
        if node.is_file() {
            candidates.push(node);
        }
    }
    candidates.sort();
    candidates.pop()
}

/// The `modules/discord_voice-<n>` directory a voice module lives in.
///
/// That whole directory is the unit Discord's module updater installs and
/// replaces, so it is also the unit to delete when the point is to make it
/// fetch a fresh one.
pub fn module_dir(node: &Path) -> Option<PathBuf> {
    // .../modules/discord_voice-1/discord_voice/discord_voice.node
    let dir = node.parent()?.parent()?;
    let name = dir.file_name()?.to_string_lossy().to_string();
    // Refuse anything that is not shaped like the directory we expect, so a
    // surprising layout deletes nothing rather than deleting the wrong thing.
    if !name.starts_with("discord_voice-") {
        return None;
    }
    Some(dir.to_path_buf())
}

/// Discord's updater database for the install in `app_dir`.
///
/// It sits beside the `app-<version>` directories, one per channel, and records
/// which host version and which modules are installed. Nothing else in this
/// tool reads it; [`crate::manage`] removes it to make the updater fetch a
/// fresh module, which is the only thing that actually works.
///
/// `None` when `app_dir` is not inside a channel directory this tool knows,
/// so that a path from somewhere unexpected cannot nominate a file for
/// deletion.
pub fn installer_db(app_dir: &Path) -> Option<PathBuf> {
    let root = app_dir.parent()?;
    let name = root.file_name()?.to_string_lossy().to_string();
    if !CHANNELS.iter().any(|(dir, _)| *dir == name) {
        return None;
    }
    Some(root.join("installer.db"))
}

/// True when this install has modules but no voice module.
///
/// The difference matters, and neither the file system nor Discord will state
/// it. An install that has never been launched has an empty `modules/` and
/// simply needs starting; one whose `modules/` is full of everything *except*
/// the voice module had it removed, and Discord will not fetch it back on its
/// own — the updater's record still says it is installed. Telling the second
/// case to "launch Discord once" is advice that cannot work.
pub fn voice_module_removed(app_dir: &Path) -> bool {
    if voice_node_in(app_dir).is_some() {
        return false;
    }
    let Ok(entries) = fs::read_dir(app_dir.join("modules")) else { return false };
    entries.flatten().any(|e| {
        let name = e.file_name();
        let name = name.to_string_lossy();
        name.starts_with("discord_") && !name.starts_with("discord_voice-")
    })
}

/// PIDs of running Discord processes launched from `app_dir`.
///
/// A patch written while the client is running does not take effect: the old
/// image is already mapped, and the file the process reopens on restart may
/// well be a different one anyway.
pub fn running_pids(app_dir: &Path) -> Vec<u32> {
    let needle = app_dir.to_string_lossy().to_string();
    let mut pids = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else { return pids };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Ok(pid) = name.to_string_lossy().parse::<u32>() else { continue };
        let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else { continue };
        let cmdline = String::from_utf8_lossy(&cmdline);
        // Only the executable itself, not helper processes that merely mention
        // the path in a flag, would be enough — but any process holding the
        // module mapped is a reason to stop, so match the whole command line.
        if cmdline.contains(&needle) {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    pids
}
