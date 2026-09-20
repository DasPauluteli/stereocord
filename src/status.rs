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

//! Reading a module back and saying, in plain words, what it will sound like.
//!
//! `scan` answers "can the catalogue still find its way around this build",
//! which is a question about the tool. This answers "what is this module
//! currently doing to my audio", which is the question the person actually
//! has. The two are different enough to be worth keeping apart: a site can be
//! perfectly locatable and simply not applied.
//!
//! The awkward part is that a patched module hides its own evidence. Most
//! signatures key on the very instructions the patch overwrites, so scanning a
//! patched file reports two thirds of the catalogue as MISSING and can say
//! nothing about what was done to it. The way out is the backup: it is a stock
//! copy of the same build, patching never changes the file's length, and so
//! offsets resolved against the backup are valid in the patched file byte for
//! byte. Resolve there, read here.

use crate::backup;
use crate::discovery::Install;
use crate::elf;
use crate::md5;
use crate::patch::{self, State};
use crate::resolve::{self, Report};
use crate::shellcode;
use crate::sites::{self, Action, Site};
use std::collections::HashMap;
use std::path::Path;

/// What one site is currently doing in the file on disk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SiteState {
    /// The bytes this tool writes are there.
    Applied,
    /// The bytes an untouched build has are there.
    Stock,
    /// Located, but holding something neither stock nor ours. Another tool,
    /// another version of this one, or a signature that found the wrong place.
    Foreign,
    /// Could not be located, so nothing can be said about it.
    Unknown,
    /// This build does not have the code, and does not need it.
    Excused,
}

impl SiteState {
    pub fn applied(self) -> bool {
        self == SiteState::Applied
    }
}

/// How a row reads at a glance. The colour is the whole point of the panel:
/// someone should be able to tell whether their audio is intact without
/// reading a word.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// The signal gets through untouched.
    Good,
    /// Works, but with a catch worth naming.
    Warn,
    /// Discord is still degrading this.
    Bad,
    /// Neither good nor bad — a setting that is a trade either way.
    Neutral,
    /// Not determinable on this build.
    Unknown,
}

/// One line of the readout.
pub struct Row {
    pub label: &'static str,
    pub value: String,
    pub level: Level,
    /// One sentence, for someone who does not know what any of these words
    /// mean. Shown under the panel for whichever row is highlighted.
    pub note: String,
}

/// The same shape the interface draws, for output that has no colour to lean
/// on. A pipe, a log, a bug report: the glyph has to carry the meaning alone.
pub fn plain_glyph(level: Level) -> &'static str {
    match level {
        Level::Good => "+",
        Level::Warn => "~",
        Level::Bad => "-",
        Level::Neutral => ".",
        Level::Unknown => "?",
    }
}

/// Where the offsets used to read the file came from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Baseline {
    /// Resolved against a stock backup of this exact build. Everything below
    /// can be read with confidence, patched or not.
    Backup,
    /// Resolved against the file itself. Fine while it is stock; once patched,
    /// most sites no longer match and read as unknown.
    SelfScan,
}

pub struct Inspection {
    pub size: u64,
    pub md5: String,
    pub symbols: Option<usize>,
    pub state: State,
    pub baseline: Baseline,
    /// The report to plan a patch from: against the backup where there is one,
    /// because that is the file a patch run actually starts from.
    pub report: Report,
    pub sites: HashMap<&'static str, SiteState>,
    pub rows: Vec<Row>,
    /// How many sites were located at all.
    pub located: usize,
    /// The bitrate the module currently carries, read back out of the file.
    pub bitrate_bps: Option<u32>,
    /// Whether that bitrate is the `opus_encoder_ctl` lock rather than merely
    /// the value the encoder starts from.
    pub bitrate_locked: bool,
}

impl Inspection {
    /// Whether this file looks like a Discord voice module at all.
    ///
    /// Nothing located means one of two things, and they are worth telling
    /// apart from "stock": a file that is not the module — someone pointed the
    /// path box at the wrong thing — or a build so far ahead of the catalogue
    /// that none of it applies. Reporting either as "untouched" would be a
    /// confident answer to a question that was never understood.
    pub fn recognised(&self) -> bool {
        self.located > 0
    }

