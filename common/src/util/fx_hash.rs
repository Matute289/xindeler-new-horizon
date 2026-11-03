//! Fx hashers, bit-for-bit identical to the unmaintained `fxhash` 0.2.1 crate
//! (RUSTSEC-2025-0057).
//!
//! These are kept in-tree instead of switching to `rustc-hash` on purpose:
//! `rustc-hash` 2.x uses a different algorithm, which would change map
//! iteration order and with it world generation and A* tie-breaking for an
//! existing seed. The golden values in the tests below were produced by
//! `fxhash` 0.2.1 itself.
use std::{
    hash::{BuildHasherDefault, Hasher},
    ops::BitXor,
};

pub type FxBuildHasher = BuildHasherDefault<FxHasher>;

const ROTATE: u32 = 5;
const SEED64: u64 = 0x517cc1b727220a95;
const SEED32: u32 = (SEED64 & 0xFFFF_FFFF) as u32;
#[cfg(target_pointer_width = "32")]
const SEED: usize = SEED32 as usize;
#[cfg(target_pointer_width = "64")]
const SEED: usize = SEED64 as usize;

trait HashWord {
    fn hash_word(&mut self, word: Self);
}

macro_rules! impl_hash_word {
    ($($ty:ty = $key:ident),* $(,)*) => (
        $(
            impl HashWord for $ty {
                #[inline]
                fn hash_word(&mut self, word: Self) {
                    *self = self.rotate_left(ROTATE).bitxor(word).wrapping_mul($key);
                }
            }
        )*
    )
}
impl_hash_word!(usize = SEED, u32 = SEED32, u64 = SEED64);

#[inline]
fn write32(mut hash: u32, mut bytes: &[u8]) -> u32 {
    while let Some((chunk, rest)) = bytes.split_first_chunk::<4>() {
        hash.hash_word(u32::from_ne_bytes(*chunk));
        bytes = rest;
    }
    for byte in bytes {
        hash.hash_word(*byte as u32);
    }
    hash
}

#[inline]
fn write64(mut hash: u64, mut bytes: &[u8]) -> u64 {
    while let Some((chunk, rest)) = bytes.split_first_chunk::<8>() {
        hash.hash_word(u64::from_ne_bytes(*chunk));
        bytes = rest;
    }
    if let Some((chunk, rest)) = bytes.split_first_chunk::<4>() {
        hash.hash_word(u32::from_ne_bytes(*chunk) as u64);
        bytes = rest;
    }
    for byte in bytes {
        hash.hash_word(*byte as u64);
    }
    hash
}

/// Pointer-width Fx hasher (`fxhash::FxHasher`).
#[derive(Debug, Clone, Default)]
pub struct FxHasher {
    hash: usize,
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        #[cfg(target_pointer_width = "32")]
        {
            self.hash = write32(self.hash as u32, bytes) as usize;
        }
        #[cfg(target_pointer_width = "64")]
        {
            self.hash = write64(self.hash as u64, bytes) as usize;
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) { self.hash.hash_word(i as usize); }

    #[inline]
    fn write_u16(&mut self, i: u16) { self.hash.hash_word(i as usize); }

    #[inline]
    fn write_u32(&mut self, i: u32) { self.hash.hash_word(i as usize); }

    #[inline]
    #[cfg(target_pointer_width = "32")]
    fn write_u64(&mut self, i: u64) {
        self.hash.hash_word(i as usize);
        self.hash.hash_word((i >> 32) as usize);
    }

    #[inline]
    #[cfg(target_pointer_width = "64")]
    fn write_u64(&mut self, i: u64) { self.hash.hash_word(i as usize); }

    #[inline]
    fn write_usize(&mut self, i: usize) { self.hash.hash_word(i); }

    #[inline]
    fn finish(&self) -> u64 { self.hash as u64 }
}

/// 64-bit Fx hasher (`fxhash::FxHasher64`).
#[derive(Debug, Clone, Default)]
pub struct FxHasher64 {
    hash: u64,
}

impl Hasher for FxHasher64 {
    #[inline]
    fn write(&mut self, bytes: &[u8]) { self.hash = write64(self.hash, bytes); }

    #[inline]
    fn write_u8(&mut self, i: u8) { self.hash.hash_word(i as u64); }

    #[inline]
    fn write_u16(&mut self, i: u16) { self.hash.hash_word(i as u64); }

    #[inline]
    fn write_u32(&mut self, i: u32) { self.hash.hash_word(i as u64); }

    #[inline]
    fn write_u64(&mut self, i: u64) { self.hash.hash_word(i); }

    #[inline]
    fn write_usize(&mut self, i: usize) { self.hash.hash_word(i as u64); }

    #[inline]
    fn finish(&self) -> u64 { self.hash }
}

/// 32-bit Fx hasher (`fxhash::FxHasher32`).
#[derive(Debug, Clone, Default)]
pub struct FxHasher32 {
    hash: u32,
}

impl Hasher for FxHasher32 {
    #[inline]
    fn write(&mut self, bytes: &[u8]) { self.hash = write32(self.hash, bytes); }

    #[inline]
    fn write_u8(&mut self, i: u8) { self.hash.hash_word(i as u32); }

    #[inline]
    fn write_u16(&mut self, i: u16) { self.hash.hash_word(i as u32); }

    #[inline]
    fn write_u32(&mut self, i: u32) { self.hash.hash_word(i); }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.hash.hash_word(i as u32);
        self.hash.hash_word((i >> 32) as u32);
    }

    #[inline]
    #[cfg(target_pointer_width = "32")]
    fn write_usize(&mut self, i: usize) { self.write_u32(i as u32); }

    #[inline]
    #[cfg(target_pointer_width = "64")]
    fn write_usize(&mut self, i: usize) { self.write_u64(i as u64); }

    #[inline]
    fn finish(&self) -> u64 { self.hash as u64 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::Hash;

    fn h64<T: Hash>(v: T) -> u64 {
        let mut s = FxHasher64::default();
        v.hash(&mut s);
        s.finish()
    }
    fn h32<T: Hash>(v: T) -> u64 {
        let mut s = FxHasher32::default();
        v.hash(&mut s);
        s.finish()
    }
    fn hw<T: Hash>(v: T) -> u64 {
        let mut s = FxHasher::default();
        v.hash(&mut s);
        s.finish()
    }

    const LONG: &str = "hello world, this is a longer string over eight bytes";
    const BYTES: [u8; 11] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

    #[test]
    fn matches_fxhash_0_2_1_golden_values() {
        assert_eq!(h64(0xdead_beef_cafe_f00du64), 18077081907773585809);
        assert_eq!(h64(LONG), 3164016198635783468);
        assert_eq!(h64((1u32, 2u32, 3i16)), 18287753081191268646);
        assert_eq!(h64(BYTES), 12832413538497138417);

        assert_eq!(h32(0xdead_beef_cafe_f00du64), 3403945232);
        assert_eq!(h32(LONG), 2624190547);
        assert_eq!(h32((1u32, 2u32, 3i16)), 2559027086);
        assert_eq!(h32(BYTES), 4266230715);

        // The pointer-width hasher equals the 64-bit one on 64-bit targets.
        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!(hw(0xdead_beef_cafe_f00du64), 18077081907773585809);
            assert_eq!(hw(LONG), 3164016198635783468);
            assert_eq!(hw((1u32, 2u32, 3i16)), 18287753081191268646);
            assert_eq!(hw(BYTES), 12832413538497138417);
        }
    }
}
