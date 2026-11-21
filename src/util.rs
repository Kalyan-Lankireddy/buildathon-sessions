use rustc_hash::FxHasher;
use std::hash::Hasher;

#[inline]
pub fn hash_bytes(b: &[u8]) -> u64 {
    let mut h = FxHasher::default();
    h.write(b);
    // FxHasher's low bits are weak for short keys; fold the high half in.
    let x = h.finish();
    x ^ (x >> 29)
}

/// A raw pointer that may be shared across threads. Every use writes disjoint indexes.
pub struct SyncPtr<T>(pub *mut T);
unsafe impl<T> Send for SyncPtr<T> {}
unsafe impl<T> Sync for SyncPtr<T> {}
impl<T> Clone for SyncPtr<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for SyncPtr<T> {}
impl<T> SyncPtr<T> {
    /// # Safety
    /// `i` must be in bounds and written by one thread only.
    #[inline]
    pub unsafe fn write(&self, i: usize, v: T) {
        self.0.add(i).write(v)
    }
    /// # Safety
    /// The range must be in bounds and used by one thread only.
    #[inline]
    pub unsafe fn slice_mut<'a>(&self, i: usize, n: usize) -> &'a mut [T] {
        std::slice::from_raw_parts_mut(self.0.add(i), n)
    }
    /// # Safety
    /// Copies `n` items to index `i`; the range must be in bounds and written by one thread only.
    #[inline]
    pub unsafe fn copy_in(&self, i: usize, src: *const T, n: usize) {
        std::ptr::copy_nonoverlapping(src, self.0.add(i), n)
    }
}

/// Bits needed to hold values 0..n (n distinct values).
pub fn bits_for(n: u64) -> u32 {
    if n <= 1 {
        0
    } else {
        64 - (n - 1).leading_zeros()
    }
}

/// Concatenate parts into one Vec, copying the parts in parallel.
pub fn concat_par<T: Copy + Send + Sync>(parts: &[Vec<T>]) -> Vec<T> {
    use rayon::prelude::*;
    let mut off = Vec::with_capacity(parts.len() + 1);
    let mut run = 0;
    for p in parts {
        off.push(run);
        run += p.len();
    }
    let mut out: Vec<T> = Vec::with_capacity(run);
    let ptr = SyncPtr(out.as_mut_ptr());
    parts.par_iter().zip(off.par_iter()).for_each(|(p, &o)| unsafe {
        ptr.copy_in(o, p.as_ptr(), p.len());
    });
    unsafe { out.set_len(run) };
    out
}