    pub fn site(&self, name: &str) -> SiteState {
        self.sites.get(name).copied().unwrap_or(SiteState::Unknown)
    }

    /// How many of a group's sites are applied, out of how many this build can
    /// have at all.
    pub fn group_roll(&self, group: &str) -> (usize, usize) {
        let mine: Vec<&Site> = sites::SITES.iter().filter(|s| s.group == group).collect();
        let usable = mine
            .iter()
            .filter(|s| self.site(s.name) != SiteState::Excused)
            .count();
        let applied = mine.iter().filter(|s| self.site(s.name).applied()).count();
        (applied, usable)
    }

}

/// Read `data`, using `baseline` (a stock copy of the same build, if there is
/// one) to locate the sites.
pub fn inspect(data: &[u8], baseline: Option<&[u8]>) -> Inspection {
    let syms = elf::symbols(data);
    let live = resolve::resolve_all(data, syms.as_ref());
    let state = patch::classify(data, &live);

    // A backup of a different build would resolve to offsets that mean nothing
    // here. Equal length is a weak check on its own, but combined with the
    // per-site byte validation below — every read has to land on either stock
    // or patched bytes to count as either — a mismatched backup degrades to
    // "unknown" rather than to a confident lie.
    let (report, source) = match baseline.filter(|b| b.len() == data.len()) {
        Some(b) => {
            let bsyms = elf::symbols(b);
            (resolve::resolve_all(b, bsyms.as_ref()), Baseline::Backup)
        }
        None => (live, Baseline::SelfScan),
    };

    let mut states: HashMap<&'static str, SiteState> = HashMap::new();
    for site in sites::SITES {
        states.insert(site.name, read_site(data, site, &report));
    }

    let located = report
        .order
        .iter()
        .filter(|n| report.resolved(n).is_some())
        .count();

    // Two sites can carry a bitrate. The `opus_encoder_ctl` lock is the one
    // that decides, because it overrides both the value the config started with
    // and the one the server sends on joining, so prefer it when it is in.
    let applied = |name: &str| states.get(name) == Some(&SiteState::Applied);
    let bitrate_locked = applied("WebRtcOpus_SetBitRate");
    let bitrate_bps = if bitrate_locked {
        value_of(data, &report, "WebRtcOpus_SetBitRate")
    } else if applied("OpusConfig_Bitrate") {
        value_of(data, &report, "OpusConfig_Bitrate")
    } else {
        None
    }
    .and_then(|v| u32::try_from(v).ok())
    .filter(|v| *v > 0);

    let mut inspection = Inspection {
        size: data.len() as u64,
        md5: md5::hex(data),
        symbols: syms.as_ref().map(|s| s.len()),
        state,
        baseline: source,
        report,
        sites: states,
        rows: Vec::new(),
        located,
        bitrate_bps,
        bitrate_locked,
    };
    inspection.rows = rows(&inspection);
    inspection
}

/// Everything needed to inspect one install, backup included.
pub fn inspect_install(install: &Install, node: &Path) -> Result<Inspection, String> {
    let data = std::fs::read(node).map_err(|e| format!("cannot read {}: {e}", node.display()))?;
    let stock = std::fs::read(backup::path_for(install)).ok();
    Ok(inspect(&data, stock.as_deref()))
}

/// Inspect a module that is not part of a known install, so has no backup.
pub fn inspect_path(node: &Path) -> Result<Inspection, String> {
    let data = std::fs::read(node).map_err(|e| format!("cannot read {}: {e}", node.display()))?;
    Ok(inspect(&data, None))
}

fn at<'a>(data: &'a [u8], off: usize, len: usize) -> Option<&'a [u8]> {
    data.get(off..off + len)
}

