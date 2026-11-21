//! Exact dedup by key bytes, in place.
//!
//! Records arrive already routed into hash shards (`parts[chunk][shard]`). Each shard is handled
//! by one thread with an open-addressing table over the 64-bit hash. Equal hashes are confirmed by
//! comparing the key bytes, so a collision between two different keys just takes the next slot.
//! Of two copies with the same key the one with the smaller rank stays; the other is marked dead
//! where it lies. No record is copied.

use crate::parse::SHARDS;
use crate::util::SyncPtr;
use rayon::prelude::*;

pub trait Rec: Copy + Send + Sync {
    fn hash(&self) -> u64;
    fn same_key(&self, o: &Self) -> bool;
    /// Smaller rank wins within one key.
    fn rank(&self) -> i64;
    fn kill(&mut self);
    fn alive(&self) -> bool;
}

pub fn dedup_in_place<R: Rec>(parts: &mut [Vec<Vec<R>>]) {
    let nc = parts.len();
    let mut ptrs: Vec<(SyncPtr<R>, usize)> = Vec::with_capacity(SHARDS * nc);
    for s in 0..SHARDS {
        for c in parts.iter_mut() {
            ptrs.push((SyncPtr(c[s].as_mut_ptr()), c[s].len()));
        }
    }
    (0..SHARDS).into_par_iter().for_each(|s| {
        let mine = &ptrs[s * nc..(s + 1) * nc];
        let total: usize = mine.iter().map(|m| m.1).sum();
        let cap = (total * 2).next_power_of_two().max(16);
        let mask = cap - 1;
        // slot = chunk << 32 | index, or MAX when empty
        let mut table = vec![u64::MAX; cap];
        for (c, &(base, len)) in mine.iter().enumerate() {
            for i in 0..len {
                // SAFETY: shard s is owned by this thread; indexes are in bounds.
                let r = unsafe { &mut *base.0.add(i) };
                let h = r.hash();
                let mut p = (h as usize) & mask;
                loop {
                    let e = table[p];
                    if e == u64::MAX {
                        table[p] = ((c as u64) << 32) | i as u64;
                        break;
                    }
                    let (c2, i2) = ((e >> 32) as usize, (e & 0xffff_ffff) as usize);
                    let q = unsafe { &mut *mine[c2].0 .0.add(i2) };
                    if q.hash() == h && q.same_key(r) {
                        if r.rank() < q.rank() {
                            q.kill();
                            table[p] = ((c as u64) << 32) | i as u64;
                        } else {
                            r.kill();
                        }
                        break;
                    }
                    p = (p + 1) & mask;
                }
            }
        }
    });
}

/// Alive records only, copied into one Vec (in shard, chunk order).
pub fn flatten_alive<R: Rec>(parts: &[Vec<Vec<R>>]) -> Vec<R> {
    let slices: Vec<&[R]> = (0..SHARDS).flat_map(|s| parts.iter().map(move |c| c[s].as_slice())).collect();
    let counts: Vec<usize> = slices.par_iter().map(|sl| sl.iter().filter(|r| r.alive()).count()).collect();
    let mut off = Vec::with_capacity(slices.len() + 1);
    let mut run = 0;
    for &c in &counts {
        off.push(run);
        run += c;
    }
    let mut out: Vec<R> = Vec::with_capacity(run);
    let op = SyncPtr(out.as_mut_ptr());
    slices.par_iter().zip(off.par_iter()).for_each(|(sl, &o)| {
        let mut j = o;
        for r in sl.iter().filter(|r| r.alive()) {
            unsafe { op.write(j, *r) };
            j += 1;
        }
    });
    unsafe { out.set_len(run) };
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    type B = &'static [u8];
    #[derive(Clone, Copy)]
    struct T(u64, B, i64, bool);
    impl Rec for T {
        fn hash(&self) -> u64 {
            self.0
        }
        fn same_key(&self, o: &Self) -> bool {
            self.1 == o.1
        }
        fn rank(&self) -> i64 {
            self.2
        }
        fn kill(&mut self) {
            self.3 = false;
        }
        fn alive(&self) -> bool {
            self.3
        }
    }

    #[test]
    fn smallest_rank_and_collisions() {
        // "a" and "b" share the hash 7 on purpose
        let mut c1: Vec<Vec<T>> = vec![vec![]; SHARDS];
        c1[0] = vec![T(7, b"a", 5, true), T(7, b"b", 9, true), T(3, b"c", 1, true)];
        let mut c2: Vec<Vec<T>> = vec![vec![]; SHARDS];
        c2[0] = vec![T(7, b"a", 2, true), T(7, b"b", 4, true)];
        let mut parts = vec![c1, c2];
        dedup_in_place(&mut parts);
        let mut got: Vec<(B, i64)> = flatten_alive(&parts).iter().map(|r| (r.1, r.2)).collect();
        got.sort();
        assert_eq!(got, vec![(&b"a"[..], 2), (&b"b"[..], 4), (&b"c"[..], 1)]);
    }
}
