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

//! stereocord — stereo / high-bitrate patcher for Discord's Linux voice module.
//!
//! Run it with no arguments and it opens a terminal interface: pick a client,
//! read what its voice module is currently doing to your audio, and decide
//! from there. Run it with a command and it behaves as a plain command-line
//! tool, which is what scripts and the impatient want.
//!
//! Both paths meet in [`apply`], so there is exactly one implementation of
//! what patching means.

mod apply;
mod backup;
mod discovery;
mod elf;
mod manage;
mod md5;
mod patch;
mod resolve;
mod shellcode;
mod sig;
mod sites;
mod status;
mod tui;

use discovery::Install;
use patch::Config;
use resolve::Outcome;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
stereocord — force stereo, 48 kHz and a high Opus bitrate in Discord on Linux

USAGE:
    stereocord                    Open the interface
    stereocord <COMMAND> [OPTIONS]

Run with no arguments at all and stereocord opens a terminal interface: choose
a client, see what its voice module is currently doing to your audio, and patch
or restore from there. Everything below is the same work without the interface.

COMMANDS:
    tui                 Open the interface (what running with no arguments does)
    scan                Show every Discord install, what its module is doing,
                        and whether each patch site can still be located
    patch               Apply the patches
    restore             Put the original module back from the backup
    backups             List backups on record
    groups              List the patch groups and what each one does
    shellcode           Print the injected filter replacements as bytes

OPTIONS:
    -c, --client <TEXT>   Only act on installs whose label contains TEXT
                          (e.g. 'Canary', '1.0.155')
    -a, --all             Act on every install, not just the newest per channel
                          that has a voice module
    -b, --bitrate <KBPS>  Opus bitrate to lock in           [default: 248]
        --gain <FACTOR>   Gain applied by the injected filters  [default: 1.0]
    -n, --dry-run         Say what would be written, write nothing
    -f, --force           Patch even while Discord is running
        --allow-partial   Patch even if some sites could not be located
    -y, --yes             Do not ask for confirmation
    -g, --groups <LIST>   Comma-separated groups to apply (see 'groups');
                          'all' selects every group  [default: the recommended set]
    -v, --verbose         Show every resolved offset
        --node <PATH>     Act on this discord_voice.node instead of searching
                          for installs (useful for checking a build, or for a
                          custom client using Discord's own module)
    -h, --help            Show this message
    -V, --version         Show the version

Discord must be closed: a running client already has the old module mapped.
";

struct Args {
    command: String,
    client: Option<String>,
    all: bool,
    bitrate: u32,
    gain: f32,
    dry_run: bool,
    force: bool,
    allow_partial: bool,
    yes: bool,
    verbose: bool,
    node: Option<String>,
    groups: Option<Vec<String>>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        command: String::new(),
        client: None,
        all: false,
        bitrate: 248,
        gain: 1.0,
        dry_run: false,
        force: false,
        allow_partial: false,
        yes: false,
        verbose: false,
        node: None,
        groups: None,
    };

    let mut it = std::env::args().skip(1).peekable();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| -> Result<String, String> {
            it.next().ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("stereocord {VERSION}");
                std::process::exit(0);
            }
            "-c" | "--client" => args.client = Some(value("--client")?),
            "-a" | "--all" => args.all = true,
            "-b" | "--bitrate" => {
                let v = value("--bitrate")?;
                args.bitrate = v.parse().map_err(|_| format!("bad bitrate {v:?}"))?;
            }
            "--gain" => {
                let v = value("--gain")?;
                args.gain = v.parse().map_err(|_| format!("bad gain {v:?}"))?;
            }
            "-g" | "--groups" => {
                let v = value("--groups")?;
                args.groups = Some(parse_groups(&v)?);
            }
            "-n" | "--dry-run" => args.dry_run = true,
            "-f" | "--force" => args.force = true,
            "--allow-partial" => args.allow_partial = true,
            "-y" | "--yes" => args.yes = true,
            "-v" | "--verbose" => args.verbose = true,
            "--node" => args.node = Some(value("--node")?),
            other if other.starts_with('-') => return Err(format!("unknown option {other:?}")),
            other if args.command.is_empty() => args.command = other.to_string(),
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }

    // No arguments at all means the interface. Options without a command still
    // mean `scan`, so `stereocord --node <path>` keeps working the way it
    // always has rather than silently opening a UI.
    if args.command.is_empty() {
        args.command = if std::env::args().len() == 1 { "tui" } else { "scan" }.to_string();
    }
    if !(8..=512).contains(&args.bitrate) {
        return Err(format!("bitrate {} kbps is outside 8-512", args.bitrate));
    }
    if !(0.1..=10.0).contains(&args.gain) {
        return Err(format!("gain {} is outside 0.1-10.0", args.gain));
    }
    Ok(args)
}

