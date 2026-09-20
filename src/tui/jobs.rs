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

//! Work that takes long enough to notice, moved off the drawing thread.
//!
//! Scanning means hashing 120 MB, parsing fifty thousand symbols and matching
//! thirty-seven signatures — about a second, and twice that when there is a
//! backup to resolve against as well. Patching adds a full read and write of
//! the same file. Doing either between two frames freezes the keyboard, and a
//! terminal that has stopped responding is indistinguishable from one that has
//! crashed. So each of these runs on its own thread and reports back down a
//! channel; the event loop keeps drawing, and the spinner is honest about the
//! fact that something is happening.

use crate::apply::{self, Note};
use crate::backup;
use crate::discovery::Install;
use crate::manage;
use crate::patch::Config;
use crate::status::{self, Inspection, Level};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;

pub enum Request {
    /// Read a module and work out what it is currently doing.
    Scan { install: Option<Install>, node: PathBuf },
    Patch { install: Install, node: PathBuf, cfg: Config, allow_partial: bool },
    Restore { install: Install, node: PathBuf },
    DeleteBackup { install: Install },
    /// Delete the module *and* clear the updater's record of it, which is the
    /// only combination that makes Discord fetch a fresh one.
    Reinstall { install: Install, node: Option<PathBuf> },
}

impl Request {
    /// What to put next to the spinner. Present tense: it is happening now.
    pub fn verb(&self) -> &'static str {
        match self {
            Request::Scan { .. } => "Reading the module",
            Request::Patch { .. } => "Patching",
            Request::Restore { .. } => "Restoring the original",
            Request::DeleteBackup { .. } => "Deleting the backup",
            Request::Reinstall { .. } => "Clearing the module",
        }
    }
}

pub struct LogLine {
    pub text: String,
    pub level: Level,
}

/// What a finished job leaves on screen.
pub struct Log {
    pub title: String,
    pub lines: Vec<LogLine>,
    pub ok: bool,
    /// Whether the module on disk changed, so the readout needs taking again.
    pub changed: bool,
}

pub enum Reply {
    Scanned(Box<Inspection>),
    Finished(Box<Log>),
}

/// Start `request` on its own thread. The receiver yields exactly one `Reply`.
pub fn spawn(request: Request) -> Receiver<Reply> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let reply = run(request);
        // A send that fails means the UI moved on — the user cancelled, or
        // quit. Nothing to report to, and nothing to clean up.
        let _ = tx.send(reply);
    });
    rx
}

fn run(request: Request) -> Reply {
    match request {
        Request::Scan { install, node } => match install {
            Some(i) => match status::inspect_install(&i, &node) {
                Ok(f) => Reply::Scanned(Box::new(f)),
                Err(e) => Reply::Finished(Box::new(failure("Could not read the module", e))),
            },
            None => match status::inspect_path(&node) {
                Ok(f) => Reply::Scanned(Box::new(f)),
                Err(e) => Reply::Finished(Box::new(failure("Could not read the module", e))),
            },
        },
        Request::Patch { install, node, cfg, allow_partial } => {
            Reply::Finished(Box::new(do_patch(install, node, cfg, allow_partial)))
        }
        Request::Restore { install, node } => {
            let mut log = Log {
                title: format!("Restored {}", install.label()),
                lines: Vec::new(),
                ok: true,
                changed: true,
            };
            match backup::restore(&install, &node) {
                Ok(src) => {
                    log.lines.push(line(Level::Good, format!("put back from {}", src.display())));
                    log.lines.push(line(
                        Level::Neutral,
                        "The module is exactly as Discord shipped it. Start Discord and it \
                         will behave normally again."
                            .to_string(),
                    ));
                    Reply::Finished(Box::new(log))
                }
                Err(e) => Reply::Finished(Box::new(failure("Could not restore", e.to_string()))),
            }
        }
        Request::DeleteBackup { install } => match backup::remove(&install) {
            Ok(path) => Reply::Finished(Box::new(Log {
                title: "Backup deleted".to_string(),
                lines: vec![
                    line(Level::Good, format!("removed {}", path.display())),
                    line(
                        Level::Warn,
                        "This install can no longer be restored. If its module is patched, \
                         the way back is to delete the module and let Discord download a \
                         fresh one."
                            .to_string(),
                    ),
                ],
                ok: true,
                changed: false,
            })),
            Err(e) => Reply::Finished(Box::new(failure("Could not delete the backup", e.to_string()))),
        },
        Request::Reinstall { install, node } => {
            match manage::force_reinstall(&install, node.as_deref()) {
                Ok(cleared) => {
                    let mut lines = Vec::new();
                    match &cleared.module_dir {
                        Some(dir) => {
                            lines.push(line(Level::Good, format!("removed {}", dir.display())))
                        }
                        None => lines.push(line(
                            Level::Neutral,
                            "the module was already gone".to_string(),
                        )),
                    }
                    match &cleared.registry {
                        Some((db, kept)) => {
                            lines.push(line(
                                Level::Good,
                                format!("cleared the updater's record at {}", db.display()),
                            ));
                            lines.push(line(
                                Level::Neutral,
                                format!("a copy of it is at {}", kept.display()),
                            ));
                        }
                        None => lines.push(line(
                            Level::Neutral,
                            "there was no updater record to clear".to_string(),
                        )),
                    }
                    lines.push(line(
                        Level::Neutral,
                        "Start Discord. It downloads its modules again on the next launch, \
                         which takes a moment — the voice module is the large one. Your \
                         logins, servers and settings are untouched; none of them were in \
                         the file that was cleared."
                            .to_string(),
                    ));
                    Reply::Finished(Box::new(Log {
                        title: "Discord will fetch a fresh module".to_string(),
                        lines,
                        ok: true,
                        changed: true,
                    }))
                }
                Err(e) => Reply::Finished(Box::new(failure("Could not clear the module", e))),
            }
        }
    }
}

