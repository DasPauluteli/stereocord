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

//! Drawing. Every screen is a pure function of [`App`]; nothing here decides
//! anything or touches a file.
//!
//! The shape is the same throughout — a breadcrumb line, a body, and a line of
//! key hints — so that moving between screens never moves the furniture. Where
//! a screen has two panes, the left one is the list and the right one explains
//! whatever the cursor is on, because every choice in this tool needs a
//! sentence that does not assume the reader knows what Opus is.

use super::theme::{self, ACCENT, BAD, DANGER, FAINT, GOOD, MUTED, WARN};
use super::{App, Focus, Kind, Screen, ACTIONS};
use crate::sites;
use crate::status::{Baseline, Inspection, Level};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Clear, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};
use ratatui::Frame;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn draw(f: &mut Frame, app: &App) {
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(f.area());

    header(f, app, rows[0]);
    match app.screen {
        Screen::Targets => targets(f, app, rows[1]),
        Screen::AskPath => ask_path(f, app, rows[1]),
        Screen::Overview => overview(f, app, rows[1]),
        Screen::Patch => patch(f, app, rows[1]),
        Screen::Manage => manage(f, app, rows[1]),
        Screen::Result => result(f, app, rows[1]),
    }
    footer(f, app, rows[2]);

    if let Some(_m) = &app.modal {
        modal(f, app);
    }
}

// ---------------------------------------------------------------------------
// Chrome
// ---------------------------------------------------------------------------

