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
//! A Rust reimplementation of the Linux half of ProdHallow's
//! Discord-Stereo-Windows-MacOS-Linux (discontinued, last commit 5e96ff0).
//! Same patches, located by signature rather than by hardcoded offset, and
//! applied to the module the machine actually has.

mod backup;
mod discovery;
mod elf;
mod md5;
mod patch;
mod resolve;
mod shellcode;
mod sig;
mod sites;

use discovery::Install;
use patch::Config;
use resolve::Outcome;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
stereocord — force stereo, 48 kHz and a high Opus bitrate in Discord on Linux

USAGE:
    stereocord <COMMAND> [OPTIONS]

COMMANDS:
    scan                Show every Discord install, and whether each patch site
                        can be located in its voice module  (default)
    patch               Apply the patches
    restore             Put the original module back from the backup
    backups             List backups on record
    shellcode           Print the injected filter replacements as bytes

OPTIONS:
    -c, --client <TEXT>   Only act on installs whose label contains TEXT
                          (e.g. 'Canary', '1.0.155')
    -a, --all             Act on every install, not just the newest per channel
                          that has a voice module
    -b, --bitrate <KBPS>  Opus bitrate to lock in           [default: 248]
    -g, --gain <FACTOR>   Gain applied by the injected filters  [default: 1.0]
    -n, --dry-run         Say what would be written, write nothing
    -f, --force           Patch even while Discord is running
        --allow-partial   Patch even if some sites could not be located
    -y, --yes             Do not ask for confirmation
    -v, --verbose         Show every resolved offset
        --node <PATH>     Scan this discord_voice.node instead of searching for
                          installs (useful for checking a build before patching)
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
            "-g" | "--gain" => {
                let v = value("--gain")?;
                args.gain = v.parse().map_err(|_| format!("bad gain {v:?}"))?;
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

    if args.command.is_empty() {
        args.command = "scan".to_string();
    }
    if !(8..=512).contains(&args.bitrate) {
        return Err(format!("bitrate {} kbps is outside 8-512", args.bitrate));
    }
    if !(0.1..=10.0).contains(&args.gain) {
        return Err(format!("gain {} is outside 0.1-10.0", args.gain));
    }
    Ok(args)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("stereocord: {e}\n\nTry 'stereocord --help'.");
            return ExitCode::FAILURE;
        }
    };
    let cfg = Config { bitrate_kbps: args.bitrate, gain: args.gain };

    let result = match args.command.as_str() {
        "scan" => cmd_scan(&args),
        "patch" => cmd_patch(&args, &cfg),
        "restore" => cmd_restore(&args),
        "backups" => cmd_backups(),
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
        return report_sites(Path::new(path), args.verbose);
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
        report_sites(&node, args.verbose)?;
    }
    Ok(())
}

/// A missing site that the build genuinely does not need, and why.
///
/// Returns the explanation when absence is fine, `None` when it is a real gap.
fn excused(name: &str, report: &resolve::Report) -> Option<&'static str> {
    let site = sites::find(name)?;
    let absent = site.absent_ok.as_ref()?;
    match absent.covered_by {
        // Only excused if whatever covers it actually resolved.
        Some(other) => report.resolved(other).map(|_| absent.note),
        None => Some(absent.note),
    }
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

fn report_sites(node: &Path, verbose: bool) -> Result<(), String> {
    let data = read_node(node)?;
    println!("  size {} bytes, md5 {}", data.len(), md5::hex(&data));

    let syms = elf::symbols(&data);
    match &syms {
        Some(s) => println!("  symbols: {} functions", s.len()),
        None => println!("  symbols: none (stripped) - falling back to scanning"),
    }
    let report = resolve::resolve_all(&data, syms.as_ref());
    let state = patch::classify(&data, &report);
    println!("  status: {state}");
    let mut excused_count = 0usize;
    if state == patch::State::Patched {
        println!(
            "  Most sites will read as MISSING below: their original instructions\n  \
             have been overwritten, so there is nothing left to match. That is\n  \
             expected for a patched module, not a fault. To re-scan properly, or to\n  \
             re-patch, run 'stereocord restore' first."
        );
    }
    let mut ok = 0;
    for name in &report.order {
        match report.outcomes.get(name) {
            Some(Outcome::Found(r)) => {
                ok += 1;
                if verbose {
                    let offsets: Vec<String> =
                        r.offsets.iter().map(|o| format!("0x{o:X}")).collect();
                    println!(
                        "  ok      {:<30} {:<10} {:<20} {}",
                        name,
                        r.site.group,
                        offsets.join(", "),
                        r.via
                    );
                    if r.variant > 0 {
                        println!("          via alternative encoding #{}", r.variant + 1);
                    }
                }
            }
            Some(Outcome::Ambiguous { found, wanted }) => {
                println!("  AMBIG   {name:<30} matched {found} times, expected {wanted}");
            }
            _ => match excused(name, &report) {
                Some(note) => {
                    excused_count += 1;
                    println!("  n/a     {name:<30} {note}");
                }
                None => println!("  MISSING {name:<30} no signature matched"),
            },
        }
    }
    let gaps = report.order.len() - ok - excused_count;
    print!("  {ok}/{} sites located", report.order.len());
    if excused_count > 0 {
        print!(", {excused_count} not needed on this build");
    }
    if gaps > 0 {
        print!(", {gaps} missing");
    }
    println!();
    Ok(())
}