fn do_patch(install: Install, node: PathBuf, cfg: Config, allow_partial: bool) -> Log {
    let mut lines: Vec<LogLine> = Vec::new();
    // Each `apply` call gets its own closure. One closure held across all of
    // them would keep `lines` mutably borrowed for the whole function, and
    // this function also needs to add lines of its own in between.
    let prepared = {
        let mut sink = |n: Note| lines.push(note_line(n));
        match apply::prepare(&install, &node, false, &mut sink) {
            Ok(p) => p,
            Err(e) => return failure_with("Patching failed", e, lines),
        }
    };
    {
        let mut sink = |n: Note| lines.push(note_line(n));
        apply::report_gaps(&prepared, &mut sink);
    }

    if prepared.blocked() && !allow_partial {
        lines.push(line(
            Level::Bad,
            "Nothing was written. Stereo would have been negotiated and then sent as one \
             channel, which is worse than not patching at all."
                .to_string(),
        ));
        return Log {
            title: "This build cannot be patched safely".to_string(),
            lines,
            ok: false,
            changed: false,
        };
    }

    let plan = match apply::plan(&prepared, &cfg) {
        Ok(p) => p,
        Err(e) => return failure_with("Patching failed", e, lines),
    };
    if plan.edits.is_empty() {
        lines.push(line(Level::Warn, "nothing selected to change".to_string()));
        return Log { title: "Nothing to do".to_string(), lines, ok: false, changed: false };
    }
    let effects = apply::effects(&plan, &prepared.report, &cfg);
    lines.push(line(
        Level::Neutral,
        format!("{} edits, {} bytes", plan.edits.len(), plan.total_bytes()),
    ));
    lines.push(line(Level::Good, effects.join(", ")));

    let committed = {
        let mut sink = |n: Note| lines.push(note_line(n));
        apply::commit(&install, &node, prepared, &plan, &mut sink)
    };
    match committed {
        Ok(_) => {
            lines.push(line(
                Level::Neutral,
                "Start Discord and re-join a voice channel. Set your input to a stereo \
                 source — a mono microphone still gives two identical channels, which \
                 analysers report as mono."
                    .to_string(),
            ));
            Log { title: format!("Patched {}", install.label()), lines, ok: true, changed: true }
        }
        Err(e) => failure_with("Patching failed", e, lines),
    }
}

fn note_line(n: Note) -> LogLine {
    match n {
        Note::Info(t) => line(Level::Neutral, t),
        Note::Good(t) => line(Level::Good, t),
        Note::Warn(t) => line(Level::Warn, t),
        Note::Bad(t) => line(Level::Bad, t),
    }
}

fn line(level: Level, text: String) -> LogLine {
    LogLine { text, level }
}

fn failure(title: &str, err: String) -> Log {
    failure_with(title, err, Vec::new())
}

fn failure_with(title: &str, err: String, mut lines: Vec<LogLine>) -> Log {
    for part in err.lines() {
        lines.push(line(Level::Bad, part.to_string()));
    }
    Log { title: title.to_string(), lines, ok: false, changed: false }
}
