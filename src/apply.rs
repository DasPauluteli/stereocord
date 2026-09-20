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

//! Patching one install, with the decisions left to the caller.
//!
//! Both front ends drive the same steps in the same order, and the order is
//! load-bearing: a module is never patched twice, the backup is taken before
//! the first write, and the file is read back afterwards. Keeping that
//! sequence in one place is the only way the terminal UI and the command line
//! cannot drift into disagreeing about what "patched" means.
//!
//! What the caller supplies is the judgement in the middle — which groups,
//! and whether to go ahead — because that is the part that looks completely
//! different behind a prompt and behind a picker. Everything either side of it
//! is here.

use crate::backup;
use crate::discovery::Install;
use crate::elf;
use crate::md5;
use crate::patch::{self, Config, Plan, State};
use crate::resolve::{self, Outcome, Report};
use crate::sites;
use std::fs;
use std::io::Write;
use std::path::Path;

/// A line worth showing, and how much attention it deserves.
pub enum Note {
    Info(String),
    Good(String),
    Warn(String),
    Bad(String),
}

/// The module, loaded and known to be stock.
pub struct Prepared {
    pub data: Vec<u8>,
    pub report: Report,
    /// Real gaps, excluding the ones this build does not need.
    pub missing: Vec<&'static str>,
    /// Gaps that decide mono versus stereo.
    pub critical_missing: Vec<&'static str>,
}

impl Prepared {
    /// Whether patching would leave stereo half-done.
    pub fn blocked(&self) -> bool {
        !self.critical_missing.is_empty()
    }
}

/// Load the module and, if something has already patched it, put the backup
/// back first.
///
/// Re-patching a patched binary would compound edits — and for the injected
/// filters the original bytes are simply gone — so every run starts from a
/// stock file or does not start at all.
pub fn prepare(
    install: &Install,
    node: &Path,
    dry_run: bool,
    log: &mut dyn FnMut(Note),
) -> Result<Prepared, String> {
    let mut data = read(node)?;
    log(Note::Info(format!("{} bytes, md5 {}", data.len(), md5::hex(&data))));

    let first = resolve::resolve_all(&data, elf::symbols(&data).as_ref());
    let was = patch::classify(&data, &first);

    if was != State::Stock {
        if !backup::exists(install) {
            return Err(format!(
                "{} is {was}, and no backup is on record here.\n\
                 Its original instructions are gone, so there is nothing left to patch \
                 against. Delete the module and start Discord so it re-downloads a stock \
                 one, then patch.",
                node.display()
            ));
        }
        if dry_run {
            // Read the backup rather than writing it. Without this the resolve
            // below runs against the still-patched file, every signature comes
            // up MISSING, and a dry run reports a build as unpatchable when it
            // is nothing of the sort.
            log(Note::Info(format!("{was}; would restore the backup first")));
            data = read(&backup::path_for(install))?;
        } else {
            let src = backup::restore(install, node).map_err(|e| format!("restoring backup: {e}"))?;
            log(Note::Info(format!("{was}; restored {} first", src.display())));
            data = read(node)?;
        }
    }

    let report = resolve::resolve_all(&data, elf::symbols(&data).as_ref());
    let missing: Vec<&'static str> = report
        .missing()
        .into_iter()
        .filter(|n| report.excused(n).is_none())
        .collect();
    let critical_missing: Vec<&'static str> = missing
        .iter()
        .copied()
        .filter(|n| sites::find(n).map(|s| s.critical).unwrap_or(true))
        .collect();

    Ok(Prepared { data, report, missing, critical_missing })
}

/// Say what could not be located, and what that costs.
pub fn report_gaps(p: &Prepared, log: &mut dyn FnMut(Note)) {
    let all = p.report.missing();
    if all.is_empty() {
        log(Note::Good(format!("all {} sites located", p.report.order.len())));
        return;
    }
    log(Note::Info(format!(
        "{} of {} sites located",
        p.report.order.len() - all.len(),
        p.report.order.len()
    )));
    for name in &all {
        if let Some(note) = p.report.excused(name) {
            log(Note::Info(format!("n/a  {name} — {note}")));
            continue;
        }
        match p.report.outcomes.get(name) {
            Some(Outcome::Ambiguous { found, wanted }) => {
                log(Note::Warn(format!("AMBIG  {name} (matched {found}, expected {wanted})")))
            }
            _ => log(Note::Warn(format!("MISSING  {name}"))),
        }
    }
    if p.blocked() {
        log(Note::Bad(format!(
            "{} of the missing sites decide whether audio is mono or stereo. A partial \
             patch here would negotiate stereo and still send one channel.",
            p.critical_missing.len()
        )));
    } else if p.missing.is_empty() {
        log(Note::Good("everything this build needs resolved".to_string()));
    } else {
        log(Note::Info(
            "all stereo-critical sites resolved; the rest are quality refinements"
                .to_string(),
        ));
    }
}

