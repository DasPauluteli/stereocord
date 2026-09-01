# How it works

## The problem with the original approach

The upstream project shipped a table of file offsets per Discord build:

```
OFFSET_CommitAudioCodec_StereoCheck1_Imm0=0x39C300
OFFSET_CreateAudioFrame_Channels_MovImm2=0x390070
...
```

Those offsets are correct for exactly one binary. When Discord shipped a new
build they went stale, and the fallback was to download a known-good
`discord_voice.node` from GitHub and install it over the user's module — which
replaces the voice engine with one from a different Discord build, leaving the
patches applying cleanly to a binary the rest of the client was never paired
with. It also breaks Discord's updater, which ships module updates as binary
deltas and verifies the SHA-256 of the file it is about to patch.

## Resolution

Each patch site is described by what it *is*, and located at run time.

**Symbols first.** Every `discord_voice.node` examined ships a full `.symtab` —
around 64k function symbols, covering the bundled Opus and WebRTC code by name
and Discord's own C++ under its mangled names. Five sites are just a function
entry (`downmix_and_resample`, `hp_cutoff`, `dc_reject`,
`webrtc::HighPassFilter::Process`, `AudioEncoderOpusConfig::IsOk`), so a symbol
lookup is the whole job.

**Signatures scoped to a function.** The rest need a specific instruction inside
a function — an immediate operand, usually. Those search a byte pattern, but
only within the extent of a named symbol, so a pattern that would match twice
across a 100 MB binary cannot. `CELT_Force` searches inside `opus_encoder_init`;
`WebRtcOpus_SetBitRate`'s value argument searches inside the function of the
same name.

**Whole-file scanning as fallback.** If a binary is stripped, or a site's
function was inlined away, the same patterns run across the whole file with
explicit disambiguation rules (expected match count, or proximity to an
already-resolved anchor).

`scan -v` reports which route each site took.

## Validation

A signature can match somewhere unintended. Every site therefore declares the
bytes it expects to find, including the already-patched form, and the run is
abandoned before anything is written if what is actually there does not match.
After writing, the file is read back and every edit compared against what was
supposed to land.

Sites are also marked critical or not. The critical ones decide whether audio is
mono or stereo; the rest are quality refinements. When only non-critical sites
are missing the tool proceeds and says so. When a critical one is missing it
refuses, because a client that negotiates stereo and still sends one channel is
worse than one that does neither.

## The injected filters

Two sites replace a function body rather than an operand: libopus' `hp_cutoff`
and `dc_reject`, the encoder's input high-pass and DC-rejection filters.
Replacing them with a straight copy is what makes the result "filterless".

Each replacement also writes four `OpusEncoder` fields before copying — pinning
CELT mode and clearing the carried-over prediction state on every call rather
than only at encoder init. `hp_mem` sits at a fixed offset inside
`OpusEncoder`, so the encoder base is a constant displacement from the pointer
the caller already passes in.

Upstream compiled a C++ file at install time with whatever `g++`/`clang++` the
machine had and copied the resulting function bodies into the binary, making the
injected bytes depend on the host toolchain. Here they are emitted byte by byte
(`src/shellcode.rs`), so the same input always produces the same patch and no
compiler is required:

```
mov  dword [rcx+0x10],  1002      ; st->mode = MODE_CELT_ONLY
mov  dword [rcx-0x36e4], -1       ; st->prev_mode
mov  dword [rcx-0x36e0], -1       ; st->prev_channels
mov  dword [rcx-0x36cc], 0        ; st->prev_framesize
mov  r11d, r9d
imul r11d, r8d                    ; channels * len
test r11d, r11d
jle  done
movss xmm1, [rip+gain]
xor  eax, eax
loop:
movss xmm0, [rdi+rax*4]
mulss xmm0, xmm1
movss [rdx+rax*4], xmm0
add  eax, 1
cmp  eax, r11d
jl   loop
done:
ret
```

Both replacements are around 92 bytes against function bodies of 400+, so the
tail of the original is simply left as unreachable code.

## Interaction with Discord's updater

Discord ships voice-module updates as binary deltas and verifies the SHA-256 of
the file it is about to patch. A patched module fails that check, which aborts
the entire host update — Discord goes on launching the old version and the
staged `app-` directory sits half-populated, which looks like a stalled
download. `scan` detects this and explains it. The fix is `restore`, let Discord
update, then `patch` again.

This also means only `discord_voice.node` is ever written. Replacing other files
in the module bundle, as the upstream script did, leaves the updater failing on
whichever file it checks next.