fn header(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![
        Span::styled(
            " stereocord ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        theme::faint(format!("v{}  ", crate::VERSION)),
    ];
    if let Some(target) = app.target() {
        spans.push(theme::faint("›  "));
        spans.push(theme::plain(target.label.clone()));
    }
    let leaf = match app.screen {
        Screen::Patch => Some("Patch"),
        Screen::Manage => Some("Manage"),
        Screen::AskPath => Some("Module by path"),
        Screen::Result => Some("Result"),
        _ => None,
    };
    if let Some(leaf) = leaf {
        spans.push(theme::faint("  ›  "));
        spans.push(theme::accent(leaf));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn footer(f: &mut Frame, app: &App, area: Rect) {
    if app.modal.is_some() {
        return hints(f, area, &[("←→", "choose"), ("enter", "go"), ("esc", "cancel")]);
    }
    if app.busy.is_some() {
        return hints(f, area, &[("ctrl-c", "quit")]);
    }
    match app.screen {
        Screen::Targets => hints(
            f,
            area,
            &[("↑↓", "move"), ("enter", "open"), ("r", "refresh"), ("q", "quit")],
        ),
        Screen::AskPath => hints(
            f,
            area,
            &[("tab", "complete"), ("enter", "open"), ("esc", "back")],
        ),
        Screen::Overview => hints(
            f,
            area,
            &[
                ("↑↓", "move"),
                ("tab", "pane"),
                ("enter", "choose"),
                ("p", "patch"),
                ("m", "manage"),
                ("r", "re-read"),
                ("esc", "back"),
            ],
        ),
        Screen::Patch => hints(
            f,
            area,
            &[
                ("↑↓", "move"),
                ("space", "toggle"),
                ("←→", "bitrate"),
                ("a/n/d", "all/none/defaults"),
                ("enter", "apply"),
                ("esc", "back"),
            ],
        ),
        Screen::Manage => hints(f, area, &[("↑↓", "move"), ("enter", "choose"), ("esc", "back")]),
        Screen::Result => hints(f, area, &[("↑↓", "scroll"), ("enter", "continue")]),
    }
}

fn hints(f: &mut Frame, area: Rect, keys: &[(&str, &str)]) {
    let mut spans = vec![Span::raw(" ")];
    for (key, what) in keys {
        spans.extend(theme::key_hint(key, what));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// A list where rows can be greyed out. `render` turns one entry into its
/// spans, given whether the cursor is on it.
fn list<'a, T>(
    f: &mut Frame,
    area: Rect,
    title: &str,
    focused: bool,
    items: &'a [T],
    cursor: usize,
    render: impl Fn(&'a T, bool) -> Line<'a>,
) {
    let rows: Vec<ListItem> = items
        .iter()
        .enumerate()
        .map(|(i, item)| ListItem::new(render(item, i == cursor)))
        .collect();
    let widget = List::new(rows)
        .block(theme::panel(title, focused))
        .highlight_style(if focused {
            Style::default().bg(ratatui::style::Color::Indexed(236))
        } else {
            Style::default()
        });
    let mut state = ListState::default();
    state.select(Some(cursor.min(items.len().saturating_sub(1))));
    f.render_stateful_widget(widget, area, &mut state);

    // A list taller than its pane scrolls silently otherwise, and a reader who
    // cannot see that there are two more rows below the fold will not go
    // looking for them.
    let visible = area.height.saturating_sub(2) as usize;
    if items.len() > visible && visible > 0 {
        let mut bar = ScrollbarState::new(items.len()).position(state.offset());
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                // No track: the scrollbar sits on the panel's right border, and
                // drawing a track there would erase it. Only the thumb shows,
                // which reads as the border thickening where you are.
                .track_symbol(None)
                .thumb_symbol("┃")
                .style(Style::default().fg(FAINT)),
            area,
            &mut bar,
        );
    }
}

/// The right-hand pane: a heading and prose, optionally with a caveat.
fn detail(f: &mut Frame, area: Rect, title: &str, body: &str, caveat: Option<&str>) {
    let mut text = vec![Line::from(theme::plain(body.to_string()))];
    if let Some(c) = caveat {
        text.push(Line::raw(""));
        text.push(Line::from(vec![
            Span::styled("! ", Style::default().fg(WARN).add_modifier(Modifier::BOLD)),
            Span::styled(c.to_string(), Style::default().fg(WARN)),
        ]));
    }
    f.render_widget(
        Paragraph::new(Text::from(text))
            .block(theme::text_panel(title, false))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn spinner(app: &App) -> &'static str {
    SPINNER[(app.tick / 2) % SPINNER.len()]
}

// ---------------------------------------------------------------------------
// Screen: the opening list
// ---------------------------------------------------------------------------

fn targets(f: &mut Frame, app: &App, area: Rect) {
    // Size the list to what is in it. A five-row list stretched down thirty
    // rows of empty box reads as something having failed to load.
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Max(app.targets.len() as u16 + 2),
        Constraint::Length(7),
        Constraint::Min(0),
    ])
    .split(area);
    f.render_widget(
        Paragraph::new(Line::from(vec![theme::muted(
            " Which client should this work on? Anything greyed out is not installed here.",
        )])),
        rows[0],
    );

    let width = app
        .targets
        .iter()
        .map(|t| t.label.chars().count())
        .max()
        .unwrap_or(20)
        .max(20);

    list(
        f,
        rows[1],
        "Discord installs",
        true,
        &app.targets,
        app.target_cursor,
        move |target, selected| {
            let enabled = target.selectable();
            let mut spans = vec![theme::cursor(selected, enabled)];
            let is_path_row = matches!(target.kind, Kind::AskForPath);
            spans.push(Span::styled(
                theme::pad(&target.label, width + 2),
                if is_path_row {
                    Style::default().fg(ACCENT)
                } else {
                    theme::row_style(selected, enabled)
                },
            ));
            spans.push(match target.kind {
                Kind::NotInstalled | Kind::NoModule { .. } => theme::faint(target.detail.clone()),
                _ => theme::muted(target.detail.clone()),
            });
            Line::from(spans)
        },
    );

    f.render_widget(
        Paragraph::new(Text::from(vec![
            Line::from(vec![
                theme::muted("Quit Discord first. "),
                theme::faint(
                    "A running client already has the old module loaded, so nothing you \
                     change here reaches it until you close it and start it again.",
                ),
            ]),
            Line::raw(""),
            Line::from(vec![
                theme::muted("Nothing here is one-way. "),
                theme::faint(
                    "The untouched module is copied aside before the first change, and \
                     putting it back is one keypress away.",
                ),
            ]),
        ]))
        .block(theme::text_panel("Worth knowing", false))
        .wrap(Wrap { trim: false }),
        rows[2],
    );
}

// ---------------------------------------------------------------------------
// Screen: a module by path
// ---------------------------------------------------------------------------

fn ask_path(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::vertical([
        Constraint::Length(6),
        Constraint::Length(3),
        Constraint::Min(0),
    ])
    .split(area);

    f.render_widget(
        Paragraph::new(Text::from(vec![
            Line::from(theme::plain(
                "Custom clients ship Discord's own voice module, so this works on them \
                 too. Point at the discord_voice.node file itself.",
            )),
            Line::raw(""),
            Line::from(theme::muted(
                "A backup is kept for it the same way, filed under the path you give here.",
            )),
        ]))
        .block(theme::text_panel("Module by path", false))
        .wrap(Wrap { trim: false }),
        rows[0],
    );

    // Paths here run to a hundred characters. Show the end of one that does not
    // fit, not the beginning: what you are typing has to stay visible, and the
    // part already typed is the part you can remember.
    let room = rows[1].width.saturating_sub(8) as usize;
    let typed: Vec<char> = app.input.chars().collect();
    let (ellipsis, visible) = if typed.len() > room {
        ("…", typed[typed.len() - room..].iter().collect::<String>())
    } else {
        ("", app.input.clone())
    };
    let text = Line::from(vec![
        theme::accent("› "),
        theme::faint(ellipsis),
        theme::plain(visible),
        Span::styled("█", Style::default().fg(ACCENT)),
    ]);
    f.render_widget(
        Paragraph::new(text).block(theme::text_panel("Path", true)),
        rows[1],
    );

    if let Some(err) = &app.input_error {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  ! ", Style::default().fg(BAD)),
                Span::styled(err.clone(), Style::default().fg(BAD)),
            ]))
            .wrap(Wrap { trim: false }),
            rows[2],
        );
    } else {
        f.render_widget(
            Paragraph::new(Line::from(theme::faint(
                "  Tab completes. ~ works. Typically \
                 …/modules/discord_voice-1/discord_voice/discord_voice.node",
            )))
            .wrap(Wrap { trim: false }),
            rows[2],
        );
    }
}

