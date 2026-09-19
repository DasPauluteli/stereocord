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

//! The catalogue of patch sites, and how to find each one in an arbitrary
//! build of `discord_voice.node`.
//!
//! The upstream project hardcoded a file offset per site and shipped a fresh
//! set with every Discord release; when the offsets went stale its answer was
//! to download a known-good `discord_voice.node` from GitHub and install that
//! over the user's module. That trades a stale patch for a voice engine from a
//! different Discord build, which is worse.
//!
//! Here every site is instead located by scanning for the instructions around
//! it, so the same catalogue keeps working across builds and the tool patches
//! the module the user actually has. Where a code sequence was rewritten
//! between builds a site simply lists both encodings.

use crate::sig::Pattern;

/// What a site does once located.
pub enum Action {
    /// Overwrite with fixed bytes.
    Bytes(&'static [u8]),
    /// Overwrite with the little-endian encoding of the configured bitrate.
    BitrateImm32,
    /// Overwrite with `push rbp; mov edx, <bitrate>`, forcing the argument of
    /// every `opus_encoder_ctl(OPUS_SET_BITRATE)` call in the module.
    BitrateSetter,
    /// The same six-byte prologue rewrite as [`Action::BitrateSetter`], but with
    /// a constant instead of the configured bitrate. Every `WebRtcOpus_Set*`
    /// wrapper is the same shape — `push rbp; mov rbp,rsp; mov edx,esi` in front
    /// of one `opus_encoder_ctl(inst, <request>, edx)` — so pinning `edx` pins
    /// that setting for every caller in the module.
    CtlArg(i32),
    /// Overwrite with the injected filter replacement.
    ShellcodeHpCutoff,
    /// Overwrite with the injected filter replacement.
    ShellcodeDcReject,
    /// The resolved offset holds the disp32 of a RIP-relative operand; follow
    /// it and write this f32 at the target instead of at the offset itself.
    RipRelF32(f32),
}

/// How many matches a site expects, and how to pick if there are several.
pub enum Expect {
    /// Exactly one match.
    One,
    /// Exactly `n` matches, all of which get patched. Used where the same
    /// construct is emitted at more than one call site and all of them matter.
    All(usize),
    /// Several matches are normal; keep the one that falls within `window`
    /// bytes after the (already resolved) named site. Both ambiguous sites sit
    /// inside the same function as their anchor, so proximity disambiguates
    /// them without needing a disassembler.
    NearestAfter { anchor: &'static str, window: usize },
}

/// Why a site can be legitimately absent from a build, and what covers it.
///
/// Reported instead of a bare MISSING. Without this, a build that simply does
/// not need a patch looks identical to one whose code has moved, which
/// overstates how badly the catalogue has aged.
pub struct Absent {
    /// When this other site is applied, the missing one is unnecessary.
    /// `None` means the site is never needed for stereo on any build.
    pub covered_by: Option<&'static str>,
    pub note: &'static str,
}

pub struct Site {
    pub name: &'static str,
    pub group: &'static str,
    pub what: &'static str,
    /// Whether stereo actually works without this site. The quality patches
    /// (bitrate, framing, CELT, filters) are worth having but a client missing
    /// them still sends two channels; the sites marked critical are the ones
    /// that decide mono versus stereo. A build that moved only non-critical
    /// code can still be patched usefully, which matters because Discord keeps
    /// shipping new builds.
    pub critical: bool,
    /// Set when absence is not a failure. See [`Absent`].
    pub absent_ok: Option<Absent>,
    pub expect: Expect,
    /// The function this site lives in, by symbol name; alternatives are tried
    /// in order. When one resolves, the patterns below are searched only inside
    /// that function, which removes almost all ambiguity. An empty list, or a
    /// stripped binary, falls back to scanning the whole file.
    pub symbols: &'static [&'static str],
    /// True when the patch goes at the function's entry point. With a symbol
    /// that needs no searching at all; the patterns then serve only as a
    /// fallback for a stripped binary.
    pub entry: bool,
    /// Alternative encodings, tried in order; the first that matches wins.
    ///
    /// Empty means symbol-only. The capture-processing bypasses use that: they
    /// replace whole function bodies that are large, heavily inlined and
    /// different in every build, so any prologue signature specific enough to
    /// be safe would also be too brittle to be worth carrying. Every build seen
    /// so far ships a full `.symtab`; on one that does not, these report
    /// MISSING and, being non-critical, the rest of the patch still applies.
    pub patterns: &'static [(&'static str, usize)],
    pub action: Action,
    /// The bytes a stock build has here, where that is unambiguous. Used to
    /// tell a stock module apart from one some tool has already patched —
    /// several sites wildcard exactly this value, so they resolve either way
    /// and can be read as sentinels. Empty where the stock value varies
    /// between builds.
    pub stock: &'static [u8],
    /// Bytes expected at the resolved offset before patching. Purely a
    /// sanity check — a site that resolves but holds unexpected bytes means the
    /// signature found the wrong place, and the run is aborted rather than
    /// writing into it.
    pub expect_orig: &'static [&'static [u8]],
}

// Mangled names have been stable across the builds seen so far. The two Opus
// config constructors are the exception: 1.0.155 inlines both, so those sites
// carry a signature for the inlined form alongside the symbol.
const SYM_CREATE_AUDIO_FRAME: &str =
    "_ZN7discord5media20EngineAudioTransport25CreateAudioFrameToProcessERKNS0_15AudioStreamTypeEPKvRKmS8_S8_";
const SYM_CAPTURED_AUDIO_PROCESS: &str =
    "_ZN7discord5media22CapturedAudioProcessor7ProcessENS0_15AudioStreamTypeEbjijbbRN6webrtc10AudioFrameE";
const SYM_OPUS_CONFIG_CTOR: &str = "_ZN6webrtc22AudioEncoderOpusConfigC2Ev";
const SYM_MULTICHANNEL_CTOR: &str = "_ZN6webrtc34AudioEncoderMultiChannelOpusConfigC2Ev";
const SYM_OPUS_CONFIG_ISOK: &str = "_ZNK6webrtc22AudioEncoderOpusConfig4IsOkEv";
const SYM_HIGHPASS_PROCESS: &str = "_ZN6webrtc14HighPassFilter7ProcessEPNS_11AudioBufferEb";
const SYM_GAIN_CONTROLLER2: &str = "_ZN6webrtc15GainController27ProcessEbPNS_11AudioBufferE";
/// 1.0.153 took the analog level as a leading `optional<float>`. Same function,
/// still void, so the same `ret` applies.
const SYM_GAIN_CONTROLLER2_OLD: &str =
    "_ZN6webrtc15GainController27ProcessENSt4__Cr8optionalIfEEbPNS_11AudioBufferE";
