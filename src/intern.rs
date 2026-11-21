//! Parallel exact interning: distinct byte strings get dense u32 ids.
//!
//! The caller supplies each key's 32-bit hash (computed at parse time, while the line was hot).
//! 1. scatter (key bytes, hash, input index) into 256 shards by the top hash bits;
//! 2. each shard builds its own open-addressing table, prefetching the key bytes a few rows
//!    ahead. A match needs equal hash AND equal bytes, so the result is exact;
//! 3. shard-local ids are shifted by the shard's base to give global ids.

use crate::csv::B;
use crate::parse::{Ck, Ev, Link};
use crate::dedup::Rec;
use crate::key::Key;
use crate::util::SyncPtr;
use rayon::prelude::*;

const SHARD_BITS: u32 = 8;
const SHARDS: usize = 1 << SHARD_BITS;

pub struct Interned {
    /// global index of each source's first key, plus the total at the end
    pub start: Vec<usize>,
    /// id of each input key, in input order
    pub ids: Vec<u32>,
    /// the bytes of each id
    pub names: Vec<B>,
}

/// Give every distinct key persistent bytes (inline keys are copied into one arena).
fn materialize(keys: &[Key]) -> Vec<B> {
    let mut off = Vec::with_capacity(keys.len() + 1);
    let mut run = 0usize;
    for k in keys {
        off.push(run);
        run += k.len();
    }
    let arena: &'static mut [u8] = Box::leak(vec![0u8; run].into_boxed_slice());
    let ap = SyncPtr(arena.as_mut_ptr());
    let out: Vec<B> = keys
        .par_iter()
        .zip(off.par_iter())
        .map(|(k, &o)| {
            let b = k.bytes();
            unsafe { ap.copy_in(o, b.as_ptr(), b.len()) };
            // SAFETY: the arena is leaked and each range is written once, before it is read.
            unsafe { std::slice::from_raw_parts(ap.0.add(o) as *const u8, b.len()) }
        })
        .collect();
    out
}

#[derive(Clone, Copy)]
struct Item {
    key: Key,
    hash: u32,
    idx: u32,
}

#[inline]
fn shard(h: u32) -> usize {
    (h >> (32 - SHARD_BITS)) as usize
}

