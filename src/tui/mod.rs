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

//! The terminal interface: what `stereocord` does when you just run it.
//!
//! Three screens deep, and the order is the order the questions actually come
//! in. Which client? — then what is this module doing to my audio right now,
//! which is the thing nobody could see before. Only then the choices: patch it,
//! or manage the copies on disk.
//!
//! Nothing here decides anything on its own. Every write goes through
//! [`crate::apply`], the same path the command line takes, and every
//! destructive step stops at a dialog that names the file it is about to
//! touch. The interface's job is to make the state of the module legible
//! enough that the choice is an informed one.

mod jobs;
mod render;
mod theme;

use crate::discovery::{self, Install};
use crate::patch::Config;
use crate::sites;
use crate::status::Inspection;
use jobs::{Log, Reply, Request};
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::crossterm::{execute, ExecutableCommand};
use ratatui::Terminal;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

/// One row of the opening list.
pub enum Kind {
    /// An install with a voice module ready to work on.
    Ready { install: Install, node: PathBuf },
    /// An install with no voice module. `removed` distinguishes the two ways
    /// that happens: `false` means Discord has simply not downloaded it yet and
    /// launching once is the answer, `true` means it was deleted and Discord
    /// will not fetch it back on its own — which is a state only this tool can
    /// get it out of.
    NoModule { install: Install, removed: bool },
    /// A channel this tool knows about that is not installed here.
    NotInstalled,
    /// A module the user named by path.
    Loose { install: Install, node: PathBuf },
    /// The row that asks for such a path.
    AskForPath,
}

pub struct Target {
    pub label: String,
    pub detail: String,
    pub kind: Kind,
}

impl Target {
    /// Whether the cursor may land on this row at all.
    ///
    /// An install whose module was deleted is selectable even though there is
    /// nothing to read there: the way out of that state is on its manage
    /// screen, and a row you cannot reach is a dead end.
    fn selectable(&self) -> bool {
        match &self.kind {
            Kind::Ready { .. } | Kind::Loose { .. } | Kind::AskForPath => true,
            Kind::NoModule { removed, .. } => *removed,
            Kind::NotInstalled => false,
        }
    }

    fn install(&self) -> Option<&Install> {
        match &self.kind {
            Kind::Ready { install, .. } | Kind::Loose { install, .. } => Some(install),
            Kind::NoModule { install, .. } => Some(install),
            _ => None,
        }
    }

    /// A real Discord install, as opposed to a module named by path. Only these
    /// have an updater behind them to be told anything.
    fn is_install(&self) -> bool {
        matches!(self.kind, Kind::Ready { .. } | Kind::NoModule { .. })
    }