/// Parse a --groups value. `all` is spelled out rather than inferred so that a
/// typo is an error instead of a silent no-op.
fn parse_groups(spec: &str) -> Result<Vec<String>, String> {
    if spec.trim() == "all" {
        return Ok(sites::GROUPS.iter().map(|g| g.name.to_string()).collect());
    }
    let mut out = Vec::new();
    for part in spec.split(',') {
        let name = part.trim();
        if name.is_empty() {
            continue;
        }
        match sites::group(name) {
            Some(g) => out.push(g.name.to_string()),
            None => {
                let known: Vec<&str> = sites::GROUPS.iter().map(|g| g.name).collect();
                return Err(format!(
                    "unknown group {name:?}; known groups are {}",
                    known.join(", ")
                ));
            }
        }
    }
    if out.is_empty() {
        return Err("--groups selected nothing".to_string());
    }
    Ok(out)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("stereocord: {e}\n\nTry 'stereocord --help'.");
            return ExitCode::FAILURE;
        }
    };
    let cfg = Config {
        bitrate_kbps: args.bitrate,
        gain: args.gain,
        groups: args
            .groups
            .clone()
            .unwrap_or_else(|| sites::default_groups().iter().map(|g| g.to_string()).collect()),
    };

    let result = match args.command.as_str() {
        "tui" => tui::run(args.bitrate, args.gain),
        "scan" => cmd_scan(&args),
        "patch" => cmd_patch(&args, &cfg),
        "restore" => cmd_restore(&args),
        "backups" => cmd_backups(),
        "groups" => {
            cmd_groups();
            Ok(())
        }
        "shellcode" => {
            print!("{}", shellcode::describe(cfg.gain));
            Ok(())
        }
        other => Err(format!("unknown command {other:?}\n\nTry 'stereocord --help'.")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("stereocord: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Installs to act on, honouring --client / --all.
///
/// Without --all only the newest version per channel is selected, because that
/// is the one Discord will launch. Older `app-` directories are left alone.
fn select(args: &Args) -> Result<Vec<Install>, String> {
    let all = discovery::find_installs();
    if all.is_empty() {
        return Err("no Discord installation found under ~/.config".to_string());
    }

    let mut chosen: Vec<Install> = all
        .iter()
        .filter(|i| match &args.client {
            Some(t) => i.label().to_lowercase().contains(&t.to_lowercase()),
            None => true,
        })
        .cloned()
        .collect();

    if chosen.is_empty() {
        return Err(format!(
            "no install matches {:?}; run 'stereocord scan' to see what is here",
            args.client.clone().unwrap_or_default()
        ));
    }

    if !args.all {
        // Newest version per channel that actually has a module. The newest
        // directory overall may be an update Discord has staged but not yet
        // populated; patching is only meaningful where a module exists, and
        // the staged one is reported separately.
        let mut seen = Vec::new();
        chosen.retain(|i| {
            if i.node.is_none() || seen.contains(&i.channel) {
                false
            } else {
                seen.push(i.channel.clone());
                true
            }
        });
        if chosen.is_empty() {
            return Err(
                "every Discord install here has an empty voice module directory.\n\
                 Launch Discord once so it downloads the module, then patch."
                    .to_string(),
            );
        }
    }
    Ok(chosen)
}

fn cmd_scan(args: &Args) -> Result<(), String> {
    if let Some(path) = &args.node {
        return report_sites(Path::new(path), args.verbose, None);
    }
    let installs = discovery::find_installs();
    if installs.is_empty() {
        return Err("no Discord installation found under ~/.config".to_string());
    }

    println!("Discord installs");
    let newest = newest_per_channel(&installs);
    for install in &installs {
        let is_newest = newest.contains(&install.label());
        let marker = if is_newest { "->" } else { "  " };
        match &install.node {
            Some(node) => {
                let size = fs::metadata(node).map(|m| m.len()).unwrap_or(0);
                println!(
                    "{marker} {:<26} {:>12} bytes  {}",
                    install.label(),
                    size,
                    node.display()
                );
            }
            None => println!(
                "{marker} {:<26} {:>12}         {}",
                install.label(),
                "(no module)",
                install.app_dir.join("modules").display()
            ),
        }
    }
    println!("\n'->' marks the version Discord will launch for that channel.");

    for (staged, blocker) in staged_updates(&installs) {
        match blocker {
            Some(patched) => println!(
                "\nnote: {} is staged, but its voice module is missing and {} is\n\
                 patched. Discord ships this module as a binary delta and checks the\n\
                 current file's hash before applying it, so a patched module makes the\n\
                 delta fail and aborts the whole update — Discord keeps launching the\n\
                 old version. To take the update: 'stereocord restore', start Discord\n\
                 and let it update, quit, then 'stereocord patch' again.",
                staged.label(),
                patched
            ),
            None => println!(
                "\nnote: {} has no voice module yet. Discord downloads it on next\n\
                 launch, so anything patched in an older app- directory stops applying\n\
                 once that update goes live. Re-run 'stereocord patch' after starting\n\
                 Discord on the new version.",
                staged.label()
            ),
        }
    }

    for install in select(args)? {
        let Some(node) = install.node.clone() else { continue };
        println!("\n--- {} ---", install.label());
        report_sites(&node, args.verbose, Some(&install))?;
    }
    Ok(())
}

fn newest_per_channel(installs: &[Install]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for i in installs {
        if !seen.contains(&i.channel) {
            seen.push(i.channel.clone());
            out.push(i.label());
        }
    }
    out
}

/// Installs that are newer than a sibling that does have a module — i.e. an
/// update Discord has staged but not yet populated — paired with the label of
/// the older install if that one is patched.
///
/// Discord ships module updates as binary deltas and verifies the SHA-256 of
/// the file it is about to patch. A patched module fails that check, which
/// aborts the whole update, so Discord goes on launching the old version. The
/// staged directory then sits half-populated indefinitely, which looks like a
/// stalled download rather than what it is.
fn staged_updates(installs: &[Install]) -> Vec<(&Install, Option<String>)> {
    installs
        .iter()
        .filter(|i| i.node.is_none())
        .filter_map(|i| {
            let older: Vec<&Install> = installs
                .iter()
                .filter(|o| o.channel == i.channel && o.node.is_some() && o.version < i.version)
                .collect();
            if older.is_empty() {
                return None;
            }
            let blocker = older.iter().find(|o| {
                o.node
                    .as_ref()
                    .and_then(|n| fs::read(n).ok())
                    .map(|d| {
                        let r = resolve::resolve_all(&d, elf::symbols(&d).as_ref());
                        patch::classify(&d, &r) != patch::State::Stock
                    })
                    .unwrap_or(false)
            });
            Some((i, blocker.map(|o| o.label())))
        })
        .collect()
}

/// What one module is, what it is doing, and whether the catalogue can still
/// find its way around it — in that order, because the first two are what the
/// person asked and the third is about the tool.
fn report_sites(node: &Path, verbose: bool, install: Option<&Install>) -> Result<(), String> {
    let facts = match install {
        Some(i) => status::inspect_install(i, node)?,
        None => status::inspect_path(node)?,
    };
    println!("  size {} bytes, md5 {}", facts.size, facts.md5);
    match facts.symbols {
        Some(n) => println!("  symbols: {n} functions"),
        None => println!("  symbols: none (stripped) - falling back to scanning"),
    }
    if facts.recognised() {
        println!("  status: {}", facts.state);
    } else {
        println!(
            "  status: unrecognised — not one site matched, so this is either not\n  Discord's voice module, or a build this version of stereocord has never seen"
        );
    }

    println!("\n  What this module does to your audio");
    for row in &facts.rows {
        println!(
            "    {} {:<22} {}",
            status::plain_glyph(row.level),
            row.label,
            row.value
        );
    }
    if facts.baseline == status::Baseline::SelfScan && facts.state != patch::State::Stock {
        println!(
            "\n  No backup is on record for this module, and a patch overwrites the\n  very bytes most signatures key on. So the lines above and below that say\n  they cannot be read are a limit of what is knowable here, not a fault in\n  the module. Restore it, or scan a stock copy, to see the rest."
        );
    }

    println!("\n  Patch sites");
    let report = &facts.report;
    let mut excused_count = 0usize;
    let mut ok = 0;
    for name in &report.order {
        match report.outcomes.get(name) {
            Some(Outcome::Found(r)) => {
                ok += 1;
                if verbose {
                    let offsets: Vec<String> =
                        r.offsets.iter().map(|o| format!("0x{o:X}")).collect();
                    println!(
                        "    ok      {:<30} {:<10} {:<20} {}",
                        name,
                        r.site.group,
                        offsets.join(", "),
                        r.via
                    );
                    if r.variant > 0 {
                        println!("            via alternative encoding #{}", r.variant + 1);
                    }
                }
            }
            Some(Outcome::Ambiguous { found, wanted }) => {
                println!("    AMBIG   {name:<30} matched {found} times, expected {wanted}");
            }
            _ => match report.excused(name) {
                Some(note) => {
                    excused_count += 1;
                    println!("    n/a     {name:<30} {note}");
                }
                None => println!("    MISSING {name:<30} no signature matched"),
            },
        }
    }
    let gaps = report.order.len() - ok - excused_count;
    print!("    {ok}/{} sites located", report.order.len());
    if excused_count > 0 {
        print!(", {excused_count} not needed on this build");
    }
    if gaps > 0 {
        print!(", {gaps} missing");
    }
    println!();
    Ok(())
}

fn cmd_patch(args: &Args, cfg: &Config) -> Result<(), String> {
    let installs = discovery::find_installs();
    for (staged, blocker) in staged_updates(&installs) {
        match blocker {
            Some(patched) => eprintln!(
                "note: {} is staged but blocked — Discord's delta update for the voice\n\
                 module checks the current file's hash, and {} is patched. Run\n\
                 'stereocord restore' and let Discord update before patching again.",
                staged.label(),
                patched
            ),
            None => eprintln!(
                "note: {} is staged with no voice module yet; it will need patching\n\
                 again once Discord has launched it at least once.",
                staged.label()
            ),
        }
    }

    let targets = select(args)?;
    let mut patched = 0;

    for install in &targets {
        println!("=== {} ===", install.label());
        let Some(node) = install.node.clone() else {
            println!("  no voice module present yet; skipping\n");
            continue;
        };

        let pids = discovery::running_pids(&install.app_dir);
        if !pids.is_empty() {
            let list: Vec<String> = pids.iter().map(|p| p.to_string()).collect();
            if !args.force {
                println!(
                    "  Discord is running (pid {}). The running client already has the\n\
                     \x20 old module mapped, so patching now would change nothing until it\n\
                     \x20 restarts. Close Discord and re-run, or pass --force.\n",
                    list.join(", ")
                );
                continue;
            }
            println!("  warning: Discord is running (pid {}); --force given", list.join(", "));
        }

        match patch_one(install, &node, args, cfg)? {
            true => patched += 1,
            false => {}
        }
        println!();
    }

    if patched > 0 && !args.dry_run {
        println!("Patched {patched} install(s). Start Discord and re-join a voice channel.");
        println!("Set input mode to a stereo source; a mono microphone still gives you");
        println!("two identical channels, which analysers report as mono.");
    }
    Ok(())
}

fn patch_one(
    install: &Install,
    node: &Path,
    args: &Args,
    cfg: &Config,
) -> Result<bool, String> {
    // Everything the interface does, minus the interface. The two share
    // `apply` precisely so that "patched" cannot come to mean two things.
    let mut log = |note: apply::Note| {
        let text = match note {
            apply::Note::Info(t) | apply::Note::Good(t) => t,
            apply::Note::Warn(t) => t,
            apply::Note::Bad(t) => t,
        };
        for line in text.lines() {
            println!("  {line}");
        }
    };

    let prepared = apply::prepare(install, node, args.dry_run, &mut log)?;
    apply::report_gaps(&prepared, &mut log);

    if prepared.blocked() && !args.allow_partial {
        println!(
            "  Refusing to patch. Pass --allow-partial to apply the rest anyway."
        );
        return Ok(false);
    }
    if prepared.blocked() {
        println!("  --allow-partial given; applying the sites that did resolve");
    }
    if cfg.groups.is_empty() {
        println!("  no groups selected; nothing to do");
        return Ok(false);
    }

    let plan = apply::plan(&prepared, cfg)?;
    if plan.edits.is_empty() {
        println!("  nothing to change for the groups selected");
        return Ok(false);
    }
    println!(
        "  {} edits, {} bytes: {}",
        plan.edits.len(),
        plan.total_bytes(),
        apply::effects(&plan, &prepared.report, cfg).join(", ")
    );
    if args.verbose {
        for e in &plan.edits {
            println!(
                "    0x{:08X} {:<10} {:<30} {:<40} {}",
                e.offset,
                e.group,
                e.site,
                e.what,
                sig::hex(&e.bytes[..e.bytes.len().min(12)])
            );
        }
    }

    if args.dry_run {
        println!("  dry run; nothing written");
        return Ok(false);
    }
    if !args.yes && !confirm(&format!("  Patch {}?", node.display()))? {
        println!("  skipped");
        return Ok(false);
    }

    apply::commit(install, node, prepared, &plan, &mut log)?;
    Ok(true)
}

fn cmd_restore(args: &Args) -> Result<(), String> {
    let mut done = 0;
    for install in select(args)? {
        let Some(node) = install.node.clone() else { continue };
        if !backup::exists(&install) {
            println!("{}: no backup on record", install.label());
            continue;
        }
        if args.dry_run {
            println!("{}: would restore {}", install.label(), node.display());
            continue;
        }
        let src = backup::restore(&install, &node).map_err(|e| format!("{e}"))?;
        println!("{}: restored from {}", install.label(), src.display());
        done += 1;
    }
    if done == 0 && !args.dry_run {
        println!("Nothing restored.");
    }
    Ok(())
}

/// The same text the interface shows beside each checkbox, for anyone reading
/// the tool rather than driving it.
fn cmd_groups() {
    // Written through a handle with the errors dropped rather than with
    // `println!`, so `stereocord groups | head` closes the pipe quietly instead
    // of panicking partway down the list.
    let out = std::io::stdout();
    let mut w = out.lock();
    let _ = writeln!(w, "Patch groups. The interface offers these as checkboxes; on the command");
    let _ = writeln!(w, "line --groups picks them, and --groups all selects every one.\n");
    for g in sites::GROUPS {
        let n = sites::SITES.iter().filter(|s| s.group == g.name).count();
        let plural = if n == 1 { "change" } else { "changes" };
        let state = if g.default_on { "on by default" } else { "off by default" };
        let _ = writeln!(w, "  {:<12} {}  ({n} {plural}, {state})", g.name, g.title);
        for line in wrap_plain(g.summary, 70) {
            let _ = writeln!(w, "               {line}");
        }
        if let Some(c) = g.caveat {
            for line in wrap_plain(c, 70) {
                let _ = writeln!(w, "               ! {line}");
            }
        }
        let _ = writeln!(w);
    }
}

fn wrap_plain(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

fn cmd_backups() -> Result<(), String> {
    let entries = backup::list();
    if entries.is_empty() {
        println!("No backups in {}", backup::dir().display());
        return Ok(());
    }
    println!("{}", backup::dir().display());
    for e in entries {
        println!("  {:>12} bytes  {}", e.size, e.path.display());
    }
    Ok(())
}

fn confirm(prompt: &str) -> Result<bool, String> {
    print!("{prompt} [y/N] ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| format!("reading answer: {e}"))?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}