// ---------------------------------------------------------------------------
// Screen: the readout
// ---------------------------------------------------------------------------

fn overview(f: &mut Frame, app: &App, area: Rect) {
    // Five rows for the summary, not four: its second line is a sentence that
    // wraps on a narrow terminal, and a header that loses half its warning is
    // worse than one with a blank row in it.
    let rows = Layout::vertical([
        Constraint::Length(5),
        Constraint::Min(6),
        Constraint::Length(5),
    ])
    .split(area);

    summary(f, app, rows[0]);

    match (&app.inspection, app.busy) {
        (Some(inspection), _) => {
            let panes =
                Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
                    .split(rows[1]);
            status_table(f, app, inspection, panes[0]);
            let row = inspection.rows.get(app.status_cursor);
            detail(
                f,
                panes[1],
                row.map(|r| r.label).unwrap_or("Details"),
                row.map(|r| r.note.as_str()).unwrap_or(""),
                None,
            );
        }
        (None, Some(verb)) => {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    theme::accent(format!("  {} ", spinner(app))),
                    theme::plain(format!("{verb}…")),
                ]))
                .block(theme::text_panel("What this module does to your audio", false)),
                rows[1],
            );
        }
        (None, None) => {
            // No module to read. Say which of the two reasons it is, because
            // one of them is fixed by launching Discord and the other cannot
            // be fixed by launching Discord at all.
            let removed = matches!(
                app.target().map(|t| &t.kind),
                Some(Kind::NoModule { removed: true, .. })
            );
            let body = if removed {
                vec![
                    Line::from(Span::styled(
                        "The voice module is not here.",
                        Style::default().fg(BAD).add_modifier(Modifier::BOLD),
                    )),
                    Line::raw(""),
                    Line::from(theme::plain(
                        "Discord will not download it again on its own: its updater still \
                         has this module recorded as installed, and it trusts that record \
                         without checking the files. That is why the client reports the \
                         installation as corrupt, and why restarting does not help.",
                    )),
                    Line::raw(""),
                    Line::from(theme::plain(
                        "\"Manage copies on disk\" → \"Reinstall the voice module\" clears \
                         that record. The next start fetches a fresh one.",
                    )),
                ]
            } else {
                vec![
                    Line::from(theme::muted("No voice module here yet.")),
                    Line::raw(""),
                    Line::from(theme::plain(
                        "Discord downloads it on first launch. Start the client, join a \
                         voice channel once, then come back.",
                    )),
                ]
            };
            f.render_widget(
                Paragraph::new(Text::from(body))
                    .block(theme::text_panel("What this module does to your audio", false))
                    .wrap(Wrap { trim: false }),
                rows[1],
            );
        }
    }

    list(
        f,
        rows[2],
        "What next",
        app.focus == Focus::Actions,
        ACTIONS,
        app.action_cursor,
        |(label, hint), selected| {
            let index = ACTIONS.iter().position(|(l, _)| l == label).unwrap_or(0);
            let enabled = app.action_enabled(index);
            Line::from(vec![
                theme::cursor(selected, enabled),
                Span::styled(theme::pad(label, 26), theme::row_style(selected, enabled)),
                theme::faint(if enabled { hint.to_string() } else { "no module to patch".to_string() }),
            ])
        },
    );
}

