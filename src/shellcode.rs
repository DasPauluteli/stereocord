//! Hand-assembled replacements for libopus' `hp_cutoff` and `dc_reject`.
//!
//! Both functions are the encoder's input high-pass / DC-rejection filters.
//! Replacing them with a straight copy (optionally scaled) is what makes the
//! result "filterless": the signal reaches the MDCT untouched below ~80 Hz.
//!
//! Each replacement also writes four fields of the enclosing `OpusEncoder`
//! before copying, which is how the upstream project pinned CELT mode and
//! cleared the two prediction-state fields on every frame rather than only at
//! encoder init. `hp_mem` sits at a fixed offset inside `OpusEncoder`, so the
//! encoder base is reachable by a constant displacement from the pointer the
//! caller already passes in.
//!
//! Upstream compiled a C++ file at install time with whatever g++/clang++ the
//! machine had and copied the resulting function bodies into the binary. That
//! makes the injected bytes depend on the host toolchain, which is a poor
//! property for something written into a 100 MB shared object. These are
//! emitted byte by byte instead, so the same input always produces the same
//! patch and no compiler is required.

use crate::sig::hex;

/// `hp_mem` lies this many bytes above the start of `OpusEncoder`.
const HP_MEM_FROM_ENCODER: i32 = 3553 * 4; // 14212

/// `OpusEncoder` field offsets, expressed relative to `hp_mem`.
const OFF_MODE: i32 = 3557 * 4 - HP_MEM_FROM_ENCODER; //  +16  st->mode        = MODE_CELT_ONLY
const OFF_PREV_MODE: i32 = 160 - HP_MEM_FROM_ENCODER; // -14052  st->prev_mode  = -1
const OFF_PREV_CHANNELS: i32 = 164 - HP_MEM_FROM_ENCODER; // -14048  st->prev_channels = -1
const OFF_PREV_FRAMESIZE: i32 = 184 - HP_MEM_FROM_ENCODER; // -14028  st->prev_framesize = 0

const MODE_CELT_ONLY: i32 = 1002;

/// x86-64 register numbers, in the encoding order used by ModRM/SIB.
mod reg {
    pub const RCX: u8 = 1;
    pub const RDX: u8 = 2;
    pub const RSI: u8 = 6;
    pub const RDI: u8 = 7;
}

struct Emit {
    code: Vec<u8>,
}

impl Emit {
    fn new() -> Emit {
        Emit { code: Vec::new() }
    }

    fn at(&self) -> usize {
        self.code.len()
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }

    /// `mov dword ptr [base + disp32], imm32`
    fn mov_mem_imm32(&mut self, base: u8, disp: i32, imm: i32) {
        debug_assert!(base != 4 && base != 5, "base needs a SIB byte");
        self.raw(&[0xC7, 0x80 | base]); // C7 /0, mod=10 (disp32)
        self.raw(&disp.to_le_bytes());
        self.raw(&imm.to_le_bytes());
    }

    /// `movss xmm_dst, [rip + disp32]` — displacement filled in later.
    fn movss_xmm_rip(&mut self, xmm: u8) -> usize {
        self.raw(&[0xF3, 0x0F, 0x10, 0x05 | (xmm << 3)]);
        let slot = self.at();
        self.raw(&[0, 0, 0, 0]);
        slot
    }

    /// `movss xmm0, [base + rax*4]`
    fn movss_load_idx(&mut self, base: u8) {
        self.raw(&[0xF3, 0x0F, 0x10, 0x04, 0x80 | base]);
    }

    /// `movss [base + rax*4], xmm0`
    fn movss_store_idx(&mut self, base: u8) {
        self.raw(&[0xF3, 0x0F, 0x11, 0x04, 0x80 | base]);
    }
}

/// Build one filter replacement.
///
/// `src`/`dst` are the registers holding the input and output float pointers,
/// `enc` holds `hp_mem`, and the sample count is `count_a * count_b` where both
/// are 32-bit registers (`len` and `channels`, in either order).
fn build(src: u8, dst: u8, enc: u8, count_a: CountReg, count_b: CountReg, gain: f32) -> Vec<u8> {
    let mut e = Emit::new();

    // Pin the encoder into CELT mode and clear the carried-over prediction
    // state, every call, so nothing downstream can walk it back.
    e.mov_mem_imm32(enc, OFF_MODE, MODE_CELT_ONLY);
    e.mov_mem_imm32(enc, OFF_PREV_MODE, -1);
    e.mov_mem_imm32(enc, OFF_PREV_CHANNELS, -1);
    e.mov_mem_imm32(enc, OFF_PREV_FRAMESIZE, 0);

    // r11d = count_a * count_b
    e.raw(&count_a.mov_to_r11d());
    e.raw(&count_b.imul_into_r11d());
    e.raw(&[0x45, 0x85, 0xDB]); // test r11d, r11d

    e.raw(&[0x7E, 0x00]); // jle done  (patched below)
    let jle_rel = e.at() - 1;

    let gain_ref = e.movss_xmm_rip(1); // movss xmm1, [rip+gain]
    e.raw(&[0x31, 0xC0]); // xor eax, eax

    let loop_top = e.at();
    e.movss_load_idx(src); // movss xmm0, [src + rax*4]
    e.raw(&[0xF3, 0x0F, 0x59, 0xC1]); // mulss xmm0, xmm1
    e.movss_store_idx(dst); // movss [dst + rax*4], xmm0
    e.raw(&[0x83, 0xC0, 0x01]); // add eax, 1
    e.raw(&[0x44, 0x39, 0xD8]); // cmp eax, r11d
    e.raw(&[0x7C, 0x00]); // jl loop
    let jl_rel = e.at() - 1;
    e.code[jl_rel] = ((loop_top as isize - e.at() as isize) as i8) as u8;

    let done = e.at();
    e.code[jle_rel] = ((done as isize - (jle_rel as isize + 1)) as i8) as u8;
    e.raw(&[0xC3]); // ret

    // The gain constant lives immediately after the code, 4-byte aligned.
    while e.at() % 4 != 0 {
        e.raw(&[0x90]);
    }
    let gain_at = e.at();
    e.raw(&gain.to_le_bytes());

    let next_insn = gain_ref + 4;
    let disp = (gain_at as i64 - next_insn as i64) as i32;
    e.code[gain_ref..gain_ref + 4].copy_from_slice(&disp.to_le_bytes());

    e.code
}