const SYM_AGC_PRE_PROCESS: &str =
    "_ZN6webrtc16AgcManagerDirect17AnalyzePreProcessERKNS_11AudioBufferE";
const SYM_AGC_DIGITAL_SETUP: &str =
    "_ZNK6webrtc16AgcManagerDirect23SetupDigitalGainControlERNS_11GainControlE";
const SYM_MONO_AGC_PROCESS: &str =
    "_ZN6webrtc7MonoAgc7ProcessENS_9ArrayViewIKsLln4711EEENSt4__Cr8optionalIiEE";
/// Before `rtc::ArrayView` was folded into the `webrtc` namespace.
const SYM_MONO_AGC_PROCESS_OLD: &str =
    "_ZN6webrtc7MonoAgc7ProcessEN3rtc9ArrayViewIKsLln4711EEENSt4__Cr8optionalIiEE";
const SYM_MONO_AGC_CLIPPING: &str = "_ZN6webrtc7MonoAgc14HandleClippingEi";
const SYM_MONO_AGC_UPDATE_GAIN: &str = "_ZN6webrtc7MonoAgc10UpdateGainEi";
const SYM_INPUT_VOLUME_ANALYZE: &str =
    "_ZN6webrtc21InputVolumeController17AnalyzeInputAudioEiRKNS_11AudioBufferE";
const SYM_ADAPTIVE_DIGITAL_GAIN: &str =
    "_ZN6webrtc29AdaptiveDigitalGainController7ProcessERKNS0_9FrameInfoENS_17DeinterleavedViewIfEE";
const SYM_NS_PROCESS: &str = "_ZN6webrtc15NoiseSuppressor7ProcessEPNS_11AudioBufferE";
const SYM_NS_ANALYZE: &str = "_ZN6webrtc15NoiseSuppressor7AnalyzeERKNS_11AudioBufferE";
const SYM_AEC3_PROCESS_CAPTURE: &str =
    "_ZN6webrtc14EchoCanceller314ProcessCaptureEPNS_11AudioBufferES2_b";
const SYM_FRAME_LENGTH_DECISION: &str =
    "_ZN6webrtc21FrameLengthController12MakeDecisionEPNS_25AudioEncoderRuntimeConfigE";
const SYM_CHANNEL_MAKE_DECISION: &str =
    "_ZN6webrtc17ChannelController12MakeDecisionEPNS_25AudioEncoderRuntimeConfigE";

/// `mov r12, 2` — same length as the `cmp`/`cmovae` pair it replaces.
const FORCE_TWO_CHANNELS: &[u8] = &[0x49, 0xC7, 0xC4, 0x02, 0x00, 0x00, 0x00];
/// Twelve `nop`s, then the `E9` that turns the following `jg rel32` into an
/// unconditional `jmp rel32` over the mono downmix.
const NOP12_JMP: &[u8] = &[
    0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0xE9,
];
/// Two `nop`s, used to delete a two-byte conditional jump and leave whatever
/// follows it to run.
const NOP2: &[u8] = &[0x90, 0x90];
/// `mov rax, 1; ret`
const RETURN_TRUE: &[u8] = &[0x48, 0xC7, 0xC0, 0x01, 0x00, 0x00, 0x00, 0xC3];
/// 48000, little endian.
const SR_48K: &[u8] = &[0x80, 0xBB, 0x00, 0x00];
/// `MODE_CELT_ONLY`, little endian.
const CELT_ONLY: &[u8] = &[0xEA, 0x03, 0x00, 0x00];
const RET: &[u8] = &[0xC3];
/// `push rbp; mov rbp,rsp; mov edx,esi` — the prologue every `WebRtcOpus_Set*`
/// wrapper shares, and what [`Action::CtlArg`] and [`Action::BitrateSetter`]
/// overwrite with `push rbp; mov edx, <constant>`.
const CTL_PROLOGUE: &[u8] = &[0x55, 0x48, 0x89, 0xE5, 0x89, 0xF2];

/// A group of sites the user turns on or off as one unit.
///
/// The catalogue is organised by what the machine code does; this is organised
/// by what the person sitting there is choosing. Every site belongs to exactly
/// one group, and the summary has to make sense to someone who has never heard
/// of Opus.
pub struct Group {
    pub name: &'static str,
    /// Short label for the picker.
    pub title: &'static str,
    /// What this does to your audio, in plain words.
    pub summary: &'static str,
    /// Shown under the summary when the choice has a catch worth knowing.
    pub caveat: Option<&'static str>,
    /// Whether it is on unless the user says otherwise. On for everything that
    /// gets the signal through untouched; off for anything that is a trade.
    pub default_on: bool,
}

/// Picker order: what gets sent, then how it is encoded, then what Discord
/// would otherwise do to the signal on the way in.
pub static GROUPS: &[Group] = &[
    Group {
        name: "stereo",
        title: "True stereo",
        summary: "Sends left and right as separate channels instead of mixing them \
                  into one. This is the main reason to use stereocord.",
        caveat: Some("A mono microphone still gives you two identical channels."),
        default_on: true,
    },
    Group {
        name: "samplerate",
        title: "Full 48 kHz",
        summary: "Keeps the full sample rate instead of dropping to 32 kHz, which \
                  throws away the top of the treble.",
        caveat: None,
        default_on: true,
    },
    Group {
        name: "bitrate",
        title: "High bitrate",
        summary: "Raises how much data each second of audio may use, far above what \
                  Discord normally allows. Choose the amount with --bitrate.",
        caveat: None,
        default_on: true,
    },
    Group {
        name: "opus",
        title: "Music mode",
        summary: "Tells the encoder it is handling music rather than a phone call, \
                  and lets it accept the settings above.",
        caveat: None,
        default_on: true,
    },
    Group {
        name: "celt",
        title: "Stay in music mode",
        summary: "Stops the encoder switching to its speech mode partway through, \
                  which would fold the stereo image down and cut the highs.",
        caveat: None,
        default_on: true,
    },
    Group {
        name: "encoder",
        title: "Encoder pinned wide open",
        summary: "Quality dial to maximum and the full frequency range, and stops \
                  Discord spending part of your bitrate on error correction, \
                  silence detection, or shorter frames when the network dips.",
        caveat: None,
        default_on: true,
    },
    Group {
        name: "filter",
        title: "No bass filtering",
        summary: "Removes the filters that cut the low end out of your signal before \
                  it reaches the encoder.",
        caveat: None,
        default_on: true,
    },
    Group {
        name: "gain",
        title: "No automatic volume",
        summary: "Stops Discord riding your volume up and down on its own, and stops \
                  it moving your system input slider. Your level goes out the way \
                  you set it.",
        caveat: Some("Set your input level yourself; nothing will rescue a quiet or \
                      clipping source."),
        default_on: true,
    },
    Group {
        name: "denoise",
        title: "No noise removal",
        summary: "Turns off noise suppression. It is tuned for speech and treats \
                  quiet detail — room tone, reverb tails, fade-outs — as noise to \
                  be deleted.",
        caveat: Some("Steady background noise in your room will now be audible to \
                      everyone."),
        default_on: true,
    },
    Group {
        name: "echo",
        title: "No echo cancellation",
        summary: "Turns off the echo canceller, which subtracts a guess at what your \
                  speakers are playing from what your microphone hears. It is the \
                  most destructive thing in the chain for music.",
        caveat: Some("Headphones only. On speakers, everyone else will hear \
                      themselves echoing back."),
        default_on: true,
    },
    Group {
        name: "cbr",
        title: "Constant bitrate",
        summary: "Sends the same amount of data every moment instead of easing off \
                  during quiet passages. Steadier on the network, not better \
                  sounding.",
        caveat: Some("Off by default: it uses noticeably more bandwidth for no gain \
                      in quality."),
        default_on: false,
    },
];