/// The one-glance line: what this file is, and whether what follows can be
/// trusted.
fn summary(f: &mut Frame, app: &App, area: Rect) {
    let Some(target) = app.target() else { return };
    let mut lines: Vec<Line> = Vec::new();

    match &app.inspection {
        Some(i) => {
            let (word, colour) = match (i.recognised(), i.state) {
                (false, _) => ("not a voice module this tool knows", BAD),
                (_, crate::patch::State::Stock) => ("untouched", BAD),
                (_, crate::patch::State::Stereocord) => ("patched by stereocord", GOOD),
                (_, crate::patch::State::Patched) => ("patched", GOOD),
            };
            lines.push(Line::from(vec![
                Span::styled(
                    word.to_string(),
                    Style::default().fg(colour).add_modifier(Modifier::BOLD),
                ),
                theme::faint("  ·  "),
                theme::muted(theme::bytes(i.size)),
                theme::faint("  ·  "),
                theme::muted(format!("md5 {}", &i.md5[..12])),
                theme::faint("  ·  "),
                theme::muted(match i.symbols {
                    Some(n) => format!("{n} symbols"),
                    None => "stripped".to_string(),
                }),
            ]));
            // Say where the readings came from. Without a backup, a patched
            // module can only be read where a signature happens to survive its
            // own patch, and claiming otherwise would be the one thing this
            // panel must never do.
            lines.push(match (i.baseline, i.state) {
                _ if !i.recognised() => Line::from(Span::styled(
                    "nothing in this file matched — either it is not Discord's voice \
                     module, or it is a build this version has never seen",
                    Style::default().fg(BAD),
                )),
                (Baseline::Backup, _) => Line::from(theme::faint(
                    "read against the backup, so every line below is exact",
                )),
                (Baseline::SelfScan, crate::patch::State::Stock) => Line::from(theme::faint(
                    "read directly; the module is untouched, so nothing is hidden",
                )),
                (Baseline::SelfScan, _) => Line::from(Span::styled(
                    "no backup on record — a patch hides the very bytes that identify it, \
                     so some lines below will say they cannot be read",
                    Style::default().fg(WARN),
                )),
            });
        }
        None => lines.push(Line::from(theme::muted(match target.node() {
            Some(node) => node.display().to_string(),
            None => target
                .install()
                .map(|i| i.app_dir.display().to_string())
                .unwrap_or_default(),
        }))),
    }

    f.render_widget(
        Paragraph::new(Text::from(lines))
            .block(theme::text_panel(&target.label, false))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn status_table(f: &mut Frame, app: &App, inspection: &Inspection, area: Rect) {
    list(
        f,
        area,
        "What this module does to your audio",
        app.focus == Focus::Status,
        &inspection.rows,
        app.status_cursor,
        |row, selected| {
            Line::from(vec![
                Span::raw(" "),
                theme::dot(row.level),
                Span::raw(" "),
                Span::styled(theme::pad(row.label, 21), theme::row_style(selected, true)),
                theme::coloured(row.value.clone(), theme::level_color(row.level)),
            ])
        },
    );
}

// ---------------------------------------------------------------------------
// Screen: choosing what to change
// ---------------------------------------------------------------------------

fn patch(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::vertical([Constraint::Min(6), Constraint::Length(4)]).split(area);
    let panes =
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).split(rows[0]);

    let coverage: Vec<(usize, usize)> = sites::GROUPS
        .iter()
        .map(|g| match &app.inspection {
            Some(i) => {
                let total = sites::SITES.iter().filter(|s| s.group == g.name).count();
                let found = sites::SITES
                    .iter()
                    .filter(|s| s.group == g.name && i.report.covered(s.name))
                    .count();
                (found, total)
            }
            None => (0, 0),
        })
        .collect();

    let on = &app.groups_on;
    list(
        f,
        panes[0],
        "Choose what to change",
        true,
        sites::GROUPS,
        app.group_cursor,
        |group, selected| {
            let index = sites::GROUPS.iter().position(|g| g.name == group.name).unwrap_or(0);
            let checked = on.get(index).copied().unwrap_or(false);
            let (found, total) = coverage[index];
            let mut spans = vec![
                theme::cursor(selected, true),
                if checked {
                    Span::styled("[x] ", Style::default().fg(GOOD))
                } else {
                    Span::styled("[ ] ", Style::default().fg(FAINT))
                },
                Span::styled(
                    theme::pad(group.title, 28),
                    if checked {
                        theme::row_style(selected, true)
                    } else {
                        Style::default().fg(MUTED)
                    },
                ),
            ];
            spans.push(if total == 0 {
                theme::faint("—")
            } else if found == total {
                theme::faint(format!("{found}/{total}"))
            } else {
                Span::styled(format!("{found}/{total}"), Style::default().fg(WARN))
            });
            Line::from(spans)
        },
    );

    // The right pane explains the highlighted group, and says outright when
    // some of it will be skipped on this build.
    let group = &sites::GROUPS[app.group_cursor];
    let (found, total) = coverage[app.group_cursor];
    let mut body = group.summary.to_string();
    if total > 0 && found < total {
        body.push_str(&format!(
            "\n\n{} of this group's {} changes are not in this build and will be skipped.",
            total - found,
            total
        ));
    }
    detail(f, panes[1], group.title, &body, group.caveat);

    bitrate_bar(f, app, rows[1]);
}

fn bitrate_bar(f: &mut Frame, app: &App, area: Rect) {
    let wants_bitrate = sites::GROUPS
        .iter()
        .zip(&app.groups_on)
        .any(|(g, &on)| g.name == "bitrate" && on);

    let value_style = if wants_bitrate {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(FAINT)
    };
    let mut lines = vec![Line::from(vec![
        Span::raw(" "),
        Span::styled("◂ ", Style::default().fg(if wants_bitrate { ACCENT } else { FAINT })),
        Span::styled(format!("{} kbps", app.bitrate), value_style),
        Span::styled(" ▸   ", Style::default().fg(if wants_bitrate { ACCENT } else { FAINT })),
        if wants_bitrate {
            theme::muted("Discord's own maximum is 96 kbps outside a boosted server.")
        } else {
            theme::faint("switch on \"High bitrate\" for this to do anything")
        },
    ])];
    if app.seeded_from_module {
        lines.push(Line::from(theme::faint(
            " Starting from what this module already has, not from the defaults — press d \
             for the recommended set.",
        )));
    }
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .block(theme::text_panel("Bitrate", false))
            .wrap(Wrap { trim: false }),
        area,
    );
}