    fn node(&self) -> Option<&Path> {
        match &self.kind {
            Kind::Ready { node, .. } | Kind::Loose { node, .. } => Some(node),
            _ => None,
        }
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Screen {
    Targets,
    AskPath,
    Overview,
    Patch,
    Manage,
    Result,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Focus {
    Status,
    Actions,
}

pub struct Modal {
    pub title: String,
    pub body: Vec<String>,
    pub confirm: String,
    pub danger: bool,
    /// 0 confirms, 1 cancels.
    pub cursor: usize,
    request: Option<Request>,
}

/// One entry of the manage screen, worked out fresh each time it is drawn so
/// that deleting a backup immediately greys out restoring from it.
pub struct ManageItem {
    pub label: String,
    pub hint: String,
    pub detail: &'static str,
    pub enabled: bool,
    pub danger: bool,
}

pub struct App {
    pub targets: Vec<Target>,
    pub target_cursor: usize,
    pub screen: Screen,
    pub focus: Focus,
    pub current: Option<usize>,
    pub inspection: Option<Inspection>,
    pub status_cursor: usize,
    pub action_cursor: usize,
    pub groups_on: Vec<bool>,
    pub group_cursor: usize,
    pub seeded_from_module: bool,
    pub bitrate: u32,
    pub gain: f32,
    pub manage_cursor: usize,
    pub input: String,
    pub input_error: Option<String>,
    pub log: Option<Log>,
    pub log_scroll: u16,
    pub modal: Option<Modal>,
    pub busy: Option<&'static str>,
    pub tick: usize,
    /// A module named by path, kept here rather than only in the list so that
    /// rebuilding the list after a write does not lose the row you are
    /// standing on.
    loose: Option<PathBuf>,
    job: Option<Receiver<Reply>>,
    quit: bool,
}

pub const ACTIONS: &[(&str, &str)] = &[
    ("Patch this module", "Choose what to change, then write it."),
    ("Manage copies on disk", "Restore the original, or start fresh."),
    ("Back to the list", "Pick a different Discord install."),
];

impl App {
    fn new(bitrate: u32, gain: f32) -> App {
        let targets = build_targets(None);
        let mut app = App {
            target_cursor: 0,
            targets,
            screen: Screen::Targets,
            focus: Focus::Actions,
            current: None,
            inspection: None,
            status_cursor: 0,
            action_cursor: 0,
            groups_on: sites::GROUPS.iter().map(|g| g.default_on).collect(),
            group_cursor: 0,
            seeded_from_module: false,
            bitrate,
            gain,
            manage_cursor: 0,
            input: String::new(),
            input_error: None,
            log: None,
            log_scroll: 0,
            modal: None,
            busy: None,
            tick: 0,
            loose: None,
            job: None,
            quit: false,
        };
        app.target_cursor = app.next_selectable(0, 1).unwrap_or(0);
        app
    }

    pub fn target(&self) -> Option<&Target> {
        self.current.and_then(|i| self.targets.get(i))
    }

    /// Whether an entry of [`ACTIONS`] can be chosen here. Patching needs a
    /// module to patch; an install whose module was deleted still has a manage
    /// screen, and that is the whole reason it is reachable.
    pub fn action_enabled(&self, index: usize) -> bool {
        match index {
            0 => self.target().and_then(|t| t.node()).is_some(),
            _ => true,
        }
    }

    fn next_selectable(&self, from: usize, step: isize) -> Option<usize> {
        let n = self.targets.len();
        if n == 0 {
            return None;
        }
        let mut i = from;
        for _ in 0..n {
            if self.targets[i].selectable() {
                return Some(i);
            }
            i = ((i as isize + step).rem_euclid(n as isize)) as usize;
        }
        None
    }

    fn move_target(&mut self, step: isize) {
        let n = self.targets.len() as isize;
        if n == 0 {
            return;
        }
        let mut i = self.target_cursor as isize;
        for _ in 0..n {
            i = (i + step).rem_euclid(n);
            if self.targets[i as usize].selectable() {
                self.target_cursor = i as usize;
                return;
            }
        }
    }

    /// Sites that decide mono versus stereo and could not be located.
    pub fn critical_gaps(&self) -> Vec<&'static str> {
        let Some(i) = &self.inspection else { return Vec::new() };
        i.report
            .missing()
            .into_iter()
            .filter(|n| i.report.excused(n).is_none())
            .filter(|n| sites::find(n).map(|s| s.critical).unwrap_or(true))
            .collect()
    }

    pub fn config(&self) -> Config {
        Config {
            bitrate_kbps: self.bitrate,
            gain: self.gain,
            groups: sites::GROUPS
                .iter()
                .zip(&self.groups_on)
                .filter(|(_, &on)| on)
                .map(|(g, _)| g.name.to_string())
                .collect(),
        }
    }

    pub fn manage_items(&self) -> Vec<ManageItem> {
        let install = self.target().and_then(|t| t.install());
        let has_backup = install.map(crate::backup::exists).unwrap_or(false);
        let backup_size = install
            .map(crate::backup::path_for)
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len());
        let is_install = self.target().map(|t| t.is_install()).unwrap_or(false);
        let has_module = self.target().and_then(|t| t.node()).is_some();
        vec![
            ManageItem {
                label: "Restore the original module".to_string(),
                hint: match (backup_size, has_module) {
                    // The backup is one file; the module directory around it
                    // holds a dozen more. With the directory gone there is
                    // nowhere to put it back that Discord would accept.
                    (_, false) => "no module to restore onto".to_string(),
                    (Some(n), _) => format!("backup, {}", theme::bytes(n)),
                    (None, _) => "no backup on record".to_string(),
                },
                detail:
                    "Copies the untouched module back over the patched one. Discord goes back \
                     to behaving exactly as it shipped. Do this before letting Discord update: \
                     it checks the module's hash before applying an update, and a patched file \
                     makes the update fail silently.",
                enabled: has_backup && has_module,
                danger: false,
            },
            ManageItem {
                label: "Delete the backup".to_string(),
                hint: match backup_size {
                    Some(n) => format!("frees {}", theme::bytes(n)),
                    None => "nothing to delete".to_string(),
                },
                detail:
                    "Removes this tool's copy of the original module. It frees the space, and \
                     it means this install can no longer be restored — the only way back would \
                     be to delete the module and let Discord download it again.",
                enabled: has_backup,
                danger: true,
            },
            ManageItem {
                label: "Reinstall the voice module".to_string(),
                hint: if !is_install {
                    "only for a real Discord install".to_string()
                } else if has_module {
                    "downloads a fresh one".to_string()
                } else {
                    "the way back from a missing module".to_string()
                },
                detail:
                    "Removes the voice module and clears Discord's record of having \
                     installed it, so that it downloads a current one the next time it \
                     starts. Both halves are needed: Discord's updater trusts that record \
                     without checking the files, so a module deleted on its own is never \
                     replaced and the client reports itself corrupt instead. Use this when \
                     a module was patched by something that kept no backup, or when an \
                     update is stuck. Your logins, servers and settings are not in that \
                     record and are not touched; a copy of it is kept.",
                enabled: is_install,
                danger: true,
            },
            ManageItem {
                label: "Back".to_string(),
                hint: String::new(),
                detail: "Return to the readout.",
                enabled: true,
                danger: false,
            },
        ]
    }
}

/// Build the opening list: every install found, every channel that is not
/// installed, a module named by path if there is one, and the way in for
/// naming another.
fn build_targets(loose: Option<&Path>) -> Vec<Target> {
    let installs = discovery::find_installs();
    let mut out = Vec::new();

    for channel in discovery::known_channels() {
        let mine: Vec<&Install> = installs.iter().filter(|i| i.channel == channel).collect();
        if mine.is_empty() {
            out.push(Target {
                label: channel.to_string(),
                detail: "not installed".to_string(),
                kind: Kind::NotInstalled,
            });
            continue;
        }
        for install in mine {
            match &install.node {
                Some(node) => {
                    let size = std::fs::metadata(node).map(|m| m.len()).unwrap_or(0);
                    let backup = if crate::backup::exists(install) { " · backup on record" } else { "" };
                    out.push(Target {
                        label: install.label(),
                        detail: format!("{}{}", theme::bytes(size), backup),
                        kind: Kind::Ready { install: install.clone(), node: node.clone() },
                    });
                }
                None => {
                    let removed = discovery::voice_module_removed(&install.app_dir);
                    out.push(Target {
                        label: install.label(),
                        detail: if removed {
                            "voice module missing — Discord will not fetch it back on its own"
                                .to_string()
                        } else {
                            "no voice module yet — launch Discord once".to_string()
                        },
                        kind: Kind::NoModule { install: install.clone(), removed },
                    })
                }
            }
        }
    }

    // A module named by path survives as long as the file does. Dropping it
    // after every write would take the reader back to this screen straight
    // after patching, which is the one moment they want to see the readout.
    if let Some(path) = loose.filter(|p| p.is_file()) {
        let install = Install::for_path(path);
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let backup = if crate::backup::exists(&install) { " · backup on record" } else { "" };
        out.push(Target {
            label: install.label(),
            detail: format!("{}{} · {}", theme::bytes(size), backup, path.display()),
            kind: Kind::Loose { install, node: path.to_path_buf() },
        });
    }

    out.push(Target {
        label: "Another module, by path".to_string(),
        detail: "for a custom client using Discord's module".to_string(),
        kind: Kind::AskForPath,
    });
    out
}

// ---------------------------------------------------------------------------
// Terminal lifecycle
// ---------------------------------------------------------------------------

/// Run the interface. `bitrate` and `gain` seed the patch screen from whatever
/// the command line said, so `stereocord -b 320` still means something.
pub fn run(bitrate: u32, gain: f32) -> Result<(), String> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(
            "the interface needs a terminal. Run 'stereocord scan' for the same information \
             as text, or 'stereocord --help' for the commands."
                .to_string(),
        );
    }

    enable_raw_mode().map_err(|e| format!("cannot set up the terminal: {e}"))?;
    io::stdout()
        .execute(EnterAlternateScreen)
        .map_err(|e| format!("cannot set up the terminal: {e}"))?;
    install_panic_hook();

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).map_err(|e| format!("cannot draw: {e}"))?;
    let result = event_loop(&mut terminal, bitrate, gain);

    let _ = restore_terminal();
    let _ = terminal.show_cursor();
    result
}

