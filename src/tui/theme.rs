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

//! One place for every colour and glyph, so the screens stay consistent.
//!
//! The palette is 256-colour rather than truecolour: the muted shades survive
//! terminals that remap the sixteen named colours to a theme, and nothing here
//! needs a precision that 256 entries cannot give. Red and green carry real
//! meaning in this tool — red is "Discord is still degrading your audio" — so
//! they are picked to stay legible rather than to be loud.

use crate::status::Level;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders, Padding};
use ratatui::text::Span;

pub const ACCENT: Color = Color::Indexed(74); // steel blue
pub const GOOD: Color = Color::Indexed(114); // sage green
pub const BAD: Color = Color::Indexed(210); // clay red
pub const WARN: Color = Color::Indexed(179); // amber
pub const MUTED: Color = Color::Indexed(245);
pub const FAINT: Color = Color::Indexed(240);
pub const DANGER: Color = Color::Indexed(203);

pub fn level_color(level: Level) -> Color {
    match level {
        Level::Good => GOOD,
        Level::Warn => WARN,
        Level::Bad => BAD,
        Level::Neutral => MUTED,
        Level::Unknown => FAINT,
    }
}

/// A filled dot for anything decided, a hollow one for anything not. The shape
/// carries the same information as the colour, which matters on a terminal
/// that has been themed into uselessness — or to a colour-blind reader.
pub fn level_glyph(level: Level) -> &'static str {
    match level {
        Level::Good => "●",
        Level::Warn => "◐",
        Level::Bad => "●",
        Level::Neutral => "○",
        Level::Unknown => "·",
    }
}

pub fn dot(level: Level) -> Span<'static> {
    Span::styled(level_glyph(level), Style::default().fg(level_color(level)))
}

pub fn muted(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(MUTED))
}

pub fn faint(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(FAINT))
}

pub fn plain(text: impl Into<String>) -> Span<'static> {
    Span::raw(text.into())
}

pub fn accent(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(ACCENT))
}

pub fn coloured(text: impl Into<String>, colour: Color) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(colour))
}

/// A key in the footer, and what it does.
pub fn key_hint(key: &str, what: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            key.to_string(),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {what}  "), Style::default().fg(MUTED)),
    ]
}

/// A titled frame. `focused` brightens the border, which is the only cue that
/// says which pane the arrow keys are talking to.
pub fn panel(title: &str, focused: bool) -> Block<'static> {
    let border = if focused { ACCENT } else { FAINT };
    let title_style = if focused {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(MUTED)
    };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(Span::styled(format!(" {title} "), title_style))
}

/// A frame for prose rather than for a list. Lists draw their own leading
/// column, so only this variant insets the text away from the border.
pub fn text_panel(title: &str, focused: bool) -> Block<'static> {
    panel(title, focused).padding(Padding::horizontal(1))
}

/// The cursor column in every list: an arrow for the current row, blank
/// otherwise, always the same width so nothing shifts as it moves.
pub fn cursor(selected: bool, enabled: bool) -> Span<'static> {
    match (selected, enabled) {
        (true, true) => Span::styled(" ▸ ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        (true, false) => Span::styled(" ▸ ", Style::default().fg(FAINT)),
        _ => Span::raw("   "),
    }
}

/// Style for a row's text, given whether it is the cursor row and whether it
/// can be chosen at all.
pub fn row_style(selected: bool, enabled: bool) -> Style {
    match (selected, enabled) {
        (_, false) => Style::default().fg(FAINT),
        (true, true) => Style::default().add_modifier(Modifier::BOLD),
        (false, true) => Style::default(),
    }
}

/// Pad `s` to `width` display columns. Used for the label column in tables,
/// where the alternative is a `{:<n}` that counts bytes and so mis-aligns the
/// moment a glyph outside ASCII appears.
pub fn pad(s: &str, width: usize) -> String {
    let n = unicode_width(s);
    if n >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - n))
    }
}

/// Close enough for this tool's strings: everything drawn is either ASCII or
/// one of a handful of single-width symbols, so counting characters is right.
fn unicode_width(s: &str) -> usize {
    s.chars().count()
}

/// Human-readable byte count, for module and backup sizes.
pub fn bytes(n: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / MB)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_reaches_the_requested_width() {
        assert_eq!(pad("ab", 5), "ab   ");
        assert_eq!(pad("abcdef", 3), "abcdef");
        assert_eq!(pad("●", 3), "●  ");
    }

    #[test]
    fn sizes_read_as_megabytes_once_they_are_large() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(121_075_464), "115.5 MB");
    }
}
