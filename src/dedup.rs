//! Exact dedup by key bytes: radix-partition records by hash into shards, then inside a shard
//! sort by (hash, rank) and keep the first record of each distinct key. A 64-bit hash match is
//! confirmed by comparing the key bytes, so a collision can never merge two different keys.

use crate::csv::B;
use crate::util::SyncPtr;
use rayon::prelude::*;

const SHARD_BITS: u32 = 8;
const SHARDS: usize = 1 << SHARD_BITS;

pub trait Rec: Copy + Send + Sync {
    fn hash(&self) -> u64;
    fn key(&self) -> B;
    /// Smaller rank wins within one key.
    fn rank(&self) -> i64;
}

/// Partition every record of every chunk into shards by hash.
fn partition<R: Rec>(chunks: &[Vec<R>]) -> (Vec<R>, Vec<usize>) {
    let n: usize = chunks.iter().map(|c| c.len()).sum();
    let nc = chunks.len();
    let counts: Vec<[u32; SHARDS]> = chunks
        .par_iter()
        .map(|c| {
            let mut a = [0u32; SHARDS];
            for r in c {
                a[(r.hash() >> (64 - SHARD_BITS)) as usize] += 1;
            }
            a
        })
        .collect();
    let mut offsets = vec![[0usize; SHARDS]; nc];
    let mut bounds = vec![0usize; SHARDS + 1];
    let mut run = 0;
    for s in 0..SHARDS {
        bounds[s] = run;
        for t in 0..nc {
            offsets[t][s] = run;
            run += counts[t][s] as usize;
        }
    }
    bounds[SHARDS] = run;
    let mut out: Vec<R> = Vec::with_capacity(n);
    #[allow(clippy::uninit_vec)]
    unsafe {
        out.set_len(n)
    };
    let optr = SyncPtr(out.as_mut_ptr());
    chunks.par_iter().zip(offsets.par_iter_mut()).for_each(|(c, off)| {
        for r in c {
            let s = (r.hash() >> (64 - SHARD_BITS)) as usize;
            unsafe { optr.write(off[s], *r) };
            off[s] += 1;
        }
    });
    (out, bounds)
}

/// One winner per distinct key (smallest rank), mapped through `keep` (None drops it).
/// Returns the results grouped by shard; their order is otherwise unspecified.
pub fn dedup<R: Rec, O: Send>(chunks: &[Vec<R>], keep: impl Fn(&R) -> Option<O> + Sync) -> Vec<Vec<O>> {
    let (mut recs, bounds) = partition(chunks);
    // split into disjoint shard slices
    let mut slices: Vec<&mut [R]> = Vec::with_capacity(SHARDS);
    let mut rest: &mut [R] = &mut recs;
    for s in 0..SHARDS {
        let (a, b) = rest.split_at_mut(bounds[s + 1] - bounds[s]);
        slices.push(a);
        rest = b;
    }
    slices
        .into_par_iter()
        .map(|sh| {
            sh.sort_unstable_by_key(|r| (r.hash(), r.rank()));
            let mut out = Vec::with_capacity(sh.len());
            let mut i = 0;
            while i < sh.len() {
                let h = sh[i].hash();
                let mut j = i + 1;
                while j < sh.len() && sh[j].hash() == h {
                    j += 1;
                }
                // sh[i..j] share a hash; the first of each distinct key (by bytes) wins
                let first = sh[i].key();
                if sh[i + 1..j].iter().all(|r| r.key() == first) {
                    if let Some(o) = keep(&sh[i]) {
                        out.push(o);
                    }
                } else {
                    // a hash collision between different keys: group by bytes exactly
                    let mut winners: Vec<usize> = Vec::new();
                    for k in i..j {
                        if !winners.iter().any(|&w| sh[w].key() == sh[k].key()) {
                            winners.push(k);
                        }
                    }
                    for w in winners {
                        if let Some(o) = keep(&sh[w]) {
                            out.push(o);
                        }
                    }
                }
                i = j;
            }
            out
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    struct T(u64, B, i64);
    impl Rec for T {
        fn hash(&self) -> u64 {
            self.0
        }
        fn key(&self) -> B {
            self.1
        }
        fn rank(&self) -> i64 {
            self.2
        }
    }

    #[test]
    fn smallest_rank_and_collisions() {
        // "a" and "b" share the hash 7 on purpose
        let chunks = vec![vec![T(7, b"a", 5), T(7, b"b", 9), T(3, b"c", 1)], vec![T(7, b"a", 2), T(7, b"b", 4)]];
        let mut got: Vec<(B, i64)> = dedup(&chunks, |r| Some((r.1, r.2))).into_iter().flatten().collect();
        got.sort();
        assert_eq!(got, vec![(&b"a"[..], 2), (&b"b"[..], 4), (&b"c"[..], 1)]);
    }
}
