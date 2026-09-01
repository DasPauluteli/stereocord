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

//! Building and applying the concrete list of edits.

use crate::resolve::Report;
use crate::shellcode;
use crate::sig::hex;
use crate::sites::{Action, SITES};

#[derive(Clone)]
pub struct Config {
    pub bitrate_kbps: u32,
    pub gain: f32,
}

impl Default for Config {
    fn default() -> Self {
        Config { bitrate_kbps: 248, gain: 1.0 }
    }
}

impl Config {
    pub fn bitrate_bps(&self) -> u32 {
        self.bitrate_kbps * 1000
    }
}

pub struct Edit {
    pub site: &'static str,
    pub group: &'static str,
    pub what: &'static str,
    pub offset: usize,
    pub bytes: Vec<u8>,
}

pub struct Plan {
    pub edits: Vec<Edit>,
}

impl Plan {
    pub fn total_bytes(&self) -> usize {
        self.edits.iter().map(|e| e.bytes.len()).sum()
    }
}

/// Turn resolved offsets into the bytes that will be written.
/// Resolve a RIP-relative operand: `offset` points at the disp32, which is
/// relative to the end of the instruction it belongs to.
fn rip_target(data: &[u8], disp_at: usize) -> Option<usize> {
    let bytes = data.get(disp_at..disp_at + 4)?;
    let disp = i32::from_le_bytes(bytes.try_into().ok()?);
    let next_insn = disp_at + 4;
    let target = next_insn as i64 + disp as i64;
    if target < 0 || target as usize + 4 > data.len() {
        return None;
    }
    Some(target as usize)
}

pub fn build(report: &Report, cfg: &Config, data: &[u8]) -> Plan {
    let bitrate = cfg.bitrate_bps().to_le_bytes().to_vec();
    let mut edits = Vec::new();

    for site in SITES {
        let Some(resolved) = report.resolved(site.name) else { continue };
        for &offset in &resolved.offsets {
            let bytes = match site.action {
                Action::Bytes(b) => b.to_vec(),
                Action::BitrateImm32 => bitrate.clone(),
                Action::BitrateSetter => {
                    // push rbp ; mov edx, <bitrate>
                    //
                    // The original prologue is `push rbp; mov rbp,rsp; mov
                    // edx,esi`, where the frame pointer is never used. Six
                    // bytes in, six bytes out, and every caller's requested
                    // bitrate is discarded in favour of the configured one.
                    let mut v = vec![0x55, 0xBA];
                    v.extend_from_slice(&bitrate);
                    v
                }
                Action::ShellcodeHpCutoff => shellcode::hp_cutoff(cfg.gain),
                Action::ShellcodeDcReject => shellcode::dc_reject(cfg.gain),
                Action::RipRelF32(v) => v.to_le_bytes().to_vec(),
            };
            // A RIP-relative site patches the constant, not the instruction.
            let offset = match site.action {
                Action::RipRelF32(_) => match rip_target(data, offset) {
                    Some(t) => t,
                    None => continue,
                },
                _ => offset,
            };
            edits.push(Edit {
                site: site.name,
                group: site.group,
                what: site.what,
                offset,
                bytes,
            });
        }
    }
    Plan { edits }
}

pub enum CheckError {
    OutOfRange { site: &'static str, offset: usize, len: usize },
    UnexpectedBytes { site: &'static str, offset: usize, found: String, expected: Vec<String> },
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckError::OutOfRange { site, offset, len } => write!(
                f,
                "{site}: edit at 0x{offset:X} (+{len} bytes) runs past the end of the file"
            ),
            CheckError::UnexpectedBytes { site, offset, found, expected } => write!(
                f,
                "{site}: 0x{offset:X} holds {found}, expected one of [{}]",
                expected.join(" | ")
            ),
        }
    }
}

/// Confirm every edit lands inside the file and on bytes the site vouches for.
///
/// A signature can in principle match somewhere unintended in a 100 MB binary.
/// Checking the bytes actually present against what the site declares turns
/// that into a refusal instead of a corrupted voice module.
pub fn check(plan: &Plan, data: &[u8]) -> Vec<CheckError> {
    let mut errs = Vec::new();
    for edit in &plan.edits {
        if edit.offset + edit.bytes.len() > data.len() {
            errs.push(CheckError::OutOfRange {
                site: edit.site,
                offset: edit.offset,
                len: edit.bytes.len(),
            });
            continue;
        }
        let Some(site) = crate::sites::find(edit.site) else { continue };
        if site.expect_orig.is_empty() {
            continue;
        }
        let ok = site.expect_orig.iter().any(|exp| {
            edit.offset + exp.len() <= data.len()
                && &data[edit.offset..edit.offset + exp.len()] == *exp
        });
        if !ok {
            let width = site
                .expect_orig
                .iter()
                .map(|e| e.len())
                .max()
                .unwrap_or(8)
                .min(data.len() - edit.offset);
            errs.push(CheckError::UnexpectedBytes {
                site: edit.site,
                offset: edit.offset,
                found: hex(&data[edit.offset..edit.offset + width]),
                expected: site.expect_orig.iter().map(|e| hex(e)).collect(),
            });
        }
    }
    errs
}

pub fn apply(plan: &Plan, data: &mut [u8]) {
    for edit in &plan.edits {
        data[edit.offset..edit.offset + edit.bytes.len()].copy_from_slice(&edit.bytes);
    }
}

/// Read back what was written. Cheap, and it catches a truncated or
/// concurrently-replaced file rather than reporting a success that did not
/// happen.
pub fn verify(plan: &Plan, data: &[u8]) -> Vec<&'static str> {
    plan.edits
        .iter()
        .filter(|e| {
            e.offset + e.bytes.len() > data.len()
                || &data[e.offset..e.offset + e.bytes.len()] != e.bytes.as_slice()
        })
        .map(|e| e.site)
        .collect()
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum State {
    Stock,
    /// Carries this tool's injected filters.
    Stereocord,
    /// Patched, by something. Which tool cannot be told apart: the injected
    /// filters are the only distinctive fingerprint, and on builds that inline
    /// them there is nothing to inject, so a module this tool patched looks
    /// exactly like one the upstream script patched.
    Patched,
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            State::Stock => write!(f, "stock"),
            State::Stereocord => write!(f, "patched by stereocord"),
            State::Patched => write!(f, "already patched (by this tool or a compatible one)"),
        }
    }
}

/// Classify a module.
///
/// The injected filters are recognised directly. Beyond that, several sites
/// wildcard the very byte they patch, so they resolve on a patched binary too
/// and their current value can be compared against what a stock build holds.
/// That distinguishes "some tool has been here" from "this is a build whose
/// code has moved", which need different answers.
pub fn classify(data: &[u8], report: &Report) -> State {
    if contains(data, &shellcode::hp_cutoff_marker())
        || contains(data, &shellcode::dc_reject_marker())
    {
        return State::Stereocord;
    }
    // Without the injected filters there is no fingerprint, so the most that
    // can be said is that someone has been here.
    for site in SITES {
        if site.stock.is_empty() {
            continue;
        }
        let Some(resolved) = report.resolved(site.name) else { continue };
        for &offset in &resolved.offsets {
            let end = offset + site.stock.len();
            if end <= data.len() && &data[offset..end] != site.stock {
                return State::Patched;
            }
        }
    }
    State::Stock
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || hay.len() < needle.len() {
        return false;
    }
    hay.windows(needle.len()).any(|w| w == needle)
}
