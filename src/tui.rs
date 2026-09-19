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

//! The group picker shown before patching.
//!
//! Deliberately dependency-free, like the rest of the tool: raw mode is set by
//! shelling out to `stty`, and everything else is ANSI escapes and bytes read
//! from stdin. That is a little crude next to a real terminal crate, but it
//! keeps `cargo build` on a fresh machine to exactly one crate, which is worth
//! more here than a nicer redraw.

use crate::resolve::Report;
use crate::sites::{self, Group};
use std::io::{Read, Write};
use std::process::{Command, Stdio};

/// What the user chose, or that they backed out.
pub enum Choice {
    Apply(Vec<String>),
    Cancel,
}

/// Terminal state saved on entry and put back on the way out, whatever happens.
struct RawMode {
    saved: String,
}

impl RawMode {
    /// `None` when there is no terminal to put into raw mode — a pipe, a CI
    /// run, output redirected to a file. Callers fall back to the defaults.
    fn enter() -> Option<RawMode> {
        let out = Command::new("stty")
            .arg("-g")
            .stdin(Stdio::inherit())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let saved = String::from_utf8(out.stdout).ok()?.trim().to_string();
        let ok = Command::new("stty")
            .args(["raw", "-echo"])
            .stdin(Stdio::inherit())
            .status()
            .ok()?
            .success();
        if !ok {
            return None;
        }
        print!("\x1b[?1049h\x1b[?25l");
        std::io::stdout().flush().ok();
        Some(RawMode { saved })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        print!("\x1b[?25h\x1b[?1049l");
        std::io::stdout().flush().ok();
        let _ = Command::new("stty")
            .arg(&self.saved)
            .stdin(Stdio::inherit())
            .status();
    }
}

/// How many of a group's sites this build is actually covered for.
///
/// A site the build does not need counts as covered, the same way `scan` prints
/// it as `n/a` rather than as a gap — otherwise every build would look short by
/// however many surround-codec and inlined-filter sites it happens not to have.
fn coverage(report: &Report, group: &str) -> (usize, usize) {
    let mine = || sites::SITES.iter().filter(|s| s.group == group);
    let total = mine().count();
    let found = mine()
        .filter(|s| {
            report.resolved(s.name).is_some() || crate::excused(s.name, report).is_some()
        })
        .count();
    (found, total)
}

/// Visible width of `s`, i.e. ignoring ANSI escape sequences. Only used for
/// column padding, so it assumes the well-formed `ESC [ ... letter` sequences
/// this module emits rather than trying to be a general parser.
fn visible_width(s: &str) -> usize {
    let mut n = 0usize;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            n += 1;
        }
    }
    n
}

/// Break `text` into lines of at most `width` columns, indenting every line by
/// `indent` spaces. Whitespace-only wrapping; the summaries are plain prose so
/// there is nothing cleverer to do.
fn wrap(text: &str, width: usize, indent: usize) -> Vec<String> {
    let pad = " ".repeat(indent);
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > width {
            lines.push(format!("{pad}{cur}"));
            cur = String::new();
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(format!("{pad}{cur}"));
    }
    lines
}

const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

