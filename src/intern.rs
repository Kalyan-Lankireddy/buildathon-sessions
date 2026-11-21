//! Parallel exact interning: distinct byte strings get dense u32 ids.
//!
//! 1. hash every key in parallel;
//! 2. scatter key indexes into 256 shards by the top hash bits (counting sort);
//! 3. each shard builds its own open-addressing table (exact: hash AND bytes must match);
//! 4. shard-local ids are shifted by the shard's base to give global ids.

use crate::csv::B;
use crate::util::{hash_bytes, SyncPtr};
use rayon::prelude::*;

const SHARD_BITS: u32 = 8;
const SHARDS: usize = 1 << SHARD_BITS;

pub struct Interned {
    /// id of each input key, in input order
    pub ids: Vec<u32>,
    /// the bytes of each id
    pub names: Vec<B>,
}

pub fn intern(keys: &[B]) -> Interned {
    let n = keys.len();
    assert!(n < u32::MAX as usize);
    let hashes: Vec<u64> = keys.par_iter().map(|k| hash_bytes(k)).collect();

    // counting sort of key indexes by shard
    let nt = rayon::current_num_threads().max(1) * 4;
    let per = n.div_ceil(nt).max(1);
    let ranges: Vec<(usize, usize)> = (0..nt).map(|t| ((t * per).min(n), ((t + 1) * per).min(n))).collect();
    let counts: Vec<[u32; SHARDS]> = ranges
        .par_iter()
        .map(|&(a, b)| {
            let mut c = [0u32; SHARDS];
            for &h in &hashes[a..b] {
                c[(h >> (64 - SHARD_BITS)) as usize] += 1;
            }
            c
        })
        .collect();
    // offsets[t][s] = where chunk t starts writing shard s
    let mut offsets = vec![[0usize; SHARDS]; nt];
    let mut bounds = vec![0usize; SHARDS + 1];
    let mut run = 0usize;
    for s in 0..SHARDS {
        bounds[s] = run;
        for t in 0..nt {
            offsets[t][s] = run;
            run += counts[t][s] as usize;
        }
    }
    bounds[SHARDS] = run;
    let mut order: Vec<u32> = Vec::with_capacity(n);
    #[allow(clippy::uninit_vec)]
    unsafe {
        order.set_len(n)
    };
    {
        let optr = SyncPtr(order.as_mut_ptr());
        ranges.par_iter().zip(offsets.par_iter_mut()).for_each(|(&(a, b), off)| {
            for i in a..b {
                let s = (hashes[i] >> (64 - SHARD_BITS)) as usize;
                unsafe { optr.write(off[s], i as u32) };
                off[s] += 1;
            }
        });
    }

    // each shard builds its table and writes shard-local ids
    let mut ids: Vec<u32> = Vec::with_capacity(n);
    #[allow(clippy::uninit_vec)]
    unsafe {
        ids.set_len(n)
    };
    let iptr = SyncPtr(ids.as_mut_ptr());
    let shard_names: Vec<Vec<B>> = (0..SHARDS)
        .into_par_iter()
        .map(|s| {
            let members = &order[bounds[s]..bounds[s + 1]];
            let cap = (members.len() * 2).next_power_of_two().max(16);
            let mask = cap - 1;
            let mut slot_hash = vec![0u64; cap];
            let mut slot_id = vec![u32::MAX; cap];
            let mut names: Vec<B> = Vec::new();
            for &ix in members {
                let h = hashes[ix as usize];
                let key = keys[ix as usize];
                let mut p = (h as usize) & mask;
                let id = loop {
                    let sid = slot_id[p];
                    if sid == u32::MAX {
                        let id = names.len() as u32;
                        names.push(key);
                        slot_hash[p] = h;
                        slot_id[p] = id;
                        break id;
                    }
                    if slot_hash[p] == h && names[sid as usize] == key {
                        break sid;
                    }
                    p = (p + 1) & mask;
                };
                unsafe { iptr.write(ix as usize, id) };
            }
            names
        })
        .collect();

    let mut base = vec![0u32; SHARDS];
    let mut tot = 0u32;
    for s in 0..SHARDS {
        base[s] = tot;
        tot += shard_names[s].len() as u32;
    }
    ids.par_iter_mut().zip(hashes.par_iter()).for_each(|(id, &h)| {
        *id += base[(h >> (64 - SHARD_BITS)) as usize];
    });
    let names: Vec<B> = shard_names.into_iter().flatten().collect();
    Interned { ids, names }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact() {
        let keys: Vec<B> = vec![b"a", b"b", b"a", b"", b"b", b"ab", b"a "];
        let r = intern(&keys);
        assert_eq!(r.names.len(), 5);
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(r.names[r.ids[i] as usize], *k);
        }
        assert_eq!(r.ids[0], r.ids[2]);
        assert_ne!(r.ids[0], r.ids[6]);
    }
}
