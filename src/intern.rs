//! Parallel exact interning: distinct byte strings get dense u32 ids.
//!
//! The caller supplies each key's 32-bit hash (computed at parse time, while the line was hot).
//! 1. scatter (key bytes, hash, input index) into 256 shards by the top hash bits;
//! 2. each shard builds its own open-addressing table, prefetching the key bytes a few rows
//!    ahead. A match needs equal hash AND equal bytes, so the result is exact;
//! 3. shard-local ids are shifted by the shard's base to give global ids.

use crate::csv::B;
use crate::util::{prefetch, SyncPtr};
use rayon::prelude::*;

const SHARD_BITS: u32 = 8;
const SHARDS: usize = 1 << SHARD_BITS;
const AHEAD: usize = 12;

pub struct Interned {
    /// id of each input key, in input order
    pub ids: Vec<u32>,
    /// the bytes of each id
    pub names: Vec<B>,
}

#[derive(Clone, Copy)]
struct Item {
    key: B,
    hash: u32,
    idx: u32,
}

#[inline]
fn shard(h: u32) -> usize {
    (h >> (32 - SHARD_BITS)) as usize
}

pub fn intern(keys: &[B], hashes: &[u32]) -> Interned {
    let n = keys.len();
    assert!(n < u32::MAX as usize && hashes.len() == n);

    // counting sort of the keys by shard
    let nt = rayon::current_num_threads().max(1) * 4;
    let per = n.div_ceil(nt).max(1);
    let ranges: Vec<(usize, usize)> = (0..nt).map(|t| ((t * per).min(n), ((t + 1) * per).min(n))).collect();
    let counts: Vec<[u32; SHARDS]> = ranges
        .par_iter()
        .map(|&(a, b)| {
            let mut c = [0u32; SHARDS];
            for &h in &hashes[a..b] {
                c[shard(h)] += 1;
            }
            c
        })
        .collect();
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
    let mut items: Vec<Item> = Vec::with_capacity(n);
    {
        let ip = SyncPtr(items.as_mut_ptr());
        ranges.par_iter().zip(offsets.par_iter_mut()).for_each(|(&(a, b), off)| {
            for i in a..b {
                let s = shard(hashes[i]);
                unsafe { ip.write(off[s], Item { key: keys[i], hash: hashes[i], idx: i as u32 }) };
                off[s] += 1;
            }
        });
        unsafe { items.set_len(n) };
    }

    // each shard builds its table and writes shard-local ids
    let mut ids: Vec<u32> = Vec::with_capacity(n);
    #[allow(clippy::uninit_vec)]
    unsafe {
        ids.set_len(n)
    };
    let idp = SyncPtr(ids.as_mut_ptr());
    let shard_names: Vec<Vec<B>> = (0..SHARDS)
        .into_par_iter()
        .map(|s| {
            let members = &items[bounds[s]..bounds[s + 1]];
            let cap = (members.len() * 2).next_power_of_two().max(16);
            let mask = cap - 1;
            let mut slot_hash = vec![0u32; cap];
            let mut slot_id = vec![u32::MAX; cap];
            let mut names: Vec<B> = Vec::new();
            for (k, it) in members.iter().enumerate() {
                if let Some(ahead) = members.get(k + AHEAD) {
                    prefetch(ahead.key.as_ptr());
                }
                let mut p = (it.hash as usize) & mask;
                let id = loop {
                    let sid = slot_id[p];
                    if sid == u32::MAX {
                        let id = names.len() as u32;
                        names.push(it.key);
                        slot_hash[p] = it.hash;
                        slot_id[p] = id;
                        break id;
                    }
                    if slot_hash[p] == it.hash && names[sid as usize] == it.key {
                        break sid;
                    }
                    p = (p + 1) & mask;
                };
                unsafe { idp.write(it.idx as usize, id) };
            }
            names
        })
        .collect();
    drop(items);

    let mut base = vec![0u32; SHARDS];
    let mut tot = 0u32;
    for s in 0..SHARDS {
        base[s] = tot;
        tot += shard_names[s].len() as u32;
    }
    ids.par_iter_mut().zip(hashes.par_iter()).for_each(|(id, &h)| {
        *id += base[shard(h)];
    });
    let names: Vec<B> = shard_names.into_iter().flatten().collect();
    Interned { ids, names }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::hash32;
    #[test]
    fn exact() {
        let keys: Vec<B> = vec![b"a", b"b", b"a", b"", b"b", b"ab", b"a "];
        let hashes: Vec<u32> = keys.iter().map(|k| hash32(k)).collect();
        let r = intern(&keys, &hashes);
        assert_eq!(r.names.len(), 5);
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(r.names[r.ids[i] as usize], *k);
        }
        assert_eq!(r.ids[0], r.ids[2]);
        assert_ne!(r.ids[0], r.ids[6]);
    }

    /// Different keys forced onto one hash must still get different ids.
    #[test]
    fn forced_collisions() {
        let keys: Vec<B> = vec![b"x", b"y", b"x", b"z", b"y"];
        let hashes = vec![7u32; 5];
        let r = intern(&keys, &hashes);
        assert_eq!(r.names.len(), 3);
        assert_eq!(r.ids[0], r.ids[2]);
        assert_eq!(r.ids[1], r.ids[4]);
        assert_ne!(r.ids[0], r.ids[1]);
        assert_ne!(r.ids[1], r.ids[3]);
    }
}