/// The groups that are on when nobody has said otherwise.
pub fn default_groups() -> Vec<&'static str> {
    GROUPS.iter().filter(|g| g.default_on).map(|g| g.name).collect()
}

pub fn group(name: &str) -> Option<&'static Group> {
    GROUPS.iter().find(|g| g.name == name)
}

pub static SITES: &[Site] = &[
    // ---- stereo ------------------------------------------------------------
    Site {
        name: "CommitAudioCodec_StereoCheck",
        group: "stereo",
        what: "SDP fmtp stereo=1 (both codec commit paths)",
        // `cmp dword [rbx+0xF0], 2` selects between the strings "1" and "0"
        // via `cmovae`. Comparing against 0 instead makes the branch always
        // taken, so the offer always advertises stereo.
        critical: true,
        absent_ok: None,
        expect: Expect::All(2),
        symbols: &[],
        entry: false,
        patterns: &[(
            "83 BB F0 00 00 00 ?? 48 8D 05 ?? ?? ?? ?? 48 8D 15 ?? ?? ?? ?? 48 0F 43 D0",
            6,
        )],
        action: Action::Bytes(&[0x00]),
        stock: &[0x02],
        expect_orig: &[&[0x02], &[0x00]],
    },
    Site {
        name: "CreateAudioFrame_Channels",
        group: "stereo",
        what: "capture frame channel count pinned to 2",
        // `cmp r12, rN` / `cmovae r12, rN` clamps the channel count down to
        // whatever the device reports. Replaced with `mov r12, 2`.
        critical: true,
        absent_ok: None,
        expect: Expect::NearestAfter { anchor: "SelectSampleRate_48k", window: 0x60 },
        symbols: &[SYM_CREATE_AUDIO_FRAME],
        entry: false,
        patterns: &[
            ("49 39 C4 4C 0F 43 E0 4D 89 66 28", 0),
            ("49 39 D4 4C 0F 43 E2 C1 E9 02 69 C9 7B 14 00 00", 0),
        ],
        action: Action::Bytes(FORCE_TWO_CHANNELS),
        stock: &[],
        expect_orig: &[
            &[0x49, 0x39, 0xC4, 0x4C, 0x0F, 0x43, 0xE0],
            &[0x49, 0x39, 0xD4, 0x4C, 0x0F, 0x43, 0xE2],
            FORCE_TWO_CHANNELS,
        ],
    },
    Site {
        name: "MonoDownmix_Bypass",
        group: "stereo",
        what: "skip the capture-side mono downmix",
        critical: true,
        absent_ok: None,
        expect: Expect::One,
        symbols: &[SYM_CAPTURED_AUDIO_PROCESS],
        entry: false,
        // `Process` guards two blocks with the same `muted() && type > 9` test;
        // the downmix is the second, told apart by its `je 0x0d` and near `jg`
        // where the first has `je 0x09` and a short one. Which register holds
        // the frame pointer moved from r14 to r15 in 1.0.157, so the call setup
        // is carried in both encodings.
        patterns: &[
            ("4C 89 F7 E8 ?? ?? ?? ?? 84 C0 74 0D 83 BB ?? ?? ?? ?? 09 0F 8F", 8),
            ("4C 89 FF E8 ?? ?? ?? ?? 84 C0 74 0D 83 BB ?? ?? ?? ?? 09 0F 8F", 8),
        ],
        action: Action::Bytes(NOP12_JMP),
        // Only the leading opcodes are fixed; the `cmp` displacement moves
        // between builds, so the check is a prefix rather than the full run.
        stock: &[],
        expect_orig: &[&[0x84, 0xC0, 0x74, 0x0D, 0x83, 0xBB], NOP12_JMP],
    },
    Site {
        name: "ChannelDownmix_Entry",
        group: "stereo",
        what: "channel downmix helper returns immediately",
        critical: true,
        absent_ok: None,
        expect: Expect::One,
        symbols: &["downmix_and_resample"],
        entry: true,
        patterns: &[(
            "55 48 89 E5 41 57 41 56 41 55 41 54 53 48 83 EC 28 64 48 8B 04 25 28 00 00 00 48 89 45 D0 0F 57",
            0,
        )],
        action: Action::Bytes(RET),
        stock: &[],
        expect_orig: &[&[0x55], RET],
    },
    Site {
        name: "OpusConfig_Channels",
        group: "stereo",
        what: "AudioEncoderOpusConfig::num_channels = 2",
        critical: true,
        absent_ok: None,
        expect: Expect::One,
        symbols: &[SYM_OPUS_CONFIG_CTOR],
        entry: false,
        patterns: &[(OPUS_CONFIG_CTOR, 0x15), (OPUS_CONFIG_CTOR_INLINED, 24)],
        action: Action::Bytes(&[0x02]),
        stock: &[0x01],
        expect_orig: &[&[0x01], &[0x02]],
    },
    Site {
        name: "MultiChannelConfig_Channels",
        group: "stereo",
        what: "AudioEncoderMultiChannelOpusConfig::num_channels = 2",
        critical: false,
        absent_ok: Some(Absent {
            covered_by: None,
            note: "multi-channel (surround) Opus config, not used for stereo voice; \
                   newer builds derive the channel count from the SDP rather than \
                   from a constant, so there is no immediate to patch",
        }),
        expect: Expect::One,
        symbols: &[SYM_MULTICHANNEL_CTOR],
        entry: false,
        patterns: &[(MULTICHANNEL_CTOR, 0x0E)],
        action: Action::Bytes(&[0x02]),
        stock: &[0x01],
        expect_orig: &[&[0x01], &[0x02]],
    },
    Site {
        name: "ChannelController_ForceStereo",
        group: "stereo",
        what: "network adaptor cannot drop the encoder to mono mid-call",
        // The one site here that acts during a call rather than before it.
        //
        // When signalling supplies an `audio_network_adaptor_config` that names
        // a ChannelController, `ChannelController::MakeDecision` is polled with
        // the current uplink estimate. With two channels in flight it compares
        // that estimate against `channel_2_to_1_bandwidth_bps` and, when the
        // estimate is at or below it, commits one channel. The decision reaches
        // the encoder as `opus_encoder_ctl(OPUS_SET_FORCE_CHANNELS, 1)`, and
        // libopus then folds the two channels to (L+R)/2 internally. Nothing
        // else in this catalogue covers that: the offer still carries stereo=1
        // and the encoder still holds two channels, the adaptor has simply
        // stopped using the second one.
        //
        // The edit is two `nop`s over the `jle` that commits the downgrade,
        // leaving the `jmp` beneath it to exit with the count unchanged at 2.
        // Deliberately not a pin of the written value: `OPUS_SET_FORCE_CHANNELS`
        // rejects a count above the encoder's own channel count, and its caller
        // treats that rejection as a fatal check. Removing only the downgrade
        // leaves the upgrade path's `min(2, num_encoder_channels)` clamp intact,
        // so on a build where the encoder really is mono this site changes
        // nothing rather than aborting the client.
        critical: false,
        absent_ok: Some(Absent {
            covered_by: None,
            note: "no ChannelController in this build; MakeDecision is virtual and so \
                   cannot be inlined away, and with neither the symbol nor the signature \
                   present the network adaptor has no channel arm to downgrade with",
        }),
        expect: Expect::One,
        symbols: &[SYM_CHANNEL_MAKE_DECISION],
        entry: false,
        // Byte-identical in every build seen so far, so the only wildcards are
        // the two bytes being patched — which lets the site resolve on an
        // already-patched module and act as a sentinel.
        patterns: &[(
            "48 8B 47 20 80 7F 2C 01 75 45 48 83 F8 01 74 1A 48 83 F8 02 75 39 \
             8B 57 28 B9 01 00 00 00 B8 02 00 00 00 3B 57 1C ?? ?? EB 25",
            38,
        )],
        action: Action::Bytes(NOP2),
        stock: &[0x7E, 0x20],
        expect_orig: &[&[0x7E, 0x20], NOP2],
    },
    // ---- sample rate -------------------------------------------------------
    Site {
        name: "SelectSampleRate_48k",
        group: "samplerate",
        what: "48 kHz on both sides of the rate selection",
        // The selection is `>= 32001 ? 48000 : 32000`; raising the fallback to
        // 48000 makes both arms agree. The two encodings differ only in which
        // registers the build happened to allocate.
        critical: true,
        absent_ok: None,
        expect: Expect::One,
        symbols: &[SYM_CREATE_AUDIO_FRAME],
        entry: false,
        patterns: &[
            ("41 81 FF 01 7D 00 00 B8 80 BB 00 00 41 BD ?? ?? ?? ?? 44 0F 43 E8", 14),
            ("41 81 FF 01 7D 00 00 BA 80 BB 00 00 B9 ?? ?? ?? ?? 0F 43 CA", 13),
        ],
        action: Action::Bytes(SR_48K),
        stock: &[0x00, 0x7D, 0x00, 0x00],
        expect_orig: &[&[0x00, 0x7D, 0x00, 0x00], SR_48K],
    },
    // ---- bitrate -----------------------------------------------------------
    Site {
        name: "OpusConfig_Bitrate",
        group: "bitrate",
        what: "AudioEncoderOpusConfig default bitrate",
        critical: false,
        absent_ok: None,
        expect: Expect::One,
        symbols: &[SYM_OPUS_CONFIG_CTOR],
        entry: false,
        patterns: &[(OPUS_CONFIG_CTOR, 0x1F), (OPUS_CONFIG_CTOR_INLINED, 34)],
        action: Action::BitrateImm32,
        stock: &[0x00, 0x7D, 0x00, 0x00],
        expect_orig: &[],
    },
    Site {
        name: "MultiChannelConfig_Bitrate",
        group: "bitrate",
        what: "AudioEncoderMultiChannelOpusConfig default bitrate",
        critical: false,
        absent_ok: Some(Absent {
            covered_by: None,
            note: "multi-channel (surround) Opus config, not used for stereo voice",
        }),
        expect: Expect::One,
        symbols: &[SYM_MULTICHANNEL_CTOR],
        entry: false,
        patterns: &[(MULTICHANNEL_CTOR, 0x18)],
        action: Action::BitrateImm32,
        stock: &[0x00, 0x7D, 0x00, 0x00],
        expect_orig: &[],
    },
    Site {
        name: "WebRtcOpus_SetBitRate",
        group: "bitrate",
        what: "central OPUS_SET_BITRATE lock",
        // Every bitrate change in the module funnels through this one wrapper
        // around `opus_encoder_ctl(OPUS_SET_BITRATE)`. Overwriting the value
        // argument here replaces the dozen separate clamp and tier patches the
        // upstream Windows script needed. Anchored on the request constant
        // 4002 so it still resolves after the prologue has been overwritten.
        critical: false,
        absent_ok: None,
        expect: Expect::One,
        symbols: &["WebRtcOpus_SetBitRate"],
        entry: false,
        patterns: &[(
            "?? ?? ?? ?? ?? ?? 48 8B 07 48 85 C0 74 ?? 48 89 C7 BE A2 0F 00 00 31 C0 E8",
            0,
        )],
        action: Action::BitrateSetter,
        stock: &[0x55, 0x48, 0x89, 0xE5, 0x89, 0xF2],
        expect_orig: &[&[0x55, 0x48, 0x89, 0xE5, 0x89, 0xF2]],
    },
    // ---- opus ---------------------------------------------------------------
    //
    // There is deliberately no frame-size site. Upstream forced 10 ms frames;
    // the stock 20 ms is the better setting and needs no patch. At a fixed
    // bitrate a 10 ms frame spends the same per-frame side information (TOC
    // byte, coarse energy, band allocation) over half as many samples, and it
    // doubles the packet rate, so RTP + crypto tag + UDP + IP overhead goes
    // from roughly 25 kbps to roughly 50 kbps on top of the payload. The only
    // thing it buys is about 10 ms of latency.
    Site {
        name: "OpusConfig_Application",
        group: "opus",
        what: "OPUS_APPLICATION_AUDIO instead of VOIP",
        // `application` is the int at offset 0x10 of AudioEncoderOpusConfig,
        // written as the low half of the same `movabs` that carries the default
        // bitrate in its high half. `WebRtcOpus_EncoderCreate` maps 0 to
        // OPUS_APPLICATION_VOIP (2048) and 1 to OPUS_APPLICATION_AUDIO (2049).
        //
        // Not the byte at 0x18, which is four bytes further along and looks
        // like a mode flag but is the engaged flag of the
        // `std::optional<int> bitrate_bps` that starts at 0x14 — stock 1, so
        // writing 1 there changes nothing and leaves the encoder in VOIP mode.
        // The field order is frame_size_ms(0x00), sample_rate_hz(0x04),
        // num_channels(0x08), application(0x10), bitrate value(0x14),
        // bitrate engaged(0x18); `IsOk` reading `cmpb $1, 0x18(%rdi)` as its
        // "has a bitrate" test is the cheapest confirmation of that layout.
        critical: false,
        absent_ok: None,
        expect: Expect::One,
        symbols: &[SYM_OPUS_CONFIG_CTOR],
        entry: false,
        patterns: &[(OPUS_CONFIG_CTOR, 27), (OPUS_CONFIG_CTOR_INLINED, 30)],
        action: Action::Bytes(&[0x01]),
        stock: &[0x00],
        expect_orig: &[&[0x00], &[0x01]],
    },
    Site {
        name: "OpusConfig_IsOk",
        group: "opus",
        what: "config validation always accepts",
        // Otherwise the 248 kbps / 2 channel / 10 ms combination is rejected
        // before it reaches the encoder.
        critical: true,
        absent_ok: None,
        expect: Expect::One,
        symbols: &[SYM_OPUS_CONFIG_ISOK],
        entry: true,
        patterns: &[(
            "55 48 89 E5 8B 0F 31 C0 85 C9 7E ?? BA CD CC CC CC 48 89 CE 48 0F AF F2",
            0,
        )],
        action: Action::Bytes(RETURN_TRUE),
        stock: &[],
        expect_orig: &[&[0x55, 0x48, 0x89, 0xE5, 0x8B, 0x0F, 0x31, 0xC0], RETURN_TRUE],
    },
    // ---- capture-side processing ---------------------------------------------
    //
    // Everything above shapes how the signal is encoded. This block is about
    // what WebRTC's audio processing module does to the signal on the way in,
    // before the encoder ever sees it: levelling it, gating it, and subtracting
    // an echo estimate from it. None of that is recoverable afterwards.
    //
    // Each of these is replaced with a bare `ret` at its entry. They all return
    // void and none of them is the only writer of anything downstream reads, so
    // returning early leaves the capture buffer exactly as it arrived — which
    // is the whole point. `expect_orig` is empty because the prologues differ
    // per build and the symbol is what located them; the resolved address is a
    // function entry by construction, not a signature guess.
    Site {
        name: "GainController2_Bypass",
        group: "gain",
        what: "AGC2 leaves the signal alone",
        // The current automatic gain control. Rides the level up and down,
        // which on music is audible pumping.
        critical: false,
        absent_ok: None,
        expect: Expect::One,
        symbols: &[SYM_GAIN_CONTROLLER2, SYM_GAIN_CONTROLLER2_OLD],
        entry: true,
        patterns: &[],
        action: Action::Bytes(RET),
        stock: &[],
        expect_orig: &[],
    },
    Site {
        name: "AgcPreAnalysis_Bypass",
        group: "gain",
        what: "AGC pre-analysis does not run",
        // Watches for clipping and drives the input-volume recommendation.
        critical: false,
        absent_ok: None,
        expect: Expect::One,
        symbols: &[SYM_AGC_PRE_PROCESS],
        entry: true,
        patterns: &[],
        action: Action::Bytes(RET),
        stock: &[],
        expect_orig: &[],
    },
    Site {
        name: "AgcDigitalSetup_Bypass",
        group: "gain",
        what: "legacy AGC digital compression not configured",
        // Installs the legacy AGC1 digital compression curve.
        critical: false,
        absent_ok: None,
        expect: Expect::One,
        symbols: &[SYM_AGC_DIGITAL_SETUP],
        entry: true,
        patterns: &[],
        action: Action::Bytes(RET),
        stock: &[],
        expect_orig: &[],
    },
    Site {
        name: "MonoAgcProcess_Bypass",
        group: "gain",
        what: "per-channel AGC does not run",
        // AGC1's per-channel worker: the thing that actually decides a gain.
        critical: false,
        absent_ok: None,
        expect: Expect::One,
        symbols: &[SYM_MONO_AGC_PROCESS, SYM_MONO_AGC_PROCESS_OLD],
        entry: true,
        patterns: &[],
        action: Action::Bytes(RET),
        stock: &[],
        expect_orig: &[],
    },
    Site {
        name: "MonoAgcClipping_Bypass",
        group: "gain",
        what: "AGC does not react to clipping",
        // Reacts to clipping by yanking the level down and holding it there.
        critical: false,
        absent_ok: None,
        expect: Expect::One,
        symbols: &[SYM_MONO_AGC_CLIPPING],
        entry: true,
        patterns: &[],
        action: Action::Bytes(RET),
        stock: &[],
        expect_orig: &[],
    },
    Site {
        name: "MonoAgcUpdateGain_Bypass",
        group: "gain",
        what: "AGC gain never updated",
        // The gain update itself, stubbed so a level already chosen cannot
        // drift even if something else calls in.
        critical: false,
        absent_ok: None,
        expect: Expect::One,
        symbols: &[SYM_MONO_AGC_UPDATE_GAIN],
        entry: true,
        patterns: &[],
        action: Action::Bytes(RET),
        stock: &[],
        expect_orig: &[],
    },
    Site {
        name: "InputVolumeAnalyze_Bypass",
        group: "gain",
        what: "input volume not driven from the signal",
        // The newer input-volume controller, which moves the OS capture
        // slider on your behalf.
        critical: false,
        absent_ok: None,
        expect: Expect::One,
        symbols: &[SYM_INPUT_VOLUME_ANALYZE],
        entry: true,
        patterns: &[],
        action: Action::Bytes(RET),
        stock: &[],
        expect_orig: &[],
    },
    Site {
        name: "AdaptiveDigitalGain_Bypass",
        group: "gain",
        what: "adaptive digital gain does not run",
        // AGC2's adaptive digital stage.
        critical: false,
        absent_ok: None,
        expect: Expect::One,
        symbols: &[SYM_ADAPTIVE_DIGITAL_GAIN],
        entry: true,
        patterns: &[],
        action: Action::Bytes(RET),
        stock: &[],
        expect_orig: &[],
    },
    Site {
        name: "NoiseSuppressorProcess_Bypass",
        group: "denoise",
        what: "noise suppressor leaves the signal alone",
        // Spectral subtraction. Removes steady noise, and with it room tone,
        // reverb tails and the quiet end of anything else.
        critical: false,
        absent_ok: None,
        expect: Expect::One,
        symbols: &[SYM_NS_PROCESS],
        entry: true,
        patterns: &[],
        action: Action::Bytes(RET),
        stock: &[],
        expect_orig: &[],
    },
    Site {
        name: "NoiseSuppressorAnalyze_Bypass",
        group: "denoise",
        what: "noise suppressor does not analyse",
        // The analysis half. Stubbed as well so the estimator is not left
        // running for a suppressor that never applies it.
        critical: false,
        absent_ok: None,
        expect: Expect::One,
        symbols: &[SYM_NS_ANALYZE],
        entry: true,
        patterns: &[],
        action: Action::Bytes(RET),
        stock: &[],
        expect_orig: &[],
    },
    Site {
        name: "EchoCanceller_Bypass",
        group: "echo",
        what: "echo canceller leaves the signal alone",
        // AEC3's capture path. It subtracts an estimate of what your speakers
        // are playing out of what your microphone picked up, and it is a
        // nonlinear, adaptive estimate — on music it is the single most
        // destructive thing in the chain.
        //
        // `ProcessCapture` is also what drains the render queue. Dropping it
        // means the queue fills, but `RenderWriter::Insert` handles a full
        // queue by discarding the frame — there is no check that aborts — so
        // the cost is bounded and the memory does not grow.
        critical: false,
        absent_ok: None,
        expect: Expect::One,
        symbols: &[SYM_AEC3_PROCESS_CAPTURE],
        entry: true,
        patterns: &[],
        action: Action::Bytes(RET),
        stock: &[],
        expect_orig: &[],
    },
    // ---- encoder locks -------------------------------------------------------
    //
    // Every `WebRtcOpus_*` wrapper is a few instructions around a single
    // `opus_encoder_ctl`, and each is called from several places (the encoder
    // constructor, the SDP reconfigure path, and the network adaptor). Pinning
    // the value at the wrapper covers all of them at once, the same way
    // `WebRtcOpus_SetBitRate` already does for the bitrate, instead of chasing
    // each caller. The request constant in the signature (`BE <req> 0F 00 00`)
    // is what tells the otherwise identical wrappers apart.
    Site {
        name: "WebRtcOpus_PacketLoss_Zero",
        group: "encoder",
        what: "packet-loss hint pinned to 0%",
        // OPUS_SET_PACKET_LOSS_PERC (4014). WebRTC feeds the measured uplink
        // loss fraction in here; libopus answers by trading audio bits for
        // error robustness — in CELT that is wider spreading and a more
        // conservative allocation, so the effect is audible even with no SILK
        // and no inband FEC. Pinning it to 0 keeps the whole budget on signal.
        critical: false,
        absent_ok: None,
        expect: Expect::One,
        symbols: &["WebRtcOpus_SetPacketLossRate"],
        entry: false,
        patterns: &[(
            "?? ?? ?? ?? ?? ?? 48 8B 07 48 85 C0 74 ?? 48 89 C7 BE AE 0F 00 00 31 C0 E8",
            0,
        )],
        action: Action::CtlArg(0),
        stock: CTL_PROLOGUE,
        expect_orig: &[CTL_PROLOGUE],
    },
    Site {
        name: "WebRtcOpus_Complexity_Max",
        group: "encoder",
        what: "encoder complexity pinned to 10",
        // OPUS_SET_COMPLEXITY (4010). Discord ships 9; 10 is the maximum and
        // costs only encoder CPU.
        critical: false,
        absent_ok: None,
        expect: Expect::One,
        symbols: &["WebRtcOpus_SetComplexity"],
        entry: false,
        patterns: &[(
            "?? ?? ?? ?? ?? ?? 48 8B 07 48 85 C0 74 ?? 48 89 C7 BE AA 0F 00 00 31 C0 E8",
            0,
        )],
        action: Action::CtlArg(10),
        stock: CTL_PROLOGUE,
        expect_orig: &[CTL_PROLOGUE],
    },
    Site {
        name: "WebRtcOpus_Bandwidth_Fullband",
        group: "encoder",
        what: "coded bandwidth pinned to fullband",
        // OPUS_SET_BANDWIDTH (4008) with OPUS_BANDWIDTH_FULLBAND (1105), the
        // full 20 kHz. Anything narrower is a low-pass by another name.
        critical: false,
        absent_ok: None,
        expect: Expect::One,
        symbols: &["WebRtcOpus_SetBandwidth"],
        entry: false,
        patterns: &[(
            "?? ?? ?? ?? ?? ?? 48 8B 07 48 85 C0 74 ?? 48 89 C7 BE A8 0F 00 00 31 C0 E8",
            0,
        )],
        action: Action::CtlArg(1105),
        stock: CTL_PROLOGUE,
        expect_orig: &[CTL_PROLOGUE],
    },
    Site {
        name: "WebRtcOpus_Fec_Off",
        group: "encoder",
        what: "inband FEC never enabled",
        // OPUS_SET_INBAND_FEC (4012). The wrapper has no `mov edx,esi` to
        // overwrite — it hardcodes 1 in each of its two arms — so the edit is
        // that immediate instead of the prologue. FEC spends payload on
        // redundancy that only pays off under loss; with CELT forced there is
        // no SILK LBRR for it to carry anyway, so this is mostly belt and
        // braces for a build where the CELT sites did not apply.
        critical: false,
        absent_ok: None,
        expect: Expect::All(2),
        symbols: &["WebRtcOpus_EnableFec"],
        entry: false,
        patterns: &[("BE AC 0F 00 00 BA ?? 00 00 00 31 C0 E8", 6)],
        action: Action::Bytes(&[0x00, 0x00, 0x00, 0x00]),
        stock: &[0x01, 0x00, 0x00, 0x00],
        expect_orig: &[&[0x01, 0x00, 0x00, 0x00], &[0x00, 0x00, 0x00, 0x00]],
    },
    Site {
        name: "WebRtcOpus_Dtx_Off",
        group: "encoder",
        what: "DTX never enabled",
        // OPUS_SET_DTX (4016), same two-armed shape as the FEC wrapper.
        // Discontinuous transmission stops sending during what it judges to be
        // silence, which on music is a gate that chews reverb tails and fades.
        critical: false,
        absent_ok: None,
        expect: Expect::All(2),
        symbols: &["WebRtcOpus_EnableDtx"],
        entry: false,
        patterns: &[("BE B0 0F 00 00 BA ?? 00 00 00 31 C0 E8", 6)],
        action: Action::Bytes(&[0x00, 0x00, 0x00, 0x00]),
        stock: &[0x01, 0x00, 0x00, 0x00],
        expect_orig: &[&[0x01, 0x00, 0x00, 0x00], &[0x00, 0x00, 0x00, 0x00]],
    },
    Site {
        name: "FrameLength_Pin",
        group: "encoder",
        what: "network adaptor cannot change the frame length mid-call",
        // The frame-length arm of the same audio network adaptor that
        // `ChannelController_ForceStereo` deals with. Left alone it walks the
        // encoder between 20 ms and 60 ms frames as the uplink estimate moves.
        //
        // Here a bare `ret` is enough, and is safer than editing the decision:
        // `MakeDecision` returns void and its only effect is writing
        // `frame_length_ms` into the runtime config. Never writing it leaves
        // the optional disengaged, `ApplyAudioNetworkAdaptor` skips
        // `SetFrameLength` entirely, and the encoder keeps the 20 ms it was
        // constructed with.
        critical: false,
        absent_ok: Some(Absent {
            covered_by: None,
            note: "no FrameLengthController in this build, so the network adaptor has \
                   no frame-length arm and the encoder keeps its configured 20 ms",
        }),
        expect: Expect::One,
        symbols: &[SYM_FRAME_LENGTH_DECISION],
        entry: true,
        patterns: &[],
        action: Action::Bytes(RET),
        stock: &[],
        expect_orig: &[],
    },
    // ---- constant bitrate ----------------------------------------------------
    Site {
        name: "WebRtcOpus_Cbr_Always",
        group: "cbr",
        what: "constant bitrate cannot be turned back off",
        // OPUS_SET_VBR (4006), inverted: `WebRtcOpus_EnableCbr` already passes
        // 0 (VBR off), so the only thing that can undo it is
        // `WebRtcOpus_DisableCbr` passing 1. Turning that immediate into 0
        // makes both wrappers agree on constant bitrate.
        //
        // Off by default, and the one group here that is not about leaving the
        // signal alone: variable bitrate at a high ceiling already spends what
        // a passage needs and no more, so constant bitrate mostly buys a
        // steadier packet size at the cost of sending full rate through quiet
        // material. Worth having when something downstream wants a predictable
        // rate; not worth having by default.
        critical: false,
        absent_ok: None,
        expect: Expect::All(2),
        symbols: &["WebRtcOpus_DisableCbr"],
        entry: false,
        patterns: &[("BE A6 0F 00 00 BA ?? 00 00 00 31 C0 E8", 6)],
        action: Action::Bytes(&[0x00, 0x00, 0x00, 0x00]),
        stock: &[0x01, 0x00, 0x00, 0x00],
        expect_orig: &[&[0x01, 0x00, 0x00, 0x00], &[0x00, 0x00, 0x00, 0x00]],
    },
    // ---- celt --------------------------------------------------------------
    Site {
        name: "CELT_Force",
        group: "celt",
        what: "user_forced_mode = MODE_CELT_ONLY",
        // `OPUS_AUTO` (-1000) would let the encoder drop into SILK or hybrid
        // at low rates, which is where the low-pass and mono folding come from.
        critical: false,
        absent_ok: None,
        expect: Expect::One,
        symbols: &["opus_encoder_init"],
        entry: false,
        patterns: &[(
            // The OpusEncoder struct shifted by 4 bytes between 1.0.153 and
            // 1.0.155, so the field displacements are wildcarded; the
            // `imul rcx, rax, 0x51EB851F` that follows is the real anchor.
            "48 C7 83 ?? 00 00 00 ?? ?? ?? ?? 48 63 83 ?? 00 00 00 48 69 C8 1F 85 EB 51",
            7,
        )],
        action: Action::Bytes(CELT_ONLY),
        stock: &[0x18, 0xFC, 0xFF, 0xFF],
        expect_orig: &[&[0x18, 0xFC, 0xFF, 0xFF], CELT_ONLY],
    },
    Site {
        name: "CELT_DefaultMode",
        group: "celt",
        what: "initial st->mode = MODE_CELT_ONLY",
        critical: false,
        absent_ok: None,
        expect: Expect::NearestAfter { anchor: "CELT_Force", window: 0x400 },
        symbols: &["opus_encoder_init"],
        entry: false,
        patterns: &[(
            "C7 83 ?? 37 00 00 01 00 00 00 C7 83 ?? 37 00 00 ?? ?? 00 00 C7 83 ?? 37 00 00 51 04 00 00",
            16,
        )],
        action: Action::Bytes(CELT_ONLY),
        stock: &[0xE9, 0x03, 0x00, 0x00],
        expect_orig: &[&[0xE9, 0x03, 0x00, 0x00], CELT_ONLY],
    },
    // ---- filters -----------------------------------------------------------
    Site {
        name: "SplHighPass_Entry",
        group: "filter",
        what: "WebRTC high-pass filter returns immediately",
        critical: false,
        absent_ok: None,
        expect: Expect::One,
        symbols: &[SYM_HIGHPASS_PROCESS],
        entry: true,
        patterns: &[(
            "55 48 89 E5 41 57 41 56 53 50 48 89 F3 49 89 FE 48 8B 46 38 85 D2 74 ?? 48 85 C0",
            0,
        )],
        action: Action::Bytes(RET),
        stock: &[],
        expect_orig: &[&[0x55], RET],
    },
    Site {
        name: "HpCutoff_Inject",
        group: "filter",
        what: "opus hp_cutoff replaced with a pass-through",
        critical: false,
        absent_ok: Some(Absent {
            covered_by: Some("OpusConfig_Application"),
            note: "libopus calls hp_cutoff only in VOIP mode; the application patch \
                   selects kAudio, so it is never reached",
        }),
        expect: Expect::One,
        symbols: &["hp_cutoff"],
        entry: true,
        patterns: &[("55 48 89 E5 49 89 D2 48 63 55 10 0F BF C6 69 C0 A7 09 00 00", 0)],
        action: Action::ShellcodeHpCutoff,
        stock: &[],
        expect_orig: &[&[0x55, 0x48, 0x89, 0xE5]],
    },
    Site {
        name: "DcReject_Coefficient",
        group: "filter",
        what: "opus dc_reject coefficient forced to 0 (pass-through)",
        // For builds that inline dc_reject, where there is no function left to
        // replace. libopus computes `coef = 6.3 * cutoff_Hz / Fs` and then
        // `out = x - m`, `m = coef*x + coef2*m` with `coef2 = 1 - coef`.
        // Zeroing the numerator gives coef = 0 and coef2 = 1, so `m` never
        // leaves its initial zero and the filter passes the signal through.
        // One four-byte constant instead of a whole injected function.
        critical: false,
        absent_ok: Some(Absent {
            covered_by: Some("DcReject_Inject"),
            note: "dc_reject is a real function in this build and is replaced \
                   outright, so its coefficient does not need neutralising",
        }),
        expect: Expect::One,
        symbols: &["opus_encode_frame_native"],
        entry: false,
        // Anchored on the coefficient computation itself: cvtsi2ss of Fs,
        // divide the constant by it, subtract from 1.
        patterns: &[(
            "F3 0F 2A ?? F3 0F 10 0D ?? ?? ?? ?? F3 0F 5E C8 F3 0F 10 15 ?? ?? ?? ?? F3 0F 5C D1",
            8,
        )],
        action: Action::RipRelF32(0.0),
        stock: &[],
        // The constant is 6.3*3 = 18.9 stock, or 0.0 once patched.
        expect_orig: &[&[0x34, 0x33, 0x97, 0x41], &[0x00, 0x00, 0x00, 0x00]],
    },
    Site {
        name: "DcReject_Inject",
        group: "filter",
        what: "opus dc_reject replaced with a pass-through",
        critical: false,
        absent_ok: Some(Absent {
            covered_by: Some("DcReject_Coefficient"),
            note: "dc_reject is inlined in this build; its coefficient is zeroed \
                   instead, which makes it pass through",
        }),
        expect: Expect::One,
        symbols: &["dc_reject"],
        entry: true,
        patterns: &[(
            "55 48 89 E5 F3 41 0F 2A C1 F3 0F 10 0D ?? ?? ?? ?? F3 0F 5E C8 F3 0F 10 15",
            0,
        )],
        action: Action::ShellcodeDcReject,
        stock: &[],
        expect_orig: &[&[0x55, 0x48, 0x89, 0xE5]],
    },
];

