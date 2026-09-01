//! Minimal MD5, used only to label a build so two people can tell whether they
//! are looking at the same `discord_voice.node`. Nothing security-relevant
//! depends on it; the patcher validates by instruction context, not by hash.

const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

fn k_table() -> [u32; 64] {
    let mut k = [0u32; 64];
    for (i, slot) in k.iter_mut().enumerate() {
        *slot = ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32;
    }
    k
}

pub fn hex(data: &[u8]) -> String {
    let k = k_table();
    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;

    let bitlen = (data.len() as u64).wrapping_mul(8);
    let mut msg = Vec::with_capacity(64);

    let full_blocks = data.len() / 64;
    let process = |chunk: &[u8], a0: &mut u32, b0: &mut u32, c0: &mut u32, d0: &mut u32| {
        let mut m = [0u32; 16];
        for (i, slot) in m.iter_mut().enumerate() {
            *slot = u32::from_le_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (*a0, *b0, *c0, *d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            let sum = a
                .wrapping_add(f)
                .wrapping_add(k[i])
                .wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(S[i]));
            a = tmp;
        }
        *a0 = a0.wrapping_add(a);
        *b0 = b0.wrapping_add(b);
        *c0 = c0.wrapping_add(c);
        *d0 = d0.wrapping_add(d);
    };

    for i in 0..full_blocks {
        process(&data[i * 64..i * 64 + 64], &mut a0, &mut b0, &mut c0, &mut d0);
    }

    msg.extend_from_slice(&data[full_blocks * 64..]);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_le_bytes());
    for chunk in msg.chunks(64) {
        process(chunk, &mut a0, &mut b0, &mut c0, &mut d0);
    }

    let mut out = String::with_capacity(32);
    for word in [a0, b0, c0, d0] {
        for byte in word.to_le_bytes() {
            out.push_str(&format!("{byte:02x}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn known_vectors() {
        assert_eq!(super::hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(super::hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            super::hex(b"The quick brown fox jumps over the lazy dog"),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
        // Exactly one block, and one block plus the length-only tail: the two
        // padding edge cases.
        assert_eq!(super::hex(&[b'a'; 64]), "014842d480b571495a4a0363793f7367");
        assert_eq!(super::hex(&[b'a'; 56]), "3b0c8ac703f828b04c6c197006d17218");
    }
}
