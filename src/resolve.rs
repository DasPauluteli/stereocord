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

//! Turning the site catalogue into concrete file offsets for one binary.

use crate::elf::Symbols;
use crate::sites::{self, Expect, Site};
use std::collections::HashMap;

/// How a site's offset was arrived at. Reported by `scan -v`, because "found
/// by name" and "found by pattern" carry very different confidence.
#[derive(Clone)]
pub enum Via {
    /// The symbol's own address.
    SymbolEntry(&'static str),
    /// A pattern, searched only inside the named function.
    InSymbol(&'static str),
    /// A pattern, searched across the whole file.
    Scan,
}

/// Mangled C++ names run to 100+ characters; the report only needs enough to
/// tell which function was used.
fn short(name: &str) -> String {
    if name.len() <= 42 {
        name.to_string()
    } else {
        format!("{}...", &name[..39])
    }
}

impl std::fmt::Display for Via {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Via::SymbolEntry(n) => write!(f, "symbol {}", short(n)),
            Via::InSymbol(n) => write!(f, "sig in {}", short(n)),
            Via::Scan => write!(f, "scan"),
        }
    }
}

pub struct Resolved {
    pub site: &'static Site,
    pub offsets: Vec<usize>,
    /// Which of the site's alternative encodings matched.
    pub variant: usize,
    #[allow(dead_code)]
    pub pattern: &'static str,
    pub via: Via,
}

pub enum Outcome {
    Found(Resolved),
    /// No alternative matched anywhere.
    Missing,
    /// Matched, but not the number of times the site requires.
    Ambiguous { found: usize, wanted: String },
}

pub struct Report {
    pub order: Vec<&'static str>,
    pub outcomes: HashMap<&'static str, Outcome>,
}

impl Report {
    pub fn resolved(&self, name: &str) -> Option<&Resolved> {
        match self.outcomes.get(name) {
            Some(Outcome::Found(r)) => Some(r),
            _ => None,
        }
    }

    pub fn missing(&self) -> Vec<&'static str> {
        self.order
            .iter()
            .copied()
            .filter(|n| self.resolved(n).is_none())
            .collect()
    }

}

/// Locate every site in `data`.
///
/// Sites whose disambiguation depends on another site are deferred until that
/// anchor has been resolved, so ordering inside the catalogue does not matter.
pub fn resolve_all(data: &[u8], syms: Option<&Symbols>) -> Report {
    let mut outcomes: HashMap<&'static str, Outcome> = HashMap::new();
    let order: Vec<&'static str> = sites::SITES.iter().map(|s| s.name).collect();

    let mut pending: Vec<&'static Site> = sites::SITES.iter().collect();
    // Two passes is enough: no site anchors on a site that itself anchors on
    // another. A third pass would be needed only if that ever changed, and the
    // loop below would still terminate because it stops when nothing moves.
    loop {
        let before = pending.len();
        let mut still: Vec<&'static Site> = Vec::new();

        for site in pending {
            if let Expect::NearestAfter { anchor, .. } = site.expect {
                if !matches!(outcomes.get(anchor), Some(Outcome::Found(_))) {
                    // Anchor not resolved yet (or unresolvable); retry later.
                    if outcomes.contains_key(anchor) {
                        outcomes.insert(site.name, Outcome::Missing);
                    } else {
                        still.push(site);
                    }
                    continue;
                }
            }
            let anchor_at = match site.expect {
                Expect::NearestAfter { anchor, .. } => match outcomes.get(anchor) {
                    Some(Outcome::Found(r)) => r.offsets.first().copied(),
                    _ => None,
                },
                _ => None,
            };
            outcomes.insert(site.name, resolve_one(data, syms, site, anchor_at));
        }

        pending = still;
        if pending.is_empty() || pending.len() == before {
            for site in pending {
                outcomes.insert(site.name, Outcome::Missing);
            }
            break;
        }
    }

    Report { order, outcomes }
}

fn resolve_one(
    data: &[u8],
    syms: Option<&Symbols>,
    site: &'static Site,
    anchor_at: Option<usize>,
) -> Outcome {
    // Prefer the symbol table. Scoping the search to one function turns a
    // whole-file scan that might match twice into one that cannot.
    if let Some(syms) = syms {
        for name in site.symbols {
            let Some(sym) = syms.get(name) else { continue };
            if site.entry {
                return Outcome::Found(Resolved {
                    site,
                    offsets: vec![sym.offset],
                    variant: 0,
                    pattern: "<function entry>",
                    via: Via::SymbolEntry(name),
                });
            }
            let end = (sym.offset + sym.size.max(1)).min(data.len());
            if sym.offset >= end {
                continue;
            }
            let scoped = &data[sym.offset..end];
            if let Some(mut found) = search(scoped, site, anchor_at.map(|a| a.wrapping_sub(sym.offset))) {
                for o in &mut found.offsets {
                    *o += sym.offset;
                }
                found.via = Via::InSymbol(name);
                return Outcome::Found(found);
            }
        }
    }

    match search(data, site, anchor_at) {
        Some(found) => Outcome::Found(found),
        None => scan_failure(data, site, anchor_at),
    }
}

/// Try each encoding against `hay`, returning the first that matches the
/// expected number of times.
fn search(hay: &[u8], site: &'static Site, anchor_at: Option<usize>) -> Option<Resolved> {
    let patterns = sites::compile_patterns(site);
    let wanted = match site.expect {
        Expect::All(n) => n,
        _ => 1,
    };

    for (variant, pat) in patterns.iter().enumerate() {
        let mut hits = pat.find_all(hay);
        if hits.is_empty() {
            continue;
        }
        if let Expect::NearestAfter { window, .. } = site.expect {
            let Some(base) = anchor_at else { continue };
            hits.retain(|&h| h > base && h - base <= window);
        }
        if hits.len() == wanted {
            return Some(Resolved {
                site,
                offsets: hits,
                variant,
                pattern: pat.source,
                via: Via::Scan,
            });
        }
    }
    None
}

/// Describe why a whole-file scan came up short, so the report can distinguish
/// "this build moved the code" from "this pattern is no longer specific".
fn scan_failure(data: &[u8], site: &'static Site, anchor_at: Option<usize>) -> Outcome {
    let wanted = match site.expect {
        Expect::All(n) => n,
        _ => 1,
    };
    for pat in sites::compile_patterns(site) {
        let mut hits = pat.find_all(data);
        if hits.is_empty() {
            continue;
        }
        if let Expect::NearestAfter { window, .. } = site.expect {
            match anchor_at {
                Some(base) => hits.retain(|&h| h > base && h - base <= window),
                None => continue,
            }
        }
        if !hits.is_empty() {
            return Outcome::Ambiguous { found: hits.len(), wanted: wanted.to_string() };
        }
    }
    Outcome::Missing
}