/// Decide what one site currently holds.
///
/// Every arm has to be able to say "neither" — that is what keeps a wrong
/// backup, or a signature that landed somewhere unintended, from being
/// reported as a confident yes or no.
fn read_site(data: &[u8], site: &'static Site, report: &Report) -> SiteState {
    let Some(resolved) = report.resolved(site.name) else {
        return match report.excused(site.name) {
            Some(_) => SiteState::Excused,
            None => SiteState::Unknown,
        };
    };

    let mut applied = 0usize;
    let mut stock = 0usize;
    for &off in &resolved.offsets {
        match read_one(data, site, off) {
            SiteState::Applied => applied += 1,
            SiteState::Stock => stock += 1,
            _ => {}
        }
    }
    let n = resolved.offsets.len();
    if applied == n {
        SiteState::Applied
    } else if stock == n {
        SiteState::Stock
    } else if applied > 0 {
        // Half-applied is not a state this tool can produce: every plan is
        // written in one pass and verified. Say so rather than rounding it to
        // whichever side has more offsets.
        SiteState::Foreign
    } else {
        SiteState::Foreign
    }
}

fn read_one(data: &[u8], site: &'static Site, off: usize) -> SiteState {
    let looks_stock = |len: usize| -> bool {
        (!site.stock.is_empty() && at(data, off, site.stock.len()) == Some(site.stock))
            || site
                .expect_orig
                .iter()
                .any(|e| e.len() == len && at(data, off, e.len()) == Some(*e))
    };

    match site.action {
        Action::Bytes(b) => {
            if at(data, off, b.len()) == Some(b) {
                // A site whose patched bytes are also a legal stock value
                // cannot be told apart by bytes alone; `stock` is declared
                // exactly where that is not the case, so prefer it.
                if !site.stock.is_empty() && site.stock == b {
                    return SiteState::Stock;
                }
                return SiteState::Applied;
            }
            if looks_stock(b.len()) {
                return SiteState::Stock;
            }
            // Function-entry stubs have no declared stock bytes — the prologue
            // differs per build and the symbol is what found them. Anything
            // that is not our `ret` is the original.
            if site.entry && site.stock.is_empty() && site.expect_orig.is_empty() {
                return SiteState::Stock;
            }
            SiteState::Foreign
        }
        Action::BitrateImm32 => match imm32(data, off) {
            Some(_) if looks_stock(4) => SiteState::Stock,
            Some(_) => SiteState::Applied,
            None => SiteState::Foreign,
        },
        Action::BitrateSetter | Action::CtlArg(_) => {
            if at(data, off, 2) == Some(&[0x55, 0xBA]) {
                SiteState::Applied
            } else if looks_stock(6) {
                SiteState::Stock
            } else {
                SiteState::Foreign
            }
        }
        Action::ShellcodeHpCutoff | Action::ShellcodeDcReject => {
            let marker = match site.action {
                Action::ShellcodeHpCutoff => shellcode::hp_cutoff_marker(),
                _ => shellcode::dc_reject_marker(),
            };
            if at(data, off, marker.len()) == Some(marker.as_slice()) {
                SiteState::Applied
            } else {
                SiteState::Stock
            }
        }
        Action::RipRelF32(v) => match patch::rip_target(data, off).and_then(|t| f32_at(data, t)) {
            Some(cur) if cur == v => SiteState::Applied,
            Some(_) => SiteState::Stock,
            None => SiteState::Foreign,
        },
    }
}

fn imm32(data: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(at(data, off, 4)?.try_into().ok()?))
}

fn f32_at(data: &[u8], off: usize) -> Option<f32> {
    Some(f32::from_le_bytes(at(data, off, 4)?.try_into().ok()?))
}

