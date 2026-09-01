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
}

impl Install {
    pub fn label(&self) -> String {
        format!("{} {}", self.channel, self.version)
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
    fn parse(s: &str) -> Version {
        Version(s.split('.').filter_map(|p| p.parse().ok()).collect())
    }
}

const CHANNELS: &[(&str, &str)] = &[
    ("discord", "Discord"),
    ("discordptb", "Discord PTB"),
    ("discordcanary", "Discord Canary"),
    ("discorddevelopment", "Discord Development"),
];

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