/// Shared by four sites: the `AudioEncoderOpusConfig` constructor stores
/// frame_ms, channels, bitrate and application as immediates in one run.
const OPUS_CONFIG_CTOR: &str = "55 48 89 E5 48 B8 ?? 00 00 00 80 BB 00 00 48 89 07 48 C7 47 08 ?? 00 00 00 \
     48 B8 00 00 00 00 ?? ?? ?? 00 48 89 47 10 C6 47 18 ??";

/// 1.0.155 inlines the constructor into its caller's stack frame, so the same
/// stores appear as `mov [rbp-disp32], ...` rather than `mov [rdi+disp8], ...`.
/// Field order and values are unchanged.
const OPUS_CONFIG_CTOR_INLINED: &str = "48 B8 ?? 00 00 00 80 BB 00 00 48 89 85 ?? ?? ?? ?? \
     48 C7 85 ?? ?? ?? ?? ?? 00 00 00 48 B8 00 00 00 00 ?? ?? ?? 00 48 89 85 ?? ?? ?? ?? \
     C6 85 ?? ?? ?? ?? ??";

/// Same idea for the multichannel variant.
const MULTICHANNEL_CTOR: &str = "55 48 89 E5 C7 07 14 00 00 00 48 C7 47 08 ?? 00 00 00 \
     48 B8 00 00 00 00 ?? ?? ?? 00 48 89 47 10 66 C7 47 18 00 00";

