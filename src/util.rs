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

/// Sort (key, payload) pairs by key. The keys' top bits are the person: partition by those bits
/// into 256 buckets (one counting pass), then sort each bucket on its own thread, where it stays
/// in cache. Very large buckets (a whale or a big shared-device group) are sorted in parallel.
pub fn sort_by_key_bucketed(arr: Vec<(u64, u64)>, key_bits: u32, top_bits_used: u32) -> Vec<(u64, u64)> {
    use rayon::prelude::*;
    const NB: usize = 256;
    let n = arr.len();
    if n < (1 << 16) || top_bits_used < 8 {
        let mut a = arr;
        a.par_sort_unstable_by_key(|x| x.0);
        return a;
    }
    // bucket = the 8 highest bits of the part of the key that holds the person
    let shift = key_bits - 8;
    let nt = rayon::current_num_threads().max(1) * 4;
    let per = n.div_ceil(nt);
    let ranges: Vec<(usize, usize)> = (0..nt).map(|t| ((t * per).min(n), ((t + 1) * per).min(n))).collect();
    let counts: Vec<[u32; NB]> = ranges
        .par_iter()
        .map(|&(a, b)| {
            let mut c = [0u32; NB];
            for x in &arr[a..b] {
                c[(x.0 >> shift) as usize] += 1;
            }
            c
        })
        .collect();
    let mut offsets = vec![[0usize; NB]; nt];
    let mut bounds = vec![0usize; NB + 1];
    let mut run = 0;
    for s in 0..NB {
        bounds[s] = run;
        for t in 0..nt {
            offsets[t][s] = run;
            run += counts[t][s] as usize;
        }
    }
    bounds[NB] = run;
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(n);
    let op = SyncPtr(out.as_mut_ptr());
    ranges.par_iter().zip(offsets.par_iter_mut()).for_each(|(&(a, b), off)| {
        for x in &arr[a..b] {
            let s = (x.0 >> shift) as usize;
            unsafe { op.write(off[s], *x) };
            off[s] += 1;
        }
    });
    unsafe { out.set_len(n) };
    drop(arr);
    let big = n / 16;
    (0..NB).into_par_iter().for_each(|s| {
        let sl = unsafe { op.slice_mut(bounds[s], bounds[s + 1] - bounds[s]) };
        if sl.len() > big.max(1 << 20) {
            sl.par_sort_unstable_by_key(|x| x.0);
        } else {
            sl.sort_unstable_by_key(|x| x.0);
        }
    });
    out
}