/// The value a site currently carries, where it carries one.
fn value_of(data: &[u8], report: &Report, name: &str) -> Option<i64> {
    let site = sites::find(name)?;
    let off = *report.resolved(name)?.offsets.first()?;
    match site.action {
        Action::BitrateImm32 => imm32(data, off).map(|v| v as i64),
        Action::BitrateSetter | Action::CtlArg(_) => {
            if at(data, off, 2) == Some(&[0x55, 0xBA]) {
                imm32(data, off + 2).map(|v| v as i32 as i64)
            } else {
                None
            }
        }
        Action::Bytes(_) => imm32(data, off).map(|v| v as i64),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The readout
// ---------------------------------------------------------------------------

fn row(label: &'static str, level: Level, value: impl Into<String>, note: impl Into<String>) -> Row {
    Row { label, value: value.into(), level, note: note.into() }
}

/// Is this site doing its job? `None` means the file could not be read there,
/// which is a third answer and not a synonym for "no".
///
/// Keeping that distinction is the whole discipline of this module. A patched
/// module with no backup hides most of its own evidence, and the tempting
/// shortcut — treat unreadable as unpatched — turns a fully patched install
/// into a panel of red warnings about things that are in fact fine.
fn tri(i: &Inspection, name: &str) -> Option<bool> {
    match i.site(name) {
        // A build that does not have the code has nothing left to do here.
        SiteState::Applied | SiteState::Excused => Some(true),
        SiteState::Stock | SiteState::Foreign => Some(false),
        SiteState::Unknown => None,
    }
}

/// As [`tri`], but "this build does not have it" reads as unreadable rather
/// than as done. Used where the excuse is conditional on another site actually
/// being applied, which resolving alone does not establish.
fn tri_strict(i: &Inspection, name: &str) -> Option<bool> {
    match i.site(name) {
        SiteState::Applied => Some(true),
        SiteState::Stock | SiteState::Foreign => Some(false),
        SiteState::Unknown | SiteState::Excused => None,
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Verdict {
    /// Everything that had to be in place is.
    Yes,
    /// Nothing is.
    No,
    /// Some is and some demonstrably is not.
    Partial,
    /// Too much of it could not be read to say.
    Unclear,
}

fn combine(readings: &[Option<bool>]) -> Verdict {
    let yes = readings.iter().filter(|r| **r == Some(true)).count();
    let no = readings.iter().filter(|r| **r == Some(false)).count();
    match (yes, no, readings.len()) {
        (_, _, 0) => Verdict::Unclear,
        (y, 0, n) if y == n => Verdict::Yes,
        (0, x, n) if x == n => Verdict::No,
        (y, x, _) if y > 0 && x > 0 => Verdict::Partial,
        // Only unknowns left over: something is unaccounted for either way.
        (_, x, _) if x > 0 => Verdict::Partial,
        _ => Verdict::Unclear,
    }
}

fn verdict(i: &Inspection, names: &[&str]) -> Verdict {
    let readings: Vec<Option<bool>> = names.iter().map(|n| tri(i, n)).collect();
    combine(&readings)
}

fn group_verdict(i: &Inspection, group: &str) -> Verdict {
    let names: Vec<&str> = sites::SITES
        .iter()
        .filter(|s| s.group == group)
        .map(|s| s.name)
        .collect();
    verdict(i, &names)
}

/// "a, b and c" — the note is prose, and a bare comma list reads as a dump.
fn join_and(items: &[&str]) -> String {
    match items {
        [] => "nothing".to_string(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

const UNREADABLE: &str =
    "This part of the module could not be read. Restoring the original and scanning \
     again will say for certain.";

fn rows(i: &Inspection) -> Vec<Row> {
    let mut out = Vec::new();

    // --- stereo ------------------------------------------------------------
    //
    // Three separate things have to be true, and they fail in ways that look
    // identical from the outside. The offer has to advertise stereo, the
    // capture path has to stop folding the two channels together, and the
    // network adaptor has to be stopped from undoing both partway through a
    // call. The third is why this row is not a yes or a no: a client missing
    // only that starts every call in stereo and collapses to mono the first
    // time the uplink estimate dips, which sounds like the patch wearing off
    // rather than like a setting.
    let core: Vec<&str> = sites::SITES
        .iter()
        .filter(|s| s.group == "stereo" && s.critical)
        .map(|s| s.name)
        .collect();
    let held = tri(i, "ChannelController_ForceStereo");
    out.push(match (verdict(i, &core), held) {
        (Verdict::Yes, Some(true)) => row(
            "True stereo",
            Level::Good,
            "working, and held for the whole call",
            "Left and right go out as two separate channels, and Discord cannot quietly \
             fold them back together when the network dips.",
        ),
        (Verdict::Yes, Some(false)) => row(
            "True stereo",
            Level::Warn,
            "working, but can collapse mid-call",
            "Calls start in stereo, but Discord is still allowed to drop back to one \
             channel when it thinks your connection is struggling. It does not come back \
             on its own.",
        ),
        (Verdict::Yes, None) => row(
            "True stereo",
            Level::Good,
            "working",
            "Left and right go out as two separate channels. Whether Discord can still \
             drop back to mono partway through a call could not be read here.",
        ),
        (Verdict::Partial, _) => row(
            "True stereo",
            Level::Bad,
            "partly patched",
            "Some of the stereo changes are in place and some are not, which usually \
             means stereo is negotiated and then sent as one channel. Patching again \
             fixes it.",
        ),
        (Verdict::No, _) => row(
            "True stereo",
            Level::Bad,
            "off — you are sending mono",
            "Discord mixes your left and right together before anyone hears them.",
        ),
        (Verdict::Unclear, _) => row("True stereo", Level::Unknown, "cannot tell", UNREADABLE),
    });

    // --- sample rate -------------------------------------------------------
    out.push(match tri(i, "SelectSampleRate_48k") {
        Some(true) => row(
            "Sample rate",
            Level::Good,
            "48 kHz",
            "The full sample rate is kept, so the top of the treble survives.",
        ),
        Some(false) => row(
            "Sample rate",
            Level::Bad,
            "32 kHz",
            "Discord resamples you down to 32 kHz, which throws away everything above \
             about 16 kHz.",
        ),
        None => row("Sample rate", Level::Unknown, "cannot tell", UNREADABLE),
    });

    // --- bitrate -----------------------------------------------------------
    out.push(match (i.bitrate_bps, tri(i, "WebRtcOpus_SetBitRate")) {
        (Some(v), _) => row(
            "Bitrate",
            Level::Good,
            format!(
                "{} kbps{}",
                v / 1000,
                if i.bitrate_locked { "" } else { " (starting value only)" }
            ),
            if i.bitrate_locked {
                "Every request to change the bitrate is overridden with this value, \
                 including the one the server sends when you join a channel."
            } else {
                "The encoder starts here, but Discord can still talk it down once you are \
                 in a channel."
            },
        ),
        (None, None) => row("Bitrate", Level::Unknown, "cannot tell", UNREADABLE),
        (None, _) => row(
            "Bitrate",
            Level::Bad,
            "whatever Discord asks for",
            "Normally 64 kbps, or up to 384 in a boosted server — and Discord lowers it \
             again whenever it decides the connection needs it to.",
        ),
    });

    // --- encoder mode ------------------------------------------------------
    out.push(match (tri(i, "OpusConfig_Application"), tri(i, "OpusConfig_IsOk")) {
        (Some(true), Some(true)) => row(
            "Encoder mode",
            Level::Good,
            "music",
            "The encoder is told it is handling music rather than a phone call, and it \
             accepts the settings above instead of rejecting them.",
        ),
        (Some(true), _) => row(
            "Encoder mode",
            Level::Warn,
            "music, but settings may be rejected",
            "The encoder is in music mode, but the check that would otherwise throw out a \
             high bitrate or a second channel is still in place.",
        ),
        (Some(false), _) => row(
            "Encoder mode",
            Level::Bad,
            "voice call",
            "The encoder is tuned for speech: it narrows the stereo image and spends its \
             bitrate on intelligibility rather than on fidelity.",
        ),
        (None, _) => row("Encoder mode", Level::Unknown, "cannot tell", UNREADABLE),
    });

    // --- celt --------------------------------------------------------------
    out.push(match group_verdict(i, "celt") {
        Verdict::Yes => row(
            "Coding mode",
            Level::Good,
            "locked to music",
            "The encoder cannot switch to its speech mode partway through, which would \
             fold the stereo image down and cut the highs.",
        ),
        Verdict::Partial => row(
            "Coding mode",
            Level::Warn,
            "partly locked",
            "Only some of the mode locks are in place, so the encoder may still switch to \
             speech mode.",
        ),
        Verdict::No => row(
            "Coding mode",
            Level::Bad,
            "encoder's choice",
            "The encoder can drop into speech mode on its own, which narrows the stereo \
             image and cuts the top end.",
        ),
        Verdict::Unclear => row("Coding mode", Level::Unknown, "cannot tell", UNREADABLE),
    });

    // --- the low cut -------------------------------------------------------
    //
    // Two filters, each of which can be out of the path more than one way: the
    // function replaced, its coefficient zeroed, or the build unable to reach
    // it at all. The last of those is conditional — libopus skips `hp_cutoff`
    // only in the mode the application patch selects — so it counts only when
    // that patch is actually applied, not merely when its site was found.
    let spl = tri(i, "SplHighPass_Entry");
    let opus_hp = match i.site("HpCutoff_Inject") {
        SiteState::Applied => Some(true),
        SiteState::Excused => tri(i, "OpusConfig_Application"),
        SiteState::Stock | SiteState::Foreign => Some(false),
        SiteState::Unknown => None,
    };
    let dc = match (
        tri_strict(i, "DcReject_Coefficient"),
        tri_strict(i, "DcReject_Inject"),
    ) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (None, None) => None,
        (a, b) if a == Some(false) || b == Some(false) => Some(false),
        _ => None,
    };
    out.push(match combine(&[spl, opus_hp, dc]) {
        Verdict::Yes => row(
            "Low cut (high-pass)",
            Level::Good,
            "bypassed",
            "Nothing removes the low end of your signal before it is encoded. Bass, room \
             tone and the body of instruments stay in.",
        ),
        Verdict::Partial => row(
            "Low cut (high-pass)",
            Level::Warn,
            "partly bypassed",
            "One of the low-cut filters is still in the path, so some of the bottom end \
             is still being removed.",
        ),
        Verdict::No => row(
            "Low cut (high-pass)",
            Level::Bad,
            "active",
            "Discord filters the low end out of your signal on the way in, which thins \
             out anything with bass in it.",
        ),
        Verdict::Unclear => row("Low cut (high-pass)", Level::Unknown, "cannot tell", UNREADABLE),
    });

    // --- the high cut ------------------------------------------------------
    out.push(match tri(i, "WebRtcOpus_Bandwidth_Fullband") {
        Some(true) => row(
            "High cut (bandwidth)",
            Level::Good,
            "fullband, 20 kHz",
            "The encoder has to code the whole audible range instead of narrowing it when \
             it wants to save data.",
        ),
        Some(false) => row(
            "High cut (bandwidth)",
            Level::Bad,
            "encoder's choice",
            "The encoder narrows the top end on its own when it decides the connection \
             cannot carry it.",
        ),
        None => row("High cut (bandwidth)", Level::Unknown, "cannot tell", UNREADABLE),
    });

    // --- the rest of the encoder locks -------------------------------------
    {
        const LOCKS: &[(&str, &str)] = &[
            ("WebRtcOpus_Fec_Off", "error correction"),
            ("WebRtcOpus_Dtx_Off", "silence skipping"),
            ("WebRtcOpus_PacketLoss_Zero", "loss padding"),
            ("WebRtcOpus_Complexity_Max", "the quality dial"),
            ("FrameLength_Pin", "frame length"),
        ];
        let names: Vec<&str> = LOCKS.iter().map(|(n, _)| *n).collect();
        let loose: Vec<&str> = LOCKS
            .iter()
            .filter(|(n, _)| tri(i, n) == Some(false))
            .map(|(_, label)| *label)
            .collect();
        out.push(match verdict(i, &names) {
            Verdict::Yes => row(
                "Encoder held open",
                Level::Good,
                "yes",
                "Quality is pinned to maximum, and none of your bitrate is spent on error \
                 correction, silence detection or shorter frames when the connection dips.",
            ),
            Verdict::Unclear => row("Encoder held open", Level::Unknown, "cannot tell", UNREADABLE),
            // The list of what is still loose goes in the note, not in the
            // value: it runs to five items, and a value column that overflows
            // its pane is worse than one that says less.
            v => row(
                "Encoder held open",
                if v == Verdict::No { Level::Bad } else { Level::Warn },
                if v == Verdict::No { "no".to_string() } else { "only partly".to_string() },
                format!(
                    "Discord still decides {}. It is free to spend part of your bitrate \
                     on those rather than on the sound itself whenever it decides the \
                     network needs it.",
                    join_and(&loose)
                ),
            ),
        });
    }

    // --- what happens before the encoder -----------------------------------
    for (group, label, on_note, off_note) in [
        (
            "gain",
            "Automatic volume",
            "Your level goes out the way you set it. Nothing rides it up and down, and \
             nothing moves your system input slider behind your back.",
            "Discord adjusts your volume for you. On music this is audible as pumping: \
             quiet parts pushed up, loud parts pulled down.",
        ),
        (
            "denoise",
            "Noise removal",
            "Nothing is being deleted from your signal. Room tone, reverb tails and \
             fade-outs survive.",
            "Noise suppression is running. It is tuned for speech and treats quiet detail \
             as noise to be removed.",
        ),
        (
            "echo",
            "Echo cancellation",
            "The echo canceller is out of the path. It is the most destructive thing in \
             the chain for music — but you need headphones now.",
            "Discord is subtracting a guess at what your speakers are playing from what \
             your microphone hears, and guessing wrong on music.",
        ),
    ] {
        out.push(match group_verdict(i, group) {
            Verdict::Yes => row(label, Level::Good, "off", on_note),
            Verdict::Partial => row(
                label,
                Level::Warn,
                "partly off",
                "Some of this is switched off and some is not, so it is still touching \
                 your signal.",
            ),
            Verdict::No => row(label, Level::Bad, "on", off_note),
            Verdict::Unclear => row(label, Level::Unknown, "cannot tell", UNREADABLE),
        });
    }

    // --- cbr, which is a trade rather than a win ---------------------------
    out.push(match tri(i, "WebRtcOpus_Cbr_Always") {
        Some(true) => row(
            "Constant bitrate",
            Level::Neutral,
            "forced on",
            "The same amount of data goes out every moment, including during silence. \
             Steadier on the network, not better sounding.",
        ),
        Some(false) => row(
            "Constant bitrate",
            Level::Neutral,
            "off",
            "The encoder eases off during quiet passages, which is the normal and \
             slightly more efficient behaviour.",
        ),
        None => row("Constant bitrate", Level::Neutral, "cannot tell", UNREADABLE),
    });

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(name: &str) -> &'static Site {
        sites::find(name).unwrap()
    }

    #[test]
    fn a_stock_byte_reads_as_stock_and_a_patched_one_as_applied() {
        // CommitAudioCodec_StereoCheck writes 0x00 over a stock 0x02.
        let s = site("CommitAudioCodec_StereoCheck");
        assert_eq!(read_one(&[0x02], s, 0), SiteState::Stock);
        assert_eq!(read_one(&[0x00], s, 0), SiteState::Applied);
        assert_eq!(read_one(&[0x7F], s, 0), SiteState::Foreign);
    }

    #[test]
    fn an_entry_stub_is_applied_only_when_it_is_a_ret() {
        let s = site("GainController2_Bypass");
        assert_eq!(read_one(&[0xC3], s, 0), SiteState::Applied);
        // A push rbp prologue is simply the untouched function.
        assert_eq!(read_one(&[0x55, 0x48, 0x89, 0xE5], s, 0), SiteState::Stock);
    }

    #[test]
    fn a_ctl_lock_is_read_back_through_its_rewritten_prologue() {
        let s = site("WebRtcOpus_SetBitRate");
        let stock = [0x55, 0x48, 0x89, 0xE5, 0x89, 0xF2];
        assert_eq!(read_one(&stock, s, 0), SiteState::Stock);
        // push rbp ; mov edx, 248000
        let mut patched = vec![0x55, 0xBA];
        patched.extend_from_slice(&248_000u32.to_le_bytes());
        assert_eq!(read_one(&patched, s, 0), SiteState::Applied);
    }

    #[test]
    fn reading_past_the_end_is_not_a_panic() {
        let s = site("WebRtcOpus_SetBitRate");
        assert_eq!(read_one(&[0x55], s, 0), SiteState::Foreign);
        assert_eq!(read_one(&[], s, 4096), SiteState::Foreign);
    }

    #[test]
    fn every_row_says_something() {
        // Whatever the state, no row may be blank — the panel is the whole
        // point and an empty cell reads as a bug.
        let empty = Inspection {
            size: 0,
            md5: String::new(),
            symbols: None,
            state: State::Stock,
            baseline: Baseline::SelfScan,
            report: resolve::resolve_all(&[], None),
            sites: HashMap::new(),
            rows: Vec::new(),
            located: 0,
            bitrate_bps: None,
            bitrate_locked: false,
        };
        let rows = rows(&empty);
        assert!(!rows.is_empty());
        for r in &rows {
            assert!(!r.value.is_empty(), "{} has no value", r.label);
            assert!(!r.note.is_empty(), "{} has no note", r.label);
        }
    }
}