fn restore_terminal() -> io::Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)
}

/// A panic inside the alternate screen would otherwise leave the terminal in
/// raw mode with no echo — the shell still works, but nothing you type appears,
/// which looks like the machine has locked up.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal();
        previous(info);
    }));
}

fn event_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    bitrate: u32,
    gain: f32,
) -> Result<(), String> {
    let mut app = App::new(bitrate, gain);

    while !app.quit {
        terminal
            .draw(|f| render::draw(f, &app))
            .map_err(|e| format!("cannot draw: {e}"))?;

        // Poll rather than block: the spinner has to keep turning, and a job
        // that finishes has to be noticed even if nobody touches the keyboard.
        if event::poll(Duration::from_millis(80)).map_err(|e| format!("input: {e}"))? {
            if let Event::Key(key) = event::read().map_err(|e| format!("input: {e}"))? {
                if key.kind == KeyEventKind::Press {
                    app.on_key(key);
                }
            }
        }
        app.tick = app.tick.wrapping_add(1);
        app.poll_job();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

impl App {
    fn poll_job(&mut self) {
        let Some(rx) = &self.job else { return };
        match rx.try_recv() {
            Ok(Reply::Scanned(inspection)) => {
                self.job = None;
                self.busy = None;
                self.seed_from(&inspection);
                self.inspection = Some(*inspection);
                self.status_cursor = 0;
            }
            Ok(Reply::Finished(log)) => {
                self.job = None;
                self.busy = None;
                let changed = log.changed;
                self.log = Some(*log);
                self.log_scroll = 0;
                self.screen = Screen::Result;
                if changed {
                    // The module on disk is not what the readout describes any
                    // more. Rather than leave a stale panel behind the result,
                    // drop it; leaving the screen re-scans.
                    self.inspection = None;
                    self.refresh_targets();
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.job = None;
                self.busy = None;
            }
        }
    }

    /// Start the patch screen from what the module already has rather than from
    /// the defaults, so that re-patching an install keeps the choices its owner
    /// made last time instead of quietly reverting them.
    fn seed_from(&mut self, inspection: &Inspection) {
        let patched = sites::GROUPS.iter().any(|g| inspection.group_roll(g.name).0 > 0);
        if !patched {
            self.groups_on = sites::GROUPS.iter().map(|g| g.default_on).collect();
            self.seeded_from_module = false;
            return;
        }
        self.groups_on = sites::GROUPS
            .iter()
            .map(|g| inspection.group_roll(g.name).0 > 0)
            .collect();
        self.seeded_from_module = true;
        // The locked bitrate is a number its owner chose once; read it back so
        // the control does not silently reset to the default on every re-patch.
        if let Some(kbps) = inspection.bitrate_bps.map(|bps| bps / 1000) {
            if (8..=512).contains(&kbps) {
                self.bitrate = kbps;
            }
        }
    }

    /// Rebuild the list after something on disk changed, and stay pointed at
    /// the same module — by path, since the row index will have moved.
    fn refresh_targets(&mut self) {
        // Re-find by label rather than by path: after a reinstall the row that
        // was standing on a module now has no module, and matching on the path
        // would lose it exactly when its manage screen is what you want.
        let here = self.target().map(|t| t.label.clone());
        self.targets = build_targets(self.loose.as_deref());
        self.current = here
            .as_deref()
            .and_then(|label| self.targets.iter().position(|t| t.label == label));
        self.target_cursor = self
            .current
            .unwrap_or_else(|| self.next_selectable(0, 1).unwrap_or(0));
    }

    fn start(&mut self, request: Request) {
        self.busy = Some(request.verb());
        self.job = Some(jobs::spawn(request));
    }

    fn rescan(&mut self) {
        let Some(target) = self.target() else { return };
        let Some(node) = target.node().map(|p| p.to_path_buf()) else { return };
        let install = target.install().cloned();
        self.inspection = None;
        self.start(Request::Scan { install, node });
    }

    fn open_target(&mut self, index: usize) {
        self.current = Some(index);
        self.screen = Screen::Overview;
        self.focus = Focus::Actions;
        self.action_cursor = 0;
        self.status_cursor = 0;
        self.manage_cursor = 0;
        self.rescan();
    }

    fn on_key(&mut self, key: KeyEvent) {
        // Ctrl-C always leaves, including out of a dialog or a running job.
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            self.quit = true;
            return;
        }
        if self.modal.is_some() {
            self.modal_key(key);
            return;
        }
        if self.busy.is_some() {
            // A job owns the file for as long as it runs. Let the user leave
            // the program, but not start a second one on top of the first.
            return;
        }
        match self.screen {
            Screen::Targets => self.targets_key(key),
            Screen::AskPath => self.ask_path_key(key),
            Screen::Overview => self.overview_key(key),
            Screen::Patch => self.patch_key(key),
            Screen::Manage => self.manage_key(key),
            Screen::Result => self.result_key(key),
        }
    }

    fn targets_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Up | KeyCode::Char('k') => self.move_target(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_target(1),
            KeyCode::Char('r') => self.refresh_targets(),
            KeyCode::Enter => {
                let index = self.target_cursor;
                let Some(target) = self.targets.get(index) else { return };
                if matches!(target.kind, Kind::AskForPath) {
                    self.screen = Screen::AskPath;
                    self.input_error = None;
                } else if target.selectable() {
                    self.open_target(index);
                }
            }
            _ => {}
        }
    }

    fn ask_path_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.screen = Screen::Targets;
                self.input_error = None;
            }
            KeyCode::Backspace => {
                self.input.pop();
                self.input_error = None;
            }
            KeyCode::Tab => self.complete_path(),
            KeyCode::Enter => self.accept_path(),
            KeyCode::Char(c) => {
                self.input.push(c);
                self.input_error = None;
            }
            _ => {}
        }
    }

    /// Extend the typed path as far as it can go unambiguously. Tab completion
    /// is the difference between typing a path into this box and giving up and
    /// going back to the shell.
    fn complete_path(&mut self) {
        let expanded = expand(&self.input);
        let (dir, prefix) = match expanded.rfind('/') {
            Some(i) => (expanded[..=i].to_string(), expanded[i + 1..].to_string()),
            None => ("./".to_string(), expanded.clone()),
        };
        let Ok(entries) = std::fs::read_dir(&dir) else { return };
        let mut matches: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(&prefix))
            .collect();
        if matches.is_empty() {
            return;
        }
        matches.sort();
        let common = matches
            .iter()
            .skip(1)
            .fold(matches[0].clone(), |acc, m| common_prefix(&acc, m));
        let mut completed = format!("{dir}{common}");
        if matches.len() == 1 && Path::new(&completed).is_dir() {
            completed.push('/');
        }
        self.input = completed;
    }

    fn accept_path(&mut self) {
        let path = PathBuf::from(expand(self.input.trim()));
        if !path.is_file() {
            self.input_error = Some(format!("{} is not a file", path.display()));
            return;
        }
        let canonical = path.canonicalize().unwrap_or(path);
        // If it turns out to be the module of an install already on the list,
        // use that row: it has the channel, the version and the right backup.
        if let Some(index) = self
            .targets
            .iter()
            .position(|t| matches!(t.kind, Kind::Ready { .. }) && t.node() == Some(&canonical))
        {
            self.screen = Screen::Targets;
            self.open_target(index);
            return;
        }
        self.loose = Some(canonical.clone());
        self.targets = build_targets(self.loose.as_deref());
        let Some(index) = self
            .targets
            .iter()
            .position(|t| matches!(t.kind, Kind::Loose { .. }))
        else {
            self.input_error = Some("could not read that file".to_string());
            return;
        };
        self.screen = Screen::Targets;
        self.open_target(index);
    }

    /// Open the manage screen with the cursor on something that can actually
    /// be chosen — starting it on a greyed-out row invites a keypress that
    /// does nothing, which reads as the program being stuck.
    fn open_manage(&mut self) {
        let items = self.manage_items();
        self.manage_cursor = items.iter().position(|i| i.enabled).unwrap_or(0);
        self.screen = Screen::Manage;
    }

    fn overview_key(&mut self, key: KeyEvent) {
        let rows = self.inspection.as_ref().map(|i| i.rows.len()).unwrap_or(0);
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Left => {
                self.screen = Screen::Targets;
                self.current = None;
                self.inspection = None;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Status => Focus::Actions,
                    Focus::Actions => Focus::Status,
                };
            }
            KeyCode::Char('r') => self.rescan(),
            KeyCode::Char('p') if self.action_enabled(0) => self.screen = Screen::Patch,
            KeyCode::Char('m') => self.open_manage(),
            KeyCode::Up | KeyCode::Char('k') => match self.focus {
                Focus::Status if rows > 0 => {
                    self.status_cursor = (self.status_cursor + rows - 1) % rows
                }
                Focus::Actions => {
                    self.action_cursor = (self.action_cursor + ACTIONS.len() - 1) % ACTIONS.len()
                }
                _ => {}
            },
            KeyCode::Down | KeyCode::Char('j') => match self.focus {
                Focus::Status if rows > 0 => self.status_cursor = (self.status_cursor + 1) % rows,
                Focus::Actions => self.action_cursor = (self.action_cursor + 1) % ACTIONS.len(),
                _ => {}
            },
            KeyCode::Enter | KeyCode::Right => match self.focus {
                // Enter in the readout means "I have read this", not "do the
                // highlighted thing" — there is no highlighted thing there.
                Focus::Status => self.focus = Focus::Actions,
                Focus::Actions if !self.action_enabled(self.action_cursor) => {}
                Focus::Actions => match self.action_cursor {
                    0 => self.screen = Screen::Patch,
                    1 => self.open_manage(),
                    _ => {
                        self.screen = Screen::Targets;
                        self.current = None;
                        self.inspection = None;
                    }
                },
            },
            _ => {}
        }
    }

    fn patch_key(&mut self, key: KeyEvent) {
        let n = sites::GROUPS.len();
        match key.code {
            KeyCode::Esc | KeyCode::Backspace => self.screen = Screen::Overview,
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Up | KeyCode::Char('k') => self.group_cursor = (self.group_cursor + n - 1) % n,
            KeyCode::Down | KeyCode::Char('j') => self.group_cursor = (self.group_cursor + 1) % n,
            KeyCode::Char(' ') => {
                self.groups_on[self.group_cursor] = !self.groups_on[self.group_cursor];
                self.seeded_from_module = false;
            }
            KeyCode::Char('a') => {
                self.groups_on.iter_mut().for_each(|v| *v = true);
                self.seeded_from_module = false;
            }
            KeyCode::Char('n') => {
                self.groups_on.iter_mut().for_each(|v| *v = false);
                self.seeded_from_module = false;
            }
            KeyCode::Char('d') => {
                self.groups_on = sites::GROUPS.iter().map(|g| g.default_on).collect();
                self.seeded_from_module = false;
            }
            KeyCode::Left | KeyCode::Char('-') => self.bitrate = step_bitrate(self.bitrate, -1),
            KeyCode::Right | KeyCode::Char('+') | KeyCode::Char('=') => {
                self.bitrate = step_bitrate(self.bitrate, 1)
            }
            KeyCode::Enter => self.confirm_patch(),
            _ => {}
        }
    }

    fn manage_key(&mut self, key: KeyEvent) {
        let items = self.manage_items();
        let n = items.len();
        match key.code {
            KeyCode::Esc | KeyCode::Backspace => self.screen = Screen::Overview,
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Up | KeyCode::Char('k') => self.manage_cursor = (self.manage_cursor + n - 1) % n,
            KeyCode::Down | KeyCode::Char('j') => self.manage_cursor = (self.manage_cursor + 1) % n,
            KeyCode::Enter => {
                if items.get(self.manage_cursor).map(|i| i.enabled) != Some(true) {
                    return;
                }
                match self.manage_cursor {
                    0 => self.confirm_restore(),
                    1 => self.confirm_delete_backup(),
                    2 => self.confirm_reinstall(),
                    _ => self.screen = Screen::Overview,
                }
            }
            _ => {}
        }
    }

    fn result_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Up | KeyCode::Char('k') => self.log_scroll = self.log_scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.log_scroll = self.log_scroll.saturating_add(1),
            KeyCode::Enter | KeyCode::Esc | KeyCode::Backspace => {
                self.log = None;
                if self.target().is_some() {
                    self.screen = Screen::Overview;
                    self.focus = Focus::Actions;
                    self.action_cursor = 0;
                    if self.inspection.is_none() {
                        self.rescan();
                    }
                } else {
                    // Whatever it described is no longer on the list at all.
                    self.refresh_targets();
                    self.current = None;
                    self.target_cursor = self.next_selectable(0, 1).unwrap_or(0);
                    self.screen = Screen::Targets;
                }
            }
            _ => {}
        }
    }

    fn modal_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.modal = None,
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                if let Some(m) = &mut self.modal {
                    m.cursor = 1 - m.cursor;
                }
            }
            KeyCode::Enter => {
                let Some(mut modal) = self.modal.take() else { return };
                if modal.cursor == 0 {
                    if let Some(request) = modal.request.take() {
                        self.start(request);
                    }
                }
            }
            _ => {}
        }
    }

    // --- the dialogs ------------------------------------------------------

    fn confirm_patch(&mut self) {
        let Some(target) = self.target() else { return };
        let Some(node) = target.node().map(|p| p.to_path_buf()) else { return };
        let Some(install) = target.install().cloned() else { return };
        let cfg = self.config();
        if cfg.groups.is_empty() {
            self.modal = Some(Modal {
                title: "Nothing selected".to_string(),
                body: vec![
                    "Every group is switched off, so there is nothing to write.".to_string(),
                    "Press d to go back to the recommended set.".to_string(),
                ],
                confirm: "All right".to_string(),
                danger: false,
                cursor: 0,
                request: None,
            });
            return;
        }

        let mut body = vec![format!("This writes to {}", node.display())];
        body.push(String::new());
        body.push(format!(
            "{} of {} groups, bitrate {} kbps. The original is copied aside first, and the \
             file is read back afterwards to confirm every change landed.",
            cfg.groups.len(),
            sites::GROUPS.len(),
            self.bitrate
        ));

        let pids = discovery::running_pids(&install.app_dir);
        if !pids.is_empty() {
            body.push(String::new());
            body.push(format!(
                "Discord is running (pid {}). It already has the old module loaded, so \
                 nothing will change until you quit it completely and start it again.",
                pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
            ));
        }

        let gaps = self.critical_gaps();
        let danger = !gaps.is_empty();
        if danger {
            body.push(String::new());
            body.push(format!(
                "{} change(s) that decide mono versus stereo could not be found in this \
                 build. Patching anyway would advertise stereo and still send one channel, \
                 which is worse than leaving it alone.",
                gaps.len()
            ));
        }
        if !pids.is_empty() && !danger {
            // Quitting Discord first is the difference between this working and
            // appearing not to, so make the default answer "no" for that too.
        }

        self.modal = Some(Modal {
            title: if danger { "Not recommended".to_string() } else { "Patch this module?".to_string() },
            body,
            confirm: if danger { "Patch anyway".to_string() } else { "Patch".to_string() },
            danger: danger || !pids.is_empty(),
            cursor: if danger || !pids.is_empty() { 1 } else { 0 },
            request: Some(Request::Patch { install, node, cfg, allow_partial: danger }),
        });
    }

    fn confirm_restore(&mut self) {
        let Some(target) = self.target() else { return };
        let Some(node) = target.node().map(|p| p.to_path_buf()) else { return };
        let Some(install) = target.install().cloned() else { return };
        self.modal = Some(Modal {
            title: "Restore the original?".to_string(),
            body: vec![
                format!("This overwrites {}", node.display()),
                String::new(),
                "with the untouched copy taken before the first patch. Nothing is lost — \
                 you can patch again afterwards."
                    .to_string(),
            ],
            confirm: "Restore".to_string(),
            danger: false,
            cursor: 0,
            request: Some(Request::Restore { install, node }),
        });
    }

    fn confirm_delete_backup(&mut self) {
        let Some(install) = self.target().and_then(|t| t.install()).cloned() else { return };
        self.modal = Some(Modal {
            title: "Delete the backup?".to_string(),
            body: vec![
                format!("This deletes {}", crate::backup::path_for(&install).display()),
                String::new(),
                "After this, a patched module for this install cannot be restored. The only \
                 way back would be to delete the module and let Discord download it again."
                    .to_string(),
            ],
            confirm: "Delete the backup".to_string(),
            danger: true,
            cursor: 1,
            request: Some(Request::DeleteBackup { install }),
        });
    }

    fn confirm_reinstall(&mut self) {
        let Some(target) = self.target() else { return };
        let Some(install) = target.install().cloned() else { return };
        let node = target.node().map(|p| p.to_path_buf());

        let mut body = Vec::new();
        match node.as_deref().and_then(discovery::module_dir) {
            Some(dir) => body.push(format!("This deletes {}", dir.display())),
            None => body.push("The voice module is already gone.".to_string()),
        }
        if let Some(db) = discovery::installer_db(&install.app_dir) {
            body.push(String::new());
            body.push(format!(
                "It also clears Discord's record of what it has installed, at {}. \
                 Without that, Discord believes the module is still there and never \
                 downloads it — which is what leaves the client reporting itself corrupt.",
                db.display()
            ));
            body.push(String::new());
            body.push(
                "Discord re-downloads its modules on the next start. Logins, servers and \
                 settings are not in that file and are not touched, and a copy of it is \
                 kept."
                    .to_string(),
            );
        }
        body.push(String::new());
        body.push("Quit Discord first, or it will not notice.".to_string());
        if crate::backup::exists(&install) {
            body.push(String::new());
            body.push(
                "Your backup of this module stays — but once Discord downloads a newer \
                 one, it belongs to a version you no longer have."
                    .to_string(),
            );
        }

        self.modal = Some(Modal {
            title: "Have Discord fetch a fresh module?".to_string(),
            body,
            confirm: "Clear it".to_string(),
            danger: true,
            cursor: 1,
            request: Some(Request::Reinstall { install, node }),
        });
    }
}

