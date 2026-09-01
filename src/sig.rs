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

//! Byte-signature scanning with single-byte wildcards.
//!
//! Patterns are written the way a disassembler prints them: space separated hex
//! bytes, with `??` marking a byte whose value varies between builds (relative
//! call/jump displacements, struct offsets, register-encoding nibbles).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tok {
    Byte(u8),
    Any,
}

#[derive(Debug, Clone)]
pub struct Pattern {
    pub toks: Vec<Tok>,
    /// Byte offset, from the start of a match, of the region this pattern is
    /// used to locate. Lets one pattern anchor on surrounding context while
    /// pointing at an immediate buried in the middle of it.
    pub delta: usize,
    pub source: &'static str,
}

impl Pattern {
    pub fn parse(src: &'static str, delta: usize) -> Pattern {
        let mut toks = Vec::new();
        for tok in src.split_whitespace() {
            if tok == "??" {
                toks.push(Tok::Any);
            } else {
                let b = u8::from_str_radix(tok, 16)
                    .unwrap_or_else(|_| panic!("bad signature byte {tok:?} in {src:?}"));
                toks.push(Tok::Byte(b));
            }
        }
        assert!(!toks.is_empty(), "empty signature: {src:?}");
        assert!(delta < toks.len(), "delta out of range for {src:?}");
        Pattern { toks, delta, source: src }
    }

    fn matches_at(&self, hay: &[u8], at: usize) -> bool {
        for (i, t) in self.toks.iter().enumerate() {
            match *t {
                Tok::Any => {}
                Tok::Byte(b) => {
                    if hay[at + i] != b {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// All positions where this pattern matches, already adjusted by `delta`.
    pub fn find_all(&self, hay: &[u8]) -> Vec<usize> {
        let n = self.toks.len();
        if hay.len() < n {
            return Vec::new();
        }

        // Anchor the search on the first concrete byte so the common case is a
        // memchr-style skip rather than a full comparison at every position.
        let (skip, anchor) = match self.toks.iter().position(|t| matches!(t, Tok::Byte(_))) {
            Some(i) => match self.toks[i] {
                Tok::Byte(b) => (i, b),
                Tok::Any => unreachable!(),
            },
            // A pattern of nothing but wildcards would match everywhere; the
            // site catalogue never contains one, but do not spin on it either.
            None => return Vec::new(),
        };

        let mut out = Vec::new();
        let last = hay.len() - n;
        let mut i = skip;
        while i <= last + skip {
            match hay[i..=(last + skip)].iter().position(|&b| b == anchor) {
                None => break,
                Some(rel) => {
                    let start = i + rel - skip;
                    if self.matches_at(hay, start) {
                        out.push(start + self.delta);
                    }
                    i = i + rel + 1;
                }
            }
        }
        out
    }
}

/// Render bytes the way the rest of the tool prints them.
pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards_match_any_byte() {
        let p = Pattern::parse("AA ?? CC", 0);
        assert_eq!(p.find_all(&[0xAA, 0x00, 0xCC]), vec![0]);
        assert_eq!(p.find_all(&[0xAA, 0xFF, 0xCC]), vec![0]);
        assert!(p.find_all(&[0xAA, 0xFF, 0xCD]).is_empty());
    }

    #[test]
    fn delta_points_inside_the_match() {
        let p = Pattern::parse("AA BB CC", 2);
        assert_eq!(p.find_all(&[0x00, 0xAA, 0xBB, 0xCC]), vec![3]);
    }

    #[test]
    fn finds_every_occurrence() {
        let p = Pattern::parse("AA BB", 0);
        assert_eq!(p.find_all(&[0xAA, 0xBB, 0xAA, 0xBB]), vec![0, 2]);
    }

    #[test]
    fn overlapping_matches_are_all_reported() {
        let p = Pattern::parse("AA AA", 0);
        assert_eq!(p.find_all(&[0xAA, 0xAA, 0xAA]), vec![0, 1]);
    }

    #[test]
    fn leading_wildcards_do_not_stall_the_scan() {
        // The site catalogue anchors one pattern on trailing context only.
        let p = Pattern::parse("?? ?? CC DD", 0);
        assert_eq!(p.find_all(&[0x01, 0x02, 0xCC, 0xDD]), vec![0]);
    }

    #[test]
    fn no_match_in_short_input() {
        let p = Pattern::parse("AA BB CC", 0);
        assert!(p.find_all(&[0xAA, 0xBB]).is_empty());
    }
}