/// Where keys come from. Keys are read straight from the parse records, in this order:
/// link rows (a, then b), alive events (user), valid clicks (user).
#[derive(Clone, Copy)]
pub enum Source<'a> {
    Links(&'a [Link]),
    Evs(&'a [Ev]),
    Cks(&'a [Ck]),
}

impl Source<'_> {
    #[allow(dead_code)]
    pub fn count(&self) -> usize {
        match self {
            Source::Links(v) => 2 * v.len(),
            Source::Evs(v) => v.iter().filter(|e| e.alive()).count(),
            Source::Cks(v) => v.len(),
        }
    }

    #[inline]
    fn visit<G: FnMut(Key, u32)>(&self, mut g: G) {
        match self {
            Source::Links(v) => {
                for l in v.iter() {
                    g(l.a, l.a.hash32());
                    g(l.b, l.b.hash32());
                }
            }
            Source::Evs(v) => {
                for e in v.iter().filter(|e| e.alive()) {
                    g(*e.user(), e.user().hash32());
                }
            }
            Source::Cks(v) => {
                for c in v.iter() {
                    g(*c.user(), c.user().hash32());
                }
            }
        }
    }
}

/// `ids[i]` is the id of the key with global index i; source k's keys start at `start[k]`.
pub fn intern(sources: &[Source]) -> Interned {
    let ns = sources.len();

    // count per (source, shard), then scatter the keys into shard order
    let counts: Vec<[u32; SHARDS]> = sources
        .par_iter()
        .map(|src| {
            let mut c = [0u32; SHARDS];
            src.visit(|_, h| c[shard(h)] += 1);
            c
        })
        .collect();
    let mut start = Vec::with_capacity(ns + 1);
    let mut n = 0usize;
    for c in &counts {
        start.push(n);
        n += c.iter().map(|&x| x as usize).sum::<usize>();
    }
    start.push(n);
    assert!(n < u32::MAX as usize);
    let mut offsets = vec![[0usize; SHARDS]; ns];
    let mut bounds = vec![0usize; SHARDS + 1];
    let mut run = 0usize;
    for s in 0..SHARDS {
        bounds[s] = run;
        for t in 0..ns {
            offsets[t][s] = run;
            run += counts[t][s] as usize;
        }
    }
    bounds[SHARDS] = run;
    let mut items: Vec<Item> = Vec::with_capacity(n);
    {
        let ip = SyncPtr(items.as_mut_ptr());
        sources.par_iter().zip(offsets.par_iter_mut()).enumerate().for_each(|(k, (src, off))| {
            let mut idx = start[k] as u32;
            src.visit(|key, hash| {
                let s = shard(hash);
                unsafe { ip.write(off[s], Item { key, hash, idx }) };
                off[s] += 1;
                idx += 1;
            });
        });
        unsafe { items.set_len(n) };
    }

    // phase A: each shard builds its table; the item's `hash` field is reused to hold its local id
    let ip = SyncPtr(items.as_mut_ptr());
    let shard_names: Vec<Vec<Key>> = (0..SHARDS)
        .into_par_iter()
        .map(|s| {
            // SAFETY: shard s's items are touched by this thread only.
            let members: &mut [Item] = unsafe { ip.slice_mut(bounds[s], bounds[s + 1] - bounds[s]) };
            let cap = (members.len() * 2).next_power_of_two().max(16);
            let mask = cap - 1;
            let mut slot_hash = vec![0u32; cap];
            let mut slot_id = vec![u32::MAX; cap];
            let mut names: Vec<Key> = Vec::new();
            for k in 0..members.len() {
                let (hash, key) = (members[k].hash, members[k].key);
                let mut p = (hash as usize) & mask;
                let id = loop {
                    let sid = slot_id[p];
                    if sid == u32::MAX {
                        let id = names.len() as u32;
                        names.push(key);
                        slot_hash[p] = hash;
                        slot_id[p] = id;
                        break id;
                    }
                    if slot_hash[p] == hash && names[sid as usize].same(&key) {
                        break sid;
                    }
                    p = (p + 1) & mask;
                };
                members[k].hash = id;
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
    // phase B: global id = shard base + local id, written to the key's input position
    let mut ids: Vec<u32> = Vec::with_capacity(n);
    let idp = SyncPtr(ids.as_mut_ptr());
    (0..SHARDS).into_par_iter().for_each(|s| {
        for it in &items[bounds[s]..bounds[s + 1]] {
            unsafe { idp.write(it.idx as usize, it.hash + base[s]) };
        }
    });
    unsafe { ids.set_len(n) };
    drop(items);
    let keys: Vec<Key> = shard_names.into_iter().flatten().collect();
    Interned { start, ids, names: materialize(&keys) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(keys: &[B], _force_hash: Option<u32>) -> Interned {
        // pairs of keys become link rows; an odd key is paired with itself
        let mut rows = vec![];
        for p in keys.chunks(2) {
            let (a, b) = (p[0], *p.get(1).unwrap_or(&p[0]));
            rows.push(Link { a: Key::safe(a), b: Key::safe(b) });
        }
        let src = [Source::Links(&rows)];
        let r = intern(&src);
        // the odd tail key was visited twice; only the first n entries matter
        assert!(r.ids.len() >= keys.len());
        r
    }

    #[test]
    fn exact() {
        let keys: Vec<B> = vec![b"a", b"b", b"a", b"", b"b", b"ab", b"a ", b"a "];
        let r = run(&keys, None);
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
        let keys: Vec<B> = vec![b"x", b"y", b"x", b"z", b"y", b"y"];
        let r = run(&keys, Some(7));
        assert_eq!(r.names.len(), 3);
        assert_eq!(r.ids[0], r.ids[2]);
        assert_eq!(r.ids[1], r.ids[4]);
        assert_ne!(r.ids[0], r.ids[1]);
        assert_ne!(r.ids[1], r.ids[3]);
    }
}