fn read_node(node: &Path) -> Result<Vec<u8>, String> {
    fs::read(node).map_err(|e| format!("cannot read {}: {e}", node.display()))
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
    let mut data = read_node(node)?;
    println!("  {} bytes, md5 {}", data.len(), md5::hex(&data));

    let report0 = resolve::resolve_all(&data, elf::symbols(&data).as_ref());
    let state = patch::classify(&data, &report0);

    // Re-patching a patched binary would compound edits (and the filter
    // functions' original bytes are gone), so always start from the backup.
    if state != patch::State::Stock {
        if backup::exists(install) {
            if args.dry_run {
                println!("  {state}; would restore the backup first");
            } else {
                let src = backup::restore(install, node)
                    .map_err(|e| format!("restoring backup: {e}"))?;
                println!("  {state}; restored {} first", src.display());
                data = read_node(node)?;
            }
        } else {
            return Err(format!(
                "{} is {state}, and no backup is on record here.\n\
                 Its original instructions are gone, so there is nothing left to\n\
                 patch against. Delete {}\n\
                 and start Discord so it re-downloads a stock module, then patch.",
                node.display(),
                node.parent().unwrap_or(node).display()
            ));
        }
    }

    let syms = elf::symbols(&data);
    let report = resolve::resolve_all(&data, syms.as_ref());
    let missing = report.missing();
    let real_missing: Vec<&str> = missing
        .iter()
        .copied()
        .filter(|n| excused(n, &report).is_none())
        .collect();
    let critical_missing: Vec<&str> = real_missing
        .iter()
        .copied()
        .filter(|n| sites::find(n).map(|s| s.critical).unwrap_or(true))
        .collect();
    if !missing.is_empty() {
        println!("  {} of {} sites located", report.order.len() - missing.len(), report.order.len());
        for name in &missing {
            if let Some(note) = excused(name, &report) {
                println!("    n/a     {name} - {note}");
                continue;
            }
            match report.outcomes.get(name) {
                Some(Outcome::Ambiguous { found, wanted }) => {
                    println!("    AMBIG   {name} (matched {found}, expected {wanted})")
                }
                _ => println!("    MISSING {name}"),
            }
        }
        if !critical_missing.is_empty() && !args.allow_partial {
            println!(
                "  Refusing to patch: {} of the missing sites decide whether audio is\n  \
                 mono or stereo, so a partial patch here would negotiate stereo and\n  \
                 still send one channel. Pass --allow-partial to apply the rest anyway.",
                critical_missing.len()
            );
            return Ok(false);
        }
        if critical_missing.is_empty() {
            if real_missing.is_empty() {
                println!("  Everything this build needs resolved. Proceeding.");
            } else {
                println!(
                    "  All stereo-critical sites resolved; the rest are quality\n  \
                     refinements (bitrate, framing, CELT, filter bypass). Proceeding."
                );
            }
        } else {
            println!("  --allow-partial given; applying the sites that did resolve");
        }
    } else {
        println!("  all {} sites located", report.order.len());
    }

    let plan = patch::build(&report, cfg, &data);
    let errors = patch::check(&plan, &data);
    if !errors.is_empty() {
        for e in &errors {
            println!("    {e}");
        }
        return Err(
            "signature matched unexpected bytes; nothing was written. Please report the\n\
             build (size and md5 above) so the catalogue can be updated."
                .to_string(),
        );
    }

    // Describe what this plan actually does, not what a complete one would:
    // on a build where some sites could not be located, claiming the filters
    // are bypassed when they were skipped is exactly the kind of quiet
    // overstatement that sent the last few hours sideways.
    let mut effects: Vec<String> = Vec::new();
    let has = |group: &str| plan.edits.iter().any(|e| e.group == group);
    if has("stereo") {
        effects.push("stereo".to_string());
    }
    if has("samplerate") {
        effects.push("48 kHz".to_string());
    }
    if has("bitrate") {
        effects.push(format!("{} kbps", cfg.bitrate_kbps));
    }
    if has("opus") {
        effects.push("10 ms frames".to_string());
    }
    if has("celt") {
        effects.push("CELT".to_string());
    }
    // "Filterless" means both Opus input filters are out of the path. That can
    // happen three ways per filter: the function was replaced, its coefficient
    // was neutralised, or the build cannot reach it at all. Count all three,
    // otherwise a fully-bypassed build reports as a partial one.
    let applied = |name: &str| plan.edits.iter().any(|e| e.site == name);
    let hp_done = applied("HpCutoff_Inject") || excused("HpCutoff_Inject", &report).is_some();
    let dc_done = applied("DcReject_Inject") || applied("DcReject_Coefficient");
    if hp_done && dc_done {
        effects.push(if cfg.gain != 1.0 {
            format!("filters bypassed (gain x{})", cfg.gain)
        } else {
            "filters bypassed".to_string()
        });
    } else if has("filter") {
        effects.push("high-pass bypassed (opus filters still active)".to_string());
    }
    println!(
        "  {} edits, {} bytes: {}",
        plan.edits.len(),
        plan.total_bytes(),
        effects.join(", ")
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

    let (backup_path, created) = backup::ensure(install, node)
        .map_err(|e| format!("creating backup: {e}"))?;
    println!(
        "  backup {} {}",
        if created { "written to" } else { "already at" },
        backup_path.display()
    );

    patch::apply(&plan, &mut data);
    write_node(node, &data)?;

    let readback = read_node(node)?;
    let failed = patch::verify(&plan, &readback);
    if !failed.is_empty() {
        return Err(format!(
            "verification failed after writing: {}. Restore with 'stereocord restore'.",
            failed.join(", ")
        ));
    }
    println!("  verified, md5 now {}", md5::hex(&readback));
    Ok(true)
}

/// Write through a temporary file and rename, so an interrupted write cannot
/// leave a half-patched module in place.
fn write_node(node: &Path, data: &[u8]) -> Result<(), String> {
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
