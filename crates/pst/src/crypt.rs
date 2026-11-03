//! NDB block encodings and CRC. [MS-PST] 5.1–5.3

#[path = "crypt_tables.rs"]
mod tables;

use tables::{MPBB_I, MPBB_R, MPBB_S};

/// `bCryptMethod` values from the file header.
pub const NDB_CRYPT_NONE: u8 = 0x00;
pub const NDB_CRYPT_PERMUTE: u8 = 0x01;
pub const NDB_CRYPT_CYCLIC: u8 = 0x02;

/// Decode a block encoded with NDB_CRYPT_PERMUTE.
pub fn permute_decode(data: &mut [u8]) {
    for b in data {
        *b = MPBB_I[*b as usize];
    }
}

/// Encode with NDB_CRYPT_PERMUTE (used by the test writer).
pub fn permute_encode(data: &mut [u8]) {
    for b in data {
        *b = MPBB_R[*b as usize];
    }
}

/// NDB_CRYPT_CYCLIC is its own inverse. `key` is the low 32 bits of the block id.
pub fn cyclic(data: &mut [u8], key: u32) {
    let mut w = (key ^ (key >> 16)) as u16;
    for b in data {
        let lo = w as u8;
        let hi = (w >> 8) as u8;
        let mut v = b.wrapping_add(lo);
        v = MPBB_R[v as usize];
        v = v.wrapping_add(hi);
        v = MPBB_S[v as usize];
        v = v.wrapping_sub(hi);
        v = MPBB_I[v as usize];
        *b = v.wrapping_sub(lo);
        w = w.wrapping_add(1);
    }
}

/// [MS-PST] 5.3 CRC: CRC-32 (reflected 0xEDB88320) with initial value 0 and no final XOR.
///
/// crc32fast seeds its state with `!initial` and returns `!state`, so seeding with
/// `0xFFFF_FFFF` and inverting the result yields the raw, un-inverted register.
pub fn crc(data: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new_with_initial(0xFFFF_FFFF);
    h.update(data);
    !h.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crc_reference(data: &[u8]) -> u32 {
        let mut crc = 0u32;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            }
        }
        crc
    }

    #[test]
    fn crc_matches_bitwise_definition() {
        for data in [&b""[..], b"a", b"123456789", &[0u8; 1000], &(0..=255u8).collect::<Vec<_>>()] {
            assert_eq!(crc(data), crc_reference(data));
        }
        assert_eq!(crc(b""), 0);
    }

    #[test]
    fn tables_are_permutations() {
        for t in [&MPBB_R, &MPBB_S, &MPBB_I] {
            let mut seen = [false; 256];
            for &v in t.iter() {
                seen[v as usize] = true;
            }
            assert!(seen.iter().all(|&s| s));
        }
        for x in 0..=255u8 {
            assert_eq!(MPBB_I[MPBB_R[x as usize] as usize], x);
        }
    }

    #[test]
    fn permute_roundtrip_and_known_value() {
        let mut d = b"Hello, PST".to_vec();
        permute_encode(&mut d);
        assert_ne!(&d, b"Hello, PST");
        // [MS-PST] 5.1: mpbbR[0] = 0x41 ('A'), so a zero byte encodes as 0x41.
        let mut z = [0u8];
        permute_encode(&mut z);
        assert_eq!(z, [0x41]);
        permute_decode(&mut d);
        assert_eq!(&d, b"Hello, PST");
    }

    #[test]
    fn cyclic_is_involution_and_key_dependent() {
        let orig: Vec<u8> = (0..600u32).map(|i| (i * 7) as u8).collect();
        let mut a = orig.clone();
        cyclic(&mut a, 0x1234_5678);
        let mut b = orig.clone();
        cyclic(&mut b, 0x1234_5679);
        assert_ne!(a, orig);
        assert_ne!(a, b);
        cyclic(&mut a, 0x1234_5678);
        assert_eq!(a, orig);
    }
}