fn draw(label: &str, on: &[bool], cursor: usize, report: &Report, bitrate: u32) {
    let mut buf = String::new();
    buf.push_str("\x1b[2J\x1b[H");
    buf.push_str(&format!(
        "{BOLD}stereocord{RESET} — choose what to change in {label}\r\n"
    ));
    buf.push_str(&format!(
        "{DIM}Bitrate {bitrate} kbps. Everything below is applied to a copy; the \
         original is backed up first.{RESET}\r\n\r\n"
    ));

    for (i, g) in sites::GROUPS.iter().enumerate() {
        let (found, total) = coverage(report, g.name);
        let selected = i == cursor;
        let marker = if selected { "›" } else { " " };
        let box_ = if on[i] {
            format!("{GREEN}[x]{RESET}")
        } else {
            format!("{DIM}[ ]{RESET}")
        };
        let title = if selected {
            format!("{BOLD}{}{RESET}", g.title)
        } else if on[i] {
            g.title.to_string()
        } else {
            format!("{DIM}{}{RESET}", g.title)
        };
        // A group whose sites could not all be located still shows, with the
        // shortfall next to it — hiding it would be the same overstatement the
        // report elsewhere goes out of its way to avoid.
        let cov = if found == total {
            format!("{DIM}{found}/{total}{RESET}")
        } else {
            format!("{YELLOW}{found}/{total}{RESET}")
        };
        // Pad by visible width: `{:<32}` would count the escape bytes in the
        // bold and dim variants and under-pad exactly the rows that are
        // styled, which is every row the eye is drawn to.
        let pad = " ".repeat(32usize.saturating_sub(visible_width(&title)));
        buf.push_str(&format!("{marker} {box_} {title}{pad} {cov}\r\n"));
    }

    let g: &Group = &sites::GROUPS[cursor];
    buf.push_str("\r\n");
    for line in wrap(g.summary, 72, 4) {
        buf.push_str(&format!("{line}\r\n"));
    }
    if let Some(c) = g.caveat {
        buf.push_str("\r\n");
        for line in wrap(c, 72, 4) {
            buf.push_str(&format!("{YELLOW}{line}{RESET}\r\n"));
        }
    }
    let (found, total) = coverage(report, g.name);
    if found < total {
        buf.push_str("\r\n");
        for line in wrap(
            &format!(
                "{} of this group's {} changes could not be found in this build and \
                 will be skipped.",
                total - found,
                total
            ),
            72,
            4,
        ) {
            buf.push_str(&format!("{YELLOW}{line}{RESET}\r\n"));
        }
    }

    buf.push_str(&format!(
        "\r\n{DIM}↑/↓ move   space toggle   a all   n none   d defaults   \
         enter apply   q cancel{RESET}\r\n"
    ));
    print!("{buf}");
    std::io::stdout().flush().ok();
}

/// Run the picker. `label` names the install being patched.
///
/// Returns `None` when there is no usable terminal, which the caller reads as
/// "carry on with the defaults" rather than as a failure.
pub fn pick(label: &str, report: &Report, bitrate: u32) -> Option<Choice> {
    let _raw = RawMode::enter()?;

    let mut on: Vec<bool> = sites::GROUPS.iter().map(|g| g.default_on).collect();
    let mut cursor = 0usize;
    let n = sites::GROUPS.len();
    let mut stdin = std::io::stdin();
    let mut byte = [0u8; 1];

    loop {
        draw(label, &on, cursor, report, bitrate);
        if stdin.read(&mut byte).ok()? == 0 {
            return Some(Choice::Cancel);
        }
        match byte[0] {
            b'q' | 0x03 => return Some(Choice::Cancel),
            b'\r' | b'\n' => break,
            b' ' => on[cursor] = !on[cursor],
            b'a' => on.iter_mut().for_each(|v| *v = true),
            b'n' => on.iter_mut().for_each(|v| *v = false),
            b'd' => {
                on = sites::GROUPS.iter().map(|g| g.default_on).collect();
            }
            b'k' => cursor = (cursor + n - 1) % n,
            b'j' => cursor = (cursor + 1) % n,
            0x1b => {
                // Either a bare Escape (cancel) or an arrow key, which arrives
                // as ESC [ A/B. Read the rest without blocking forever by
                // asking for both bytes at once.
                let mut seq = [0u8; 2];
                match stdin.read(&mut seq) {
                    Ok(2) if seq[0] == b'[' => match seq[1] {
                        b'A' => cursor = (cursor + n - 1) % n,
                        b'B' => cursor = (cursor + 1) % n,
                        _ => {}
                    },
                    _ => return Some(Choice::Cancel),
                }
            }
            _ => {}
        }
    }

    Some(Choice::Apply(
        sites::GROUPS
            .iter()
            .zip(&on)
            .filter(|(_, &v)| v)
            .map(|(g, _)| g.name.to_string())
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_respects_width_and_indent() {
        // The width applies to the text, not to the indent in front of it.
        let out = wrap("one two three four five", 9, 2);
        assert_eq!(out, vec!["  one two", "  three", "  four five"]);
    }

    #[test]
    fn wrap_keeps_an_overlong_word_on_its_own_line() {
        let out = wrap("a supercalifragilistic b", 8, 0);
        assert_eq!(out, vec!["a", "supercalifragilistic", "b"]);
    }

    #[test]
    fn visible_width_ignores_escapes() {
        assert_eq!(visible_width("plain"), 5);
        assert_eq!(visible_width("\x1b[1mbold\x1b[0m"), 4);
        assert_eq!(visible_width("\x1b[2mdim\x1b[0m tail"), 8);
    }

    #[test]
    fn wrap_of_empty_text_is_empty() {
        assert!(wrap("   ", 10, 2).is_empty());
    }
}