#[derive(Clone, Copy)]
enum CountReg {
    /// One of the legacy registers (eax..edi).
    Low(u8),
    /// One of r8d..r15d.
    High(u8),
}

impl CountReg {
    /// `mov r11d, <self>`
    fn mov_to_r11d(self) -> Vec<u8> {
        match self {
            CountReg::Low(r) => vec![0x41, 0x89, 0xC3 | (r << 3)],
            CountReg::High(r) => vec![0x45, 0x89, 0xC3 | (r << 3)],
        }
    }

    /// `imul r11d, <self>`
    fn imul_into_r11d(self) -> Vec<u8> {
        match self {
            CountReg::Low(r) => vec![0x44, 0x0F, 0xAF, 0xD8 | r],
            CountReg::High(r) => vec![0x45, 0x0F, 0xAF, 0xD8 | r],
        }
    }
}

/// `void hp_cutoff(const float *in, int cutoff_Hz, float *out, int *hp_mem,
///                 int len, int channels, int Fs, int arch)`
///
/// SysV: rdi=in, esi=cutoff_Hz, rdx=out, rcx=hp_mem, r8d=len, r9d=channels.
pub fn hp_cutoff(gain: f32) -> Vec<u8> {
    build(
        reg::RDI,
        reg::RDX,
        reg::RCX,
        CountReg::High(1), // r9d = channels
        CountReg::High(0), // r8d = len
        gain,
    )
}

/// `void dc_reject(const float *in, float *out, int *hp_mem,
///                 int len, int channels, int Fs)`
///
/// SysV: rdi=in, rsi=out, rdx=hp_mem, ecx=len, r8d=channels.
pub fn dc_reject(gain: f32) -> Vec<u8> {
    build(
        reg::RDI,
        reg::RSI,
        reg::RDX,
        CountReg::High(0), // r8d = channels
        CountReg::Low(reg::RCX),
        gain,
    )
}

/// The leading bytes of an injected filter are unique enough to recognise the
/// tool's own work in a binary whose original bytes are long gone.
fn marker(enc_base: u8) -> Vec<u8> {
    let mut e = Emit::new();
    e.mov_mem_imm32(enc_base, OFF_MODE, MODE_CELT_ONLY);
    e.mov_mem_imm32(enc_base, OFF_PREV_MODE, -1);
    e.code
}

pub fn hp_cutoff_marker() -> Vec<u8> {
    marker(reg::RCX)
}

pub fn dc_reject_marker() -> Vec<u8> {
    marker(reg::RDX)
}

/// Human-readable dump, used by `stereocord shellcode`.
pub fn describe(gain: f32) -> String {
    let hp = hp_cutoff(gain);
    let dc = dc_reject(gain);
    format!(
        "hp_cutoff  ({} bytes)\n  {}\n\ndc_reject  ({} bytes)\n  {}\n",
        hp.len(),
        hex(&hp),
        dc.len(),
        hex(&dc)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_offsets_match_the_upstream_expression() {
        // Upstream wrote these as `st = hp_mem - 3553` (an int*) followed by
        // int-indexed and byte-indexed stores; these are the same addresses
        // expressed relative to hp_mem.
        assert_eq!(OFF_MODE, 16);
        assert_eq!(OFF_PREV_MODE, -14052);
        assert_eq!(OFF_PREV_CHANNELS, -14048);
        assert_eq!(OFF_PREV_FRAMESIZE, -14028);
    }

    #[test]
    fn emitted_code_is_deterministic() {
        assert_eq!(hp_cutoff(1.0), hp_cutoff(1.0));
        assert_eq!(dc_reject(2.5), dc_reject(2.5));
        assert_ne!(hp_cutoff(1.0), hp_cutoff(2.0));
    }

    #[test]
    fn both_filters_fit_the_functions_they_replace() {
        // The two target functions sit 0x1B0 apart in every build seen so far;
        // upstream itself wrote 0x180 bytes into them.
        assert!(hp_cutoff(1.0).len() <= 0x180);
        assert!(dc_reject(1.0).len() <= 0x180);
    }

    #[test]
    fn code_ends_in_ret_before_the_gain_constant() {
        let code = hp_cutoff(1.0);
        let gain = &code[code.len() - 4..];
        assert_eq!(f32::from_le_bytes(gain.try_into().unwrap()), 1.0);
        // ret, then nop padding up to the 4-byte aligned constant.
        let tail = &code[..code.len() - 4];
        let ret = tail.iter().rposition(|&b| b == 0xC3).unwrap();
        assert!(tail[ret + 1..].iter().all(|&b| b == 0x90));
    }

    #[test]
    fn markers_are_a_prefix_of_the_code_they_identify() {
        let hp = hp_cutoff(1.0);
        let m = hp_cutoff_marker();
        assert_eq!(&hp[..m.len()], &m[..]);
        let dc = dc_reject(1.0);
        let m = dc_reject_marker();
        assert_eq!(&dc[..m.len()], &m[..]);
    }

    #[test]
    fn the_two_filters_are_told_apart_by_their_markers() {
        assert_ne!(hp_cutoff_marker(), dc_reject_marker());
    }
}
