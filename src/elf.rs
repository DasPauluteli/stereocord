//! Just enough ELF64 to read a symbol table.
//!
//! Every `discord_voice.node` seen so far ships a full `.symtab` — around 64k
//! function symbols, covering the bundled Opus and WebRTC code by name and
//! Discord's own C++ under its mangled names. That is a far better anchor than
//! a byte pattern: a signature can drift or match twice, whereas
//! `opus_encoder_init` is `opus_encoder_init`.
//!
//! Symbols are treated as an optimisation, never a requirement. A stripped
//! build falls back to scanning, which is why the signature catalogue is still
//! carried for every site.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy)]
pub struct Sym {
    /// Offset of the function within the file.
    pub offset: usize,
    pub size: usize,
}

pub struct Symbols {
    by_name: HashMap<String, Sym>,
}

impl Symbols {
    pub fn get(&self, name: &str) -> Option<Sym> {
        self.by_name.get(name).copied()
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }
}

fn u16le(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn u32le(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

fn u64le(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(at..at + 8)?.try_into().ok()?))
}

/// Read the function symbols of a little-endian 64-bit ELF.
///
/// Returns `None` for anything that is not one, or that carries no `.symtab`;
/// callers treat that as "fall back to scanning" rather than as an error.
pub fn symbols(data: &[u8]) -> Option<Symbols> {
    if data.get(..4)? != b"\x7fELF" || data.get(4)? != &2 || data.get(5)? != &1 {
        return None; // not ELF64 little-endian
    }

    let e_shoff = u64le(data, 0x28)? as usize;
    let e_shentsize = u16le(data, 0x3A)? as usize;
    let e_shnum = u16le(data, 0x3C)? as usize;
    if e_shoff == 0 || e_shentsize < 64 || e_shnum == 0 {
        return None;
    }

    // Virtual addresses and file offsets coincide in the builds seen so far,
    // but read the program headers rather than assume it: a symbol's value is
    // a vaddr and every patch needs a file offset.
    let translate = load_segments(data);

    let section = |i: usize| -> Option<(u32, usize, usize, u32, usize)> {
        let base = e_shoff + i * e_shentsize;
        Some((
            u32le(data, base + 4)?,               // sh_type
            u64le(data, base + 0x18)? as usize,   // sh_offset
            u64le(data, base + 0x20)? as usize,   // sh_size
            u32le(data, base + 0x28)?,            // sh_link
            u64le(data, base + 0x38)? as usize,   // sh_entsize
        ))
    };

    const SHT_SYMTAB: u32 = 2;
    const STT_FUNC: u8 = 2;

    for i in 0..e_shnum {
        let (sh_type, sh_offset, sh_size, sh_link, sh_entsize) = section(i)?;
        if sh_type != SHT_SYMTAB || sh_entsize < 24 {
            continue;
        }
        let (_, str_off, str_size, _, _) = section(sh_link as usize)?;
        let strtab = data.get(str_off..str_off + str_size)?;

        let mut by_name = HashMap::new();
        let count = sh_size / sh_entsize;
        for n in 0..count {
            let e = sh_offset + n * sh_entsize;
            let st_name = u32le(data, e)? as usize;
            let st_info = *data.get(e + 4)?;
            if st_info & 0xF != STT_FUNC {
                continue;
            }
            let st_value = u64le(data, e + 8)?;
            let st_size = u64le(data, e + 16)? as usize;
            if st_value == 0 {
                continue;
            }
            let Some(offset) = translate(st_value) else { continue };
            let Some(name) = cstr(strtab, st_name) else { continue };
            if name.is_empty() {
                continue;
            }
            // Keep the first definition; duplicates are ICF-folded aliases
            // that share the same code anyway.
            by_name.entry(name).or_insert(Sym { offset, size: st_size });
        }
        if by_name.is_empty() {
            return None;
        }
        return Some(Symbols { by_name });
    }
    None
}

fn cstr(strtab: &[u8], at: usize) -> Option<String> {
    let rest = strtab.get(at..)?;
    let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    std::str::from_utf8(&rest[..end]).ok().map(|s| s.to_string())
}

/// Build a vaddr -> file-offset mapping from the PT_LOAD program headers.
fn load_segments(data: &[u8]) -> impl Fn(u64) -> Option<usize> + '_ {
    let mut segs: Vec<(u64, u64, u64)> = Vec::new(); // vaddr, filesz, offset
    (|| -> Option<()> {
        let e_phoff = u64le(data, 0x20)? as usize;
        let e_phentsize = u16le(data, 0x36)? as usize;
        let e_phnum = u16le(data, 0x38)? as usize;
        const PT_LOAD: u32 = 1;
        for i in 0..e_phnum {
            let base = e_phoff + i * e_phentsize;
            if u32le(data, base)? != PT_LOAD {
                continue;
            }
            let p_offset = u64le(data, base + 0x08)?;
            let p_vaddr = u64le(data, base + 0x10)?;
            let p_filesz = u64le(data, base + 0x20)?;
            segs.push((p_vaddr, p_filesz, p_offset));
        }
        Some(())
    })();

    move |vaddr: u64| {
        for &(v, filesz, off) in &segs {
            if vaddr >= v && vaddr < v + filesz {
                return Some((vaddr - v + off) as usize);
            }
        }
        None
    }
}