pub fn compile_patterns(site: &Site) -> Vec<Pattern> {
    site.patterns
        .iter()
        .map(|(src, delta)| Pattern::parse(src, *delta))
        .collect()
}

pub fn find(name: &str) -> Option<&'static Site> {
    SITES.iter().find(|s| s.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_site_belongs_to_a_declared_group() {
        for site in SITES {
            assert!(
                group(site.group).is_some(),
                "site {} is in group {:?}, which is not in GROUPS",
                site.name,
                site.group
            );
        }
    }

    #[test]
    fn every_group_has_at_least_one_site() {
        for g in GROUPS {
            assert!(
                SITES.iter().any(|s| s.group == g.name),
                "group {:?} has no sites",
                g.name
            );
        }
    }

    #[test]
    fn site_names_are_unique() {
        for (i, a) in SITES.iter().enumerate() {
            assert!(
                !SITES[i + 1..].iter().any(|b| b.name == a.name),
                "duplicate site name {:?}",
                a.name
            );
        }
    }

    #[test]
    fn patterns_compile_and_deltas_are_in_range() {
        // Pattern::parse asserts on both; this just makes the whole catalogue
        // get parsed once under `cargo test` rather than on a user's machine.
        for site in SITES {
            for p in compile_patterns(site) {
                assert!(p.delta < p.toks.len(), "{}", site.name);
            }
        }
    }

    #[test]
    fn anchors_refer_to_real_sites() {
        for site in SITES {
            if let Expect::NearestAfter { anchor, .. } = site.expect {
                assert!(find(anchor).is_some(), "{} anchors on missing {}", site.name, anchor);
            }
            if let Some(a) = &site.absent_ok {
                if let Some(other) = a.covered_by {
                    assert!(find(other).is_some(), "{} covered_by missing {}", site.name, other);
                }
            }
        }
    }
}