// ---------------------------------------------------------------------------
// Screen: managing what is on disk
// ---------------------------------------------------------------------------

fn manage(f: &mut Frame, app: &App, area: Rect) {
    let panes =
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).split(area);
    let items = app.manage_items();

    list(
        f,
        panes[0],
        "Copies on disk",
        true,
        &items,
        app.manage_cursor,
        |item, selected| {
            let style = if !item.enabled {
                Style::default().fg(FAINT)
            } else if item.danger {
                Style::default().fg(DANGER)
            } else {
                theme::row_style(selected, true)
            };
            Line::from(vec![
                theme::cursor(selected, item.enabled),
                Span::styled(theme::pad(&item.label, 32), style),
                theme::faint(item.hint.clone()),
            ])
        },
    );

    let item = &items[app.manage_cursor.min(items.len() - 1)];
    detail(
        f,
        panes[1],
        &item.label,
        item.detail,
        if item.enabled { None } else { Some("Not available for this install.") },
    );
}

// ---------------------------------------------------------------------------
// Screen: what happened
// ---------------------------------------------------------------------------

fn result(f: &mut Frame, app: &App, area: Rect) {
    let Some(log) = &app.log else { return };
    let mut lines: Vec<Line> = Vec::new();
    for entry in &log.lines {
        lines.push(Line::from(vec![
            Span::raw("  "),
            theme::dot(entry.level),
            Span::raw(" "),
            Span::styled(
                entry.text.clone(),
                Style::default().fg(match entry.level {
                    Level::Bad => BAD,
                    Level::Warn => WARN,
                    _ => ratatui::style::Color::Reset,
                }),
            ),
        ]));
    }
    let title = if log.ok {
        format!("✓ {}", log.title)
    } else {
        format!("✗ {}", log.title)
    };
    let block = theme::text_panel(&title, false).border_style(Style::default().fg(if log.ok {
        GOOD
    } else {
        BAD
    }));
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((app.log_scroll, 0)),
        area,
    );
}

