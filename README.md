![stereocord — true stereo in Discord's Linux voice module](docs/social-preview.png)

> ⚠️ **Warning**
> This modifies Discord's client files, which is against [Discord's terms of
> service](https://discord.com/terms). Use at your own risk. Not affiliated with Discord Inc.

# stereocord

Forces true stereo, 48 kHz and a high Opus bitrate in Discord's Linux voice
module by patching `discord_voice.node`, and switches off the noise suppression,
automatic gain and echo cancellation that would otherwise reshape the signal
before it is ever encoded. Everything is grouped and optional.

Run it with no arguments and it opens a terminal interface: pick a client, read
what its voice module is currently doing to your audio in plain words, and patch
or restore from there. Give it a command and it behaves as a plain command-line
tool instead.

It started as a Rust reimplementation of the Linux half of ProdHallow's
[Discord-Stereo-Windows-MacOS-Linux](https://github.com/ProdHallow/Discord-Stereo-Windows-MacOS-Linux),
which was discontinued in August 2026 (last functional commit `5e96ff0`), and it
no longer matches that project's patch set. Sites have been added — the audio
network adaptor's mid-call overrides, the encoder locks, and the whole
capture-side processing group — one has been dropped as counterproductive (see
[below](#why-there-is-no-frame-size-patch)), and one was found to have been
writing to the wrong struct field all along. The later groups were informed by
the patch list [sudocord](https://sudocord.dev) publishes; no code is shared
between the two.

## What it changes

Patches are organised into groups you switch on and off. The interface shows
them as checkboxes with a plain-language summary each; `--groups` picks them on
the command line, and `stereocord groups` prints the same descriptions as text.

| Group | On by default | Effect |
| --- | :---: | --- |
| stereo | yes | SDP offers `stereo=1`; capture frames pinned to 2 channels; the capture-side mono downmix and the channel-downmix helper are bypassed; both Opus config constructors default to 2 channels; the audio network adaptor cannot drop the encoder to mono mid-call |
| samplerate | yes | 48 kHz on both arms of Discord's rate selection, instead of 32 kHz below the quality threshold |
| bitrate | yes | 248 kbps (configurable) written into both config constructors *and* into the one wrapper every `opus_encoder_ctl(OPUS_SET_BITRATE)` call goes through, so nothing can walk it back |
| opus | yes | `OPUS_APPLICATION_AUDIO` instead of VOIP, and config validation accepts the combination. Frame size is left at the stock 20 ms — see below |
| celt | yes | `MODE_CELT_ONLY` forced, so the encoder cannot drop into SILK/hybrid and reintroduce a low-pass or fold to mono |
| encoder | yes | complexity 10, coded bandwidth pinned to fullband, packet-loss hint pinned to 0%, inband FEC and DTX never enabled, and the network adaptor cannot change the frame length mid-call |
| filter | yes | WebRTC's high-pass filter returns immediately; libopus' `hp_cutoff` and `dc_reject` are replaced with a pass-through |
| gain | yes | AGC1, AGC2, the adaptive digital stage and the input-volume controller all return at entry, so nothing rides your level or moves your system input slider |
| denoise | yes | `NoiseSuppressor::Analyze` and `::Process` return at entry |
| echo | yes | AEC3's capture path returns at entry. **Headphones only** — on speakers everyone else hears themselves echo back |
| cbr | no | `WebRtcOpus_DisableCbr` can no longer re-enable variable bitrate |

Everything except `gain`, `denoise` and `echo` decides how your audio is
encoded. Those three are about what WebRTC's audio processing would otherwise do
to the signal on the way in — levelling it, gating it, and subtracting an echo
estimate from it — none of which is recoverable afterwards. They are on by
default because leaving the signal alone is the point of the tool, not because
they are always what you want: `echo` in particular is only safe on headphones.

### Why there is no frame-size patch

Upstream forced 10 ms Opus frames, and that was carried over here for a while
without being re-examined. Whatever it was for, it is not a quality setting, and
it costs more than it buys. At a fixed bitrate a 10 ms frame carries the same
per-frame side information — TOC byte, coarse energy, band allocation — over half
as many samples, and it doubles the packet rate, so RTP + crypto tag + UDP + IP
overhead rises from roughly 25 kbps to roughly 50 kbps on top of the payload. The
only thing it buys is about 10 ms of latency. The stock 20 ms is already the
right value, so nothing is written.

## Usage

```bash
cargo build --release
```

```bash
./target/release/stereocord
```

With no arguments, that opens the interface. Three screens:

**Choose a client.** Every Discord channel this tool knows about, with the ones
that are not installed here greyed out rather than hidden. The last entry takes
a path instead, for a custom client shipping Discord's own voice module —
backups are kept for those too, filed under a digest of the path.

**Read what the module is doing.** A dozen lines in plain words — true stereo
working or not, sample rate, bitrate, encoder mode, the low cut and the high
cut, whether automatic gain, noise removal and echo cancellation are still in
the path — green for "your signal gets through", red for "Discord is still
changing it", and an explicit *cannot tell* rather than a guess where the file
does not say. Where a backup exists it is read against that, which is what makes
a patched module legible at all: patching overwrites the very instructions most
signatures key on, so a patched file scanned on its own can only report what
happens to have survived.

**Then patch, or manage.** Patching is the checkbox list, with a bitrate control
and a count of how many of each group's changes exist in your build. Managing is
restoring the original, forgetting the backup, or making Discord reinstall the
module — the way out when a module was patched by something that kept no backup,
or when an update is stuck. Anything destructive stops at a dialog that names the
file, and defaults to *Cancel*.

Reinstalling clears Discord's record of the module as well as the module itself,
and it has to. Discord's updater keeps what it has installed in `installer.db`
and trusts that record without checking the files are still there, so a module
deleted on its own is never replaced — the next launch logs `Install of module
discord_voice finished successfully`, having downloaded nothing, and the client
then reports itself corrupt. Clearing the record makes the updater fetch
everything again on the next start. That file holds an install id and version
manifests and nothing else; logins, servers and settings are in sibling files and
are untouched, and a copy of it is kept under
`~/.local/state/stereocord/backups/`.

If that has already happened to you — a voice module that is gone and a client
that says the installation is corrupt — the install still shows up in the list,
with the reason, and its manage screen is the way back.

Re-opening a module that is already patched starts the checkboxes from what it
already has rather than from the defaults, so re-patching after a Discord update
keeps the choices you made last time.

### Without the interface

```bash
./target/release/stereocord scan
```

`scan` lists every Discord install, marks the one Discord will actually launch,
prints the same plain-words readout, and reports whether each of the 37 patch
sites can be located in that build. A site that cannot be located is reported as
missing rather than quietly worked around.

```bash
./target/release/stereocord patch --groups all --yes
```

Close Discord first. `patch` backs the module up, applies the edits, reads the
file back and verifies every byte landed. Without `--groups` it applies the
recommended set; `--dry-run` shows the plan without writing, `-v` prints every
offset, and `--yes` skips the confirmation.

```bash
./target/release/stereocord restore
```

Puts the original module back. Backups live in
`~/.local/state/stereocord/backups/`, one per install, and are never
overwritten by an already-patched copy.

Other commands: `tui` opens the interface explicitly, `groups` prints every
group and what it does, `backups` lists what is on record, `shellcode` prints
the injected filter replacements as bytes, and `scan --node <path>` inspects an
arbitrary `discord_voice.node` without touching any install.

Useful options: `-g/--groups <list>` (`--groups all` selects every one),
`-b/--bitrate <kbps>` (8–512, default 248), `--gain <factor>` applied by the
injected filters, `-c/--client <text>` to narrow to one install, `-a/--all` for
every install rather than the newest per channel, `--allow-partial` to apply the
sites that did resolve on a build where some did not.

<!-- roundtrip:begin -->
## Before & after

A real round trip through a Discord call, between two Discord desktop clients
on separate machines: the probe goes into the sending client, out through
Discord's servers, and is recorded at the receiving end. Only the sender is
patched — the receiving client is a stock, unmodified install. Measured with
[`tools/roundtrip.py`](tools/roundtrip.py); see
[the wiki](https://github.com/DasPauluteli/stereocord/wiki/Measuring) for the procedure.

![before and after](docs/roundtrip.png)

| | before | after |
| --- | --- | --- |
| channels are | mono | **stereo** |
| L/R correlation | 1.000 | -0.004 |
| bandwidth | 20000 Hz | 20000 Hz |
| round trip | 239 ms | 234 ms |
| below 100 Hz | -40.5 dB | +0.0 dB |

The headline number is the L/R correlation. Two channels carrying the same
signal are mono however many channels the container claims.

Captured 2026-09-01, against the patch catalogue as it stood then — a
measurement describes the selection it was taken with, not necessarily the
current defaults. This one predates the encoder locks and the capture-processing
groups, so it is marked outdated on the chart itself and a re-measurement is
pending.
<!-- roundtrip:end -->

## Validating the measurement

![the measurement self-test](docs/roundtrip-selftest.png)

The second chart is **a simulation, not a Discord capture** — it says nothing
about any particular Discord build, and is here only to show that the analysis
behind the first chart works. It is the output of `python3
tools/roundtrip.py selftest`, which runs the same analysis over synthetic
recordings with known properties — one folded to mono and steeply high-passed,
one stereo and unfiltered — and checks that the measurement code recovers what
was put in.

The two charted cases are shaped like the capture above, so the pair should
look alike: full band on both arms, differing in channel count and in the low
end. A third case, not charted, is band-limited to 7.8 kHz to give the
bandwidth measurement a known edge to recover: a 12th-order low-pass there
crosses the -20 dB line the measurement looks for at 11.4 kHz, and the analysis
finds 11.3 kHz.

## Which clients can hear it

Stereo has to be negotiated by both ends. The patch only controls the sending
side, so what a listener actually hears depends on their client:

| Client | Receives stereo |
| --- | --- |
| Desktop (Linux / Windows / macOS) | yes — measured, on a stock client |
| Browser (discord.com) | no — reported not to negotiate stereo |
| Mobile | varies by client and version |
| Console | unverified |

A listener does not need the patch — the desktop client in the measurement
above was stock, and it decoded stereo because a desktop client negotiates it
when the sender offers it. What a listener needs is a client that negotiates
stereo at all: on one that does not, they hear mono no matter how well the
sending side is patched. This is also why a browser is useless as the receiving end of a
measurement: the result comes back mono and looks like the patch failed.

## Documentation

The long-form documentation lives in [the wiki](https://github.com/DasPauluteli/stereocord/wiki):

- [The interface](https://github.com/DasPauluteli/stereocord/wiki/The-interface) — the three screens, what the readout means, and what the manage screen will and will not do
- [How it works](https://github.com/DasPauluteli/stereocord/wiki/How-it-works) — how sites are located and validated, what the injected filters do, why there is no frame-size patch, what Discord's capture-side processing does to the signal, and how a patched module is read back when patching overwrites the very instructions that identify it
- [Measuring](https://github.com/DasPauluteli/stereocord/wiki/Measuring) — measuring the round trip through a real call

## How it differs from the original

**Sites are found by symbol first, signature second.** Every
`discord_voice.node` seen so far ships a full `.symtab` — 51k to 55k function
symbols, covering the bundled Opus and WebRTC code by name and Discord's own C++
under its mangled names. Seventeen sites are just a function entry, so a symbol
lookup is the whole job; the rest search a signature scoped to one named
function, which cannot match twice. A stripped build falls back to scanning the
whole file, which is why a signature is carried for almost every site — the
exception is the capture-processing bypasses, whose bodies are too large and too
build-specific for any signature that would be safe to also be worth carrying.
`scan -v` reports how each site was resolved (`symbol`, `sig in <fn>`, `scan`).

**Signatures rather than hardcoded offsets.** The upstream project
shipped a table of file offsets per Discord build. When Discord shipped a new
one the offsets went stale, and the fallback was to download a known-good
`discord_voice.node` from GitHub and install it over the user's module. That
replaces the voice engine with one from a different Discord build — the patches
apply cleanly to a binary the rest of the client was never paired with.

Here each site is located by scanning for the instructions around it, so the
catalogue keeps working across builds and the module the user actually has is
the one that gets patched. Where a code sequence was rewritten between builds
the site simply lists both encodings, and where a function changed signature
without changing behaviour the site lists both mangled names. Eight sites need
one or the other today. Nothing is ever downloaded.

**No compiler at install time.** Upstream generated a C++ file, compiled it with
whatever `g++`/`clang++` the machine had, and copied the resulting function
bodies into the binary — so the injected bytes depended on the host toolchain.
The two filter replacements here are emitted byte by byte (see
`src/shellcode.rs`), so the same input always produces the same patch and a C++
toolchain is never needed. Everything that reads or writes the module — ELF
parsing, signature matching, MD5 — is in this repository and depends on nothing;
the one Cargo dependency, [ratatui](https://ratatui.rs), draws the interface and
is not reached by any code path that touches a file.

**It refuses rather than half-works.** Signature matches are checked against the
bytes each site expects before anything is written, every edit is read back
after, and a build where some site cannot be located is reported and skipped
unless `--allow-partial` is passed. A partial patch is how you get a client that
negotiates stereo and still sends one channel.

**It knows about staged updates.** Discord keeps each version in its own
`app-<version>` directory and downloads native modules into it separately. A
client that has staged an update has a new directory whose voice module is still
empty, so a patch applied to the previous directory silently stops applying at
the next restart. `scan` and `patch` both point this out; the default target is
the newest install per channel that has a module.

## Build coverage

| Build | Sites | Notes |
| --- | --- | --- |
| Stable 1.0.158 | 33/37 + 4 n/a | fully covered |
| Stable 1.0.157 | 33/37 + 4 n/a | fully covered |
| Stable 1.0.155 | 33/37 + 4 n/a | fully covered |
| Stable 1.0.153 | 36/37 + 1 n/a | fully covered |
| Stable 0.0.128–0.0.135 | 18/19 + 1 n/a | includes the build upstream last targeted, where every resolved offset matches its hardcoded table exactly |
| Stable 0.0.109, Canary 0.0.783 | 17/19 | mono-downmix site predates the code shape |

The two 0.0.x rows are out of 19, not 37: they predate the network-adaptor,
encoder-lock and capture-processing sites and have not been re-scanned since, so
their coverage of those is unverified.

Not every site applies to every build, and "n/a" is different from "missing".
A site marked n/a is one this build does not need — either because another
patch already covers it, or because the construct it targets does not exist
here. Reporting those as failures would overstate how badly the catalogue has
aged.

1.0.157 changed one register: the frame pointer that `CapturedAudioProcessor::Process`
passes to `AudioFrame::muted()` moved from `r14` to `r15`, which is enough to
miss a signature that carried the call setup. The mono-downmix site lists both
encodings now. Nothing else in the catalogue moved.

1.0.155 rebuilt a good deal of the audio path. Both Opus config constructors are
inlined into their callers' stack frames (hence a second signature for the
inlined form), the `OpusEncoder` struct shifted by 4 bytes (hence wildcarded
field displacements in the CELT signatures), libopus gained
`opus_encode_frame_native`, and `hp_cutoff` / `dc_reject` are inlined into it.
That leaves no function to replace, so both filters are handled differently
there — see [How it works](https://github.com/DasPauluteli/stereocord/wiki/How-it-works).

Sites are marked critical or not. The critical ones decide whether audio is mono
or stereo; the rest are quality refinements (bitrate, CELT, the encoder locks,
and the filter and capture-processing bypasses). When only non-critical sites are
missing the tool proceeds and says so.
When a critical one is missing it refuses unless `--allow-partial` is passed,
because a client that negotiates stereo and still sends one channel is worse
than one that does neither.

## Measuring whether it worked

The patch changes an encoder configuration; whether that reaches the person
listening is a separate question. `tools/roundtrip.py` sends a known probe
through a real call and measures what comes back — round-trip delay, L/R
correlation (the mono-versus-stereo test), effective bandwidth, and low-end
attenuation. See [Measuring](https://github.com/DasPauluteli/stereocord/wiki/Measuring).

It needs a second endpoint in the call, because a client does not decode its own
transmission — and that endpoint has to be a Discord **desktop** client on
another machine, since the browser client will not negotiate stereo and Discord
refuses to run twice on one machine. It does not have to be patched; the
published measurement was taken against a stock receiving client.

The analysis itself is validated against synthetic recordings with known
properties — `python3 tools/roundtrip.py selftest` checks the recovered numbers
against the injected ones and exits non-zero if any drift. That is the second
chart above, and it is a simulation rather than a Discord measurement.

## Caveats

- A mono source still gives you two identical channels. Analysers report that as
  mono, correctly. Feed Discord a stereo input.
- **The defaults turn off echo cancellation.** On headphones that is what you
  want. On speakers, everyone else in the call will hear themselves echoing back
  — untick **No echo cancellation** in the interface, or leave `echo` out of
  `--groups`.
- Turning off noise suppression and automatic gain means your room and your
  input level go out as they are. Set your level yourself; nothing downstream
  will rescue a quiet or clipping source.
- Patching a running client does nothing: the old module is already mapped. The
  tool refuses unless `--force` is given.
- **A patched module blocks Discord's updates.** Discord ships voice-module
  updates as a binary delta and verifies the SHA-256 of the file it is about to
  patch. A patched module fails that check, which aborts the entire host update,
  so Discord goes on launching the old version and the staged `app-` directory
  sits half-populated — it looks like a stalled download. To take an update:
  `stereocord restore`, start Discord and let it update, quit, then
  `stereocord patch` again. `scan` detects this state and says so, and the
  interface's manage screen can make Discord reinstall the module outright if
  restoring is no longer possible.
- **Deleting a voice module by hand does not make Discord replace it.** Its
  updater believes its own record of what is installed, so the module stays
  missing, voice stops working and the client reports itself corrupt. Use
  *Reinstall the voice module* in the manage screen, which clears that record
  too.
- Editing client files is against Discord's terms of service (see the warning at
  the top). Your account, your call.

## License

[CC BY-NC-SA 4.0](LICENSE) — Creative Commons
Attribution-NonCommercial-ShareAlike 4.0 International.

| | |
| --- | --- |
| modify and redistribute | yes, under the same license |
| commercial use | no |
| patent rights | not granted |
| attribution and this notice | required |

Copyright © 2026 Paul Neri
`<67437654+DasPauluteli@users.noreply.github.com>`. The notice is reproduced at
the top of every source file; keep it there if you copy any of this, and put
what you build on top of it under the same terms.

The license does not oblige you to publish source. If you ship a modified build
of a tool whose whole point is that you can check what it writes into someone
else's binary, publish the source anyway — that is a request rather than a
term, and it is stated as one in [LICENSE](LICENSE).
