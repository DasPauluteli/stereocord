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
    /// Overwrite with the injected filter replacement.
    ShellcodeHpCutoff,
    /// Overwrite with the injected filter replacement.
    ShellcodeDcReject,
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

/// `mov r12, 2` — same length as the `cmp`/`cmovae` pair it replaces.
const FORCE_TWO_CHANNELS: &[u8] = &[0x49, 0xC7, 0xC4, 0x02, 0x00, 0x00, 0x00];
/// Twelve `nop`s, then the `E9` that turns the following `jg rel32` into an
/// unconditional `jmp rel32` over the mono downmix.
const NOP12_JMP: &[u8] = &[
    0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0xE9,
];
/// `mov rax, 1; ret`
const RETURN_TRUE: &[u8] = &[0x48, 0xC7, 0xC0, 0x01, 0x00, 0x00, 0x00, 0xC3];
/// 48000, little endian.
const SR_48K: &[u8] = &[0x80, 0xBB, 0x00, 0x00];
/// `MODE_CELT_ONLY`, little endian.
const CELT_ONLY: &[u8] = &[0xEA, 0x03, 0x00, 0x00];
const RET: &[u8] = &[0xC3];

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
        expect: Expect::One,
        symbols: &[SYM_CAPTURED_AUDIO_PROCESS],
        entry: false,
        patterns: &[(
            "4C 89 F7 E8 ?? ?? ?? ?? 84 C0 74 0D 83 BB ?? ?? ?? ?? 09 0F 8F",
            8,
        )],
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
        expect: Expect::One,
        symbols: &[SYM_MULTICHANNEL_CTOR],
        entry: false,
        patterns: &[(MULTICHANNEL_CTOR, 0x0E)],
        action: Action::Bytes(&[0x02]),
        stock: &[0x01],
        expect_orig: &[&[0x01], &[0x02]],
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
    // ---- opus framing ------------------------------------------------------
    Site {
        name: "OpusConfig_FrameMs",
        group: "opus",
        what: "10 ms frames",
        critical: false,
        expect: Expect::One,
        symbols: &[SYM_OPUS_CONFIG_CTOR],
        entry: false,
        patterns: &[(OPUS_CONFIG_CTOR, 6), (OPUS_CONFIG_CTOR_INLINED, 2)],
        action: Action::Bytes(&[0x0A]),
        stock: &[0x14],
        expect_orig: &[&[0x14], &[0x0A]],
    },
    Site {
        name: "OpusConfig_Application",
        group: "opus",
        what: "OPUS_APPLICATION_AUDIO instead of VOIP",
        critical: false,
        expect: Expect::One,
        symbols: &[SYM_OPUS_CONFIG_CTOR],
        entry: false,
        patterns: &[(OPUS_CONFIG_CTOR, 0x2A), (OPUS_CONFIG_CTOR_INLINED, 51)],
        action: Action::Bytes(&[0x01]),
        stock: &[],
        expect_orig: &[&[0x00], &[0x01]],
    },
    Site {
        name: "OpusConfig_IsOk",
        group: "opus",
        what: "config validation always accepts",
        // Otherwise the 248 kbps / 2 channel / 10 ms combination is rejected
        // before it reaches the encoder.
        critical: true,
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
    // ---- celt --------------------------------------------------------------
    Site {
        name: "CELT_Force",
        group: "celt",
        what: "user_forced_mode = MODE_CELT_ONLY",
        // `OPUS_AUTO` (-1000) would let the encoder drop into SILK or hybrid
        // at low rates, which is where the low-pass and mono folding come from.
        critical: false,
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
        expect: Expect::One,
        symbols: &["hp_cutoff"],
        entry: true,
        patterns: &[("55 48 89 E5 49 89 D2 48 63 55 10 0F BF C6 69 C0 A7 09 00 00", 0)],
        action: Action::ShellcodeHpCutoff,
        stock: &[],
        expect_orig: &[&[0x55, 0x48, 0x89, 0xE5]],
    },
    Site {
        name: "DcReject_Inject",
        group: "filter",
        what: "opus dc_reject replaced with a pass-through",
        critical: false,
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
