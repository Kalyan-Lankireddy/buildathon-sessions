//! A byte string of up to 15 bytes stored inline in 16 bytes, so comparing, hashing and
//! interning IDs never chases a pointer into the mapped file. Longer strings keep a pointer and a
//! length and are compared byte by byte, so equality is always exact.
//!
//! Layout (little endian): inline keys hold the bytes in order, zero padded, with the length in
//! the top byte. Long keys have 0xFF in the top byte, the pointer in `lo`, the length in `hi`.

use crate::csv::{line_at, B};
use crate::util::hash_bytes;

#[derive(Clone, Copy)]
#[repr(C)]
pub struct Key {
    lo: u64,
    hi: u64,
}

const LONG: u64 = 0xFF;

impl Key {
    #[inline]
    fn long(b: B) -> Key {
        Key { lo: b.as_ptr() as u64, hi: b.len() as u64 | (LONG << 56) }
    }

    /// Always safe: copies byte by byte.
    #[inline]
    pub fn safe(b: B) -> Key {
        let n = b.len();
        if n > 15 {
            return Key::long(b);
        }
        let mut buf = [0u8; 16];
        buf[..n].copy_from_slice(b);
        Key {
            lo: u64::from_le_bytes(buf[..8].try_into().unwrap()),
            hi: u64::from_le_bytes(buf[8..].try_into().unwrap()) | ((n as u64) << 56),
        }
    }

    /// Fast path for a key that sits inside `line`: two unaligned 8-byte loads and a mask, never
    /// reading outside the line. Reads forward from the key when 16 bytes remain in the line,
    /// else backward so the load ends at the key's last byte, else copies byte by byte.
    #[inline]
    pub fn hot(b: B, line: &[u8]) -> Key {
        let n = b.len();
        if n > 15 {
            return Key::long(b);
        }
        let start = b.as_ptr() as usize;
        let (lo_line, hi_line) = (line.as_ptr() as usize, line.as_ptr() as usize + line.len());
        if hi_line - start >= 16 {
            let (lo, hi) = unsafe {
                let p = b.as_ptr();
                ((p as *const u64).read_unaligned(), (p.add(8) as *const u64).read_unaligned())
            };
            let lo_mask = if n >= 8 { u64::MAX } else { (1u64 << (8 * n)) - 1 };
            let hi_mask = if n <= 8 { 0 } else { (1u64 << (8 * (n - 8))) - 1 };
            Key { lo: lo & lo_mask, hi: (hi & hi_mask) | ((n as u64) << 56) }
        } else if start + n - lo_line >= 16 {
            // the 16 bytes that end where the key ends; shift the key down to byte 0
            let v = unsafe { ((b.as_ptr().add(n).sub(16)) as *const u128).read_unaligned() };
            let v = if n == 0 { 0 } else { v >> (8 * (16 - n)) };
            Key { lo: v as u64, hi: ((v >> 64) as u64) | ((n as u64) << 56) }
        } else {
            Key::safe(b)
        }
    }

    #[inline]
    fn is_long(&self) -> bool {
        self.hi >> 56 == LONG
    }

    #[inline]
    pub fn same(&self, o: &Key) -> bool {
        if self.lo == o.lo && self.hi == o.hi {
            return true;
        }
        self.is_long() && o.is_long() && self.bytes() == o.bytes()
    }

    #[inline]
    pub fn len(&self) -> usize {
        if self.is_long() {
            (self.hi & 0xFFFF_FFFF_FFFF) as usize
        } else {
            (self.hi >> 56) as usize
        }
    }

    #[inline]
    pub fn bytes(&self) -> &[u8] {
        if self.is_long() {
            line_at(self.lo as usize, (self.hi & 0xFFFF_FFFF) as u32)
        } else {
            // SAFETY: repr(C) little endian: the first `len` bytes of the struct are the key.
            unsafe { std::slice::from_raw_parts(self as *const Key as *const u8, (self.hi >> 56) as usize) }
        }
    }

    #[inline]
    pub fn hash64(&self) -> u64 {
        if self.is_long() {
            return hash_bytes(self.bytes());
        }
        let mut x = self.lo.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ self.hi.wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        x ^= x >> 32;
        x = x.wrapping_mul(0x1656_67B1_9E37_79F9);
        x ^ (x >> 29)
    }

    #[inline]
    pub fn hash32(&self) -> u32 {
        let x = self.hash64();
        (x ^ (x >> 32)) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_and_long() {
        let long: B = b"0123456789abcdefXYZ";
        for s in [&b""[..], b"a", b"abcdefg", b"abcdefgh", b"abcdefghijklmno", long, b"abcdefghijklmnop"] {
            // pad with bytes so `hot` has room to read
            let padded: B = Box::leak([s, &[b'#'; 32][..]].concat().into_boxed_slice());
            let sl = &padded[..s.len()];
            let a = Key::safe(sl);
            let b = Key::hot(sl, &padded[..]);
            let c = Key::hot(sl, &padded[..s.len()]); // key fills the whole line: safe path
            assert!(a.same(&c));
            assert!(a.same(&b), "{s:?}");
            assert_eq!(a.bytes(), s);
            assert_eq!(b.bytes(), s);
            assert_eq!(a.len(), s.len());
            assert_eq!(a.hash64(), b.hash64());
        }
        // backward read: the key is the tail of a longer line
        let line: B = Box::leak(b"xxxxxxxxxxxxxxxxxxxxxxxxabc,12".to_vec().into_boxed_slice());
        let kb = Key::hot(&line[24..27], line);
        assert!(kb.same(&Key::safe(b"abc")));
        let line2: B = Box::leak(b"0123456789abcdefghijklmnopqrstu".to_vec().into_boxed_slice());
        for n in 0..=15usize {
            let sl = &line2[31 - n..];
            assert!(Key::hot(sl, line2).same(&Key::safe(sl)), "backward n={n}");
        }
        assert!(!Key::safe(b"ab").same(&Key::safe(b"ab ")));
        assert!(!Key::safe(b"").same(&Key::safe(b"\0")));
        let l1: B = Box::leak(b"0123456789abcdefXYZ".to_vec().into_boxed_slice());
        let l2: B = Box::leak(b"0123456789abcdefXYZ".to_vec().into_boxed_slice());
        let l3: B = Box::leak(b"0123456789abcdefXYz".to_vec().into_boxed_slice());
        assert!(Key::safe(l1).same(&Key::safe(l2)));
        assert!(!Key::safe(l1).same(&Key::safe(l3)));
    }
}