/// Bitrate steps, coarse where the differences stop being audible.
fn step_bitrate(current: u32, direction: i32) -> u32 {
    const STOPS: &[u32] = &[
        8, 16, 24, 32, 48, 64, 96, 128, 160, 192, 224, 248, 256, 320, 384, 448, 512,
    ];
    let index = STOPS
        .iter()
        .position(|&s| s >= current)
        .unwrap_or(STOPS.len() - 1);
    let next = index as i32 + direction;
    STOPS[next.clamp(0, STOPS.len() as i32 - 1) as usize]
}

/// `~` and `$HOME` are what people type; neither means anything to `Path`.
fn expand(input: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    if home.is_empty() {
        return input.to_string();
    }
    if input == "~" {
        return home;
    }
    if let Some(rest) = input.strip_prefix("~/") {
        return format!("{home}/{rest}");
    }
    if let Some(rest) = input.strip_prefix("$HOME") {
        return format!("{home}{rest}");
    }
    input.to_string()
}

fn common_prefix(a: &str, b: &str) -> String {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).map(|(x, _)| x).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrate_steps_stop_at_the_ends() {
        assert_eq!(step_bitrate(8, -1), 8);
        assert_eq!(step_bitrate(512, 1), 512);
        assert_eq!(step_bitrate(248, 1), 256);
        assert_eq!(step_bitrate(248, -1), 224);
    }

    #[test]
    fn an_off_scale_bitrate_lands_on_a_neighbouring_stop() {
        // --bitrate 300 is legal on the command line but not one of the stops.
        assert_eq!(step_bitrate(300, 1), 384);
        assert_eq!(step_bitrate(300, -1), 256);
    }

    #[test]
    fn tildes_expand_and_other_paths_are_left_alone() {
        std::env::set_var("HOME", "/home/someone");
        assert_eq!(expand("~"), "/home/someone");
        assert_eq!(expand("~/x/y"), "/home/someone/x/y");
        assert_eq!(expand("$HOME/x"), "/home/someone/x");
        assert_eq!(expand("/abs/path"), "/abs/path");
        assert_eq!(expand("relative"), "relative");
    }

    #[test]
    fn completion_prefixes_agree_on_what_they_share() {
        assert_eq!(common_prefix("discord_voice", "discord_vo"), "discord_vo");
        assert_eq!(common_prefix("abc", "xyz"), "");
    }

    #[test]
    fn the_opening_list_always_offers_a_way_in() {
        // Even with nothing installed, the path row has to be selectable —
        // otherwise a custom client's user has no way past the first screen.
        let targets = build_targets(None);
        assert!(targets.iter().any(|t| matches!(t.kind, Kind::AskForPath)));
        assert!(targets.iter().any(|t| t.selectable()));
    }
}
