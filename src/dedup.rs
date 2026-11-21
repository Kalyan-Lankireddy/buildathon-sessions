//! Exact dedup by key bytes: radix-partition records by hash into shards, then inside a shard
//! sort by (hash, rank) and keep the first record of each distinct key. A 64-bit hash match is
//! confirmed by comparing the key bytes, so a collision can never merge two different keys.

use crate::csv::B;
use rayon::prelude::*;

use crate::parse::SHARDS;

pub trait Rec: Copy + Send + Sync {
    fn hash(&self) -> u64;
    fn key(&self) -> B;
    /// Smaller rank wins within one key.
    fn rank(&self) -> i64;
}

/// One winner per distinct key (smallest rank), mapped through `keep` (None drops it).
/// `parts[c][s]` holds chunk c's records for shard s (routed by hash while parsing).
/// Returns the results grouped by shard; their order is otherwise unspecified.
pub fn dedup<R: Rec, O: Send>(parts: &[Vec<Vec<R>>], keep: impl Fn(&R) -> Option<O> + Sync) -> Vec<Vec<O>> {
    (0..SHARDS)
        .into_par_iter()
        .map(|s| {
            let total: usize = parts.iter().map(|c| c[s].len()).sum();
            let mut sh: Vec<R> = Vec::with_capacity(total);
            for c in parts {
                sh.extend_from_slice(&c[s]);
            }
            // open addressing on the 64-bit hash; a slot holds the index of the best record so far.
            // Equal hashes are confirmed by comparing the key bytes, so a collision between two
            // different keys just takes the next slot.
            let cap = (total * 2).next_power_of_two().max(16);
            let mask = cap - 1;
            let mut table = vec![u32::MAX; cap];
            for i in 0..sh.len() {
                let h = sh[i].hash();
                let mut p = (h as usize) & mask;
                loop {
                    let j = table[p];
                    if j == u32::MAX {
                        table[p] = i as u32;
                        break;
                    }
                    let r = &sh[j as usize];
                    if r.hash() == h && r.key() == sh[i].key() {
                        if sh[i].rank() < r.rank() {
                            table[p] = i as u32;
                        }
                        break;
                    }
                    p = (p + 1) & mask;
                }
            }
            let mut out = Vec::with_capacity(total);
            for &j in &table {
                if j != u32::MAX {
                    if let Some(o) = keep(&sh[j as usize]) {
                        out.push(o);
                    }
                }
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
        let mut c1: Vec<Vec<T>> = vec![vec![]; SHARDS];
        c1[0] = vec![T(7, b"a", 5), T(7, b"b", 9), T(3, b"c", 1)];
        let mut c2: Vec<Vec<T>> = vec![vec![]; SHARDS];
        c2[0] = vec![T(7, b"a", 2), T(7, b"b", 4)];
        let chunks = vec![c1, c2];
        let mut got: Vec<(B, i64)> = dedup(&chunks, |r| Some((r.1, r.2))).into_iter().flatten().collect();
        got.sort();
        assert_eq!(got, vec![(&b"a"[..], 2), (&b"b"[..], 4), (&b"c"[..], 1)]);
    }
}