/// Build the plan and check it against the bytes actually present.
pub fn plan(p: &Prepared, cfg: &Config) -> Result<Plan, String> {
    let plan = patch::build(&p.report, cfg, &p.data);
    let errors = patch::check(&plan, &p.data);
    if !errors.is_empty() {
        let detail: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
        return Err(format!(
            "signature matched unexpected bytes; nothing was written.\n{}\n\
             Please report the build (size and md5 above) so the catalogue can be updated.",
            detail.join("\n")
        ));
    }
    Ok(plan)
}

/// What this plan actually does, in the words the picker uses.
///
/// Built from the edits that are really in the plan rather than from the
/// groups that were asked for: on a build where a site could not be located,
/// claiming the filters are bypassed when they were skipped is exactly the
/// kind of quiet overstatement this tool exists to avoid.
pub fn effects(plan: &Plan, report: &Report, cfg: &Config) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let has = |group: &str| plan.edits.iter().any(|e| e.group == group);
    let applied = |name: &str| plan.edits.iter().any(|e| e.site == name);

    if has("stereo") {
        // Say separately whether the network adaptor's mid-call downgrade is
        // blocked. A build where that site did not apply still starts the call
        // in stereo and can fall back to mono once the uplink estimate dips,
        // which sounds like the patch wearing off rather than like what it is.
        out.push(if applied("ChannelController_ForceStereo") {
            "stereo (held against the network adaptor)".to_string()
        } else {
            "stereo".to_string()
        });
    }
    if has("samplerate") {
        out.push("48 kHz".to_string());
    }
    if has("bitrate") {
        out.push(format!("{} kbps", cfg.bitrate_kbps));
    }
    if has("opus") {
        out.push("audio mode".to_string());
    }
    if has("encoder") {
        out.push("FEC/DTX off, complexity 10, fullband".to_string());
    }
    if has("celt") {
        out.push("CELT".to_string());
    }
    if has("gain") {
        out.push("no auto-gain".to_string());
    }
    if has("denoise") {
        out.push("no noise removal".to_string());
    }
    if has("echo") {
        out.push("no echo cancellation".to_string());
    }
    if has("cbr") {
        out.push("constant bitrate".to_string());
    }
    // "Filterless" means both Opus input filters are out of the path. That can
    // happen three ways per filter: the function was replaced, its coefficient
    // was neutralised, or the build cannot reach it at all. Count all three,
    // otherwise a fully-bypassed build reports as a partial one.
    let hp_done = applied("HpCutoff_Inject") || report.excused("HpCutoff_Inject").is_some();
    let dc_done = applied("DcReject_Inject") || applied("DcReject_Coefficient");
    if hp_done && dc_done {
        out.push(if cfg.gain != 1.0 {
            format!("filters bypassed (gain x{})", cfg.gain)
        } else {
            "filters bypassed".to_string()
        });
    } else if has("filter") {
        out.push("high-pass bypassed (opus filters still active)".to_string());
    }
    out
}

/// Take the backup, write the plan, and read it back.
///
/// Returns the md5 of the patched module. Nothing here asks anything: by the
/// time it is called the decision has been made.
pub fn commit(
    install: &Install,
    node: &Path,
    p: Prepared,
    plan: &Plan,
    log: &mut dyn FnMut(Note),
) -> Result<String, String> {
    let (backup_path, created) =
        backup::ensure(install, node).map_err(|e| format!("creating backup: {e}"))?;
    log(Note::Info(format!(
        "backup {} {}",
        if created { "written to" } else { "already at" },
        backup_path.display()
    )));

    let mut data = p.data;
    patch::apply(plan, &mut data);
    write(node, &data)?;

    let readback = read(node)?;
    let failed = patch::verify(plan, &readback);
    if !failed.is_empty() {
        return Err(format!(
            "verification failed after writing: {}. Restore the original to get back to \
             a known state.",
            failed.join(", ")
        ));
    }
    let sum = md5::hex(&readback);
    log(Note::Good(format!("verified, md5 now {sum}")));
    Ok(sum)
}

pub fn read(node: &Path) -> Result<Vec<u8>, String> {
    fs::read(node).map_err(|e| format!("cannot read {}: {e}", node.display()))
}

/// Write through a temporary file and rename, so an interrupted write cannot
/// leave a half-patched module in place.
pub fn write(node: &Path, data: &[u8]) -> Result<(), String> {
    let mode = fs::metadata(node).ok().map(|m| {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode()
    });
    let tmp = node.with_extension("node.stereocord.part");
    {
        let mut f = fs::File::create(&tmp)
            .map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
        f.write_all(data).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
        f.sync_all().map_err(|e| format!("syncing {}: {e}", tmp.display()))?;
    }
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(mode));
    }
    fs::rename(&tmp, node).map_err(|e| format!("replacing {}: {e}", node.display()))?;
    Ok(())
}