// ---------------------------------------------------------------------------
// The dialog
// ---------------------------------------------------------------------------

fn modal(f: &mut Frame, app: &App) {
    let Some(m) = &app.modal else { return };
    // Tall enough for what it says and no taller: a dialog padded out with
    // blank rows looks like it is still loading something.
    let width = 68u16.min(f.area().width.saturating_sub(4)).max(20);
    let text_width = width.saturating_sub(4).max(1) as usize;
    let body_rows: usize = m
        .body
        .iter()
        .map(|l| l.chars().count().div_ceil(text_width).max(1))
        .sum();
    let area = centred(f.area(), width, body_rows as u16 + 5);
    f.render_widget(Clear, area);

    let accent = if m.danger { DANGER } else { ACCENT };
    let block = theme::text_panel(&m.title, false).border_style(
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    );
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).split(inner);
    let body: Vec<Line> = m.body.iter().map(|l| Line::from(theme::plain(l.clone()))).collect();
    f.render_widget(
        Paragraph::new(Text::from(body)).wrap(Wrap { trim: false }),
        rows[0],
    );

    let button = |label: &str, active: bool, danger: bool| -> Span<'static> {
        let style = match (active, danger) {
            (true, true) => Style::default()
                .fg(ratatui::style::Color::Black)
                .bg(DANGER)
                .add_modifier(Modifier::BOLD),
            (true, false) => Style::default()
                .fg(ratatui::style::Color::Black)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
            (false, _) => Style::default().fg(MUTED),
        };
        Span::styled(format!("  {label}  "), style)
    };
    let buttons = Line::from(vec![
        button(&m.confirm, m.cursor == 0, m.danger),
        Span::raw("   "),
        button("Cancel", m.cursor == 1, false),
    ]);
    f.render_widget(Paragraph::new(buttons).alignment(Alignment::Center), rows[1]);
}

/// A box of at most `width` × `height`, centred, but never larger than what is
/// there — a dialog that overflows a small terminal draws nothing at all.
fn centred(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2)).max(1);
    let h = height.min(area.height.saturating_sub(2)).max(1);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dialog_never_exceeds_the_screen() {
        let tiny = Rect { x: 0, y: 0, width: 20, height: 6 };
        let box_ = centred(tiny, 66, 15);
        assert!(box_.width <= tiny.width && box_.height <= tiny.height);
        assert!(box_.x + box_.width <= tiny.x + tiny.width);
        assert!(box_.y + box_.height <= tiny.y + tiny.height);
    }

    #[test]
    fn a_dialog_on_a_one_cell_screen_is_still_valid() {
        let sliver = Rect { x: 0, y: 0, width: 1, height: 1 };
        let box_ = centred(sliver, 66, 15);
        assert_eq!((box_.width, box_.height), (1, 1));
    }
}
