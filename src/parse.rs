//! Row parsers for events, clicks and links. One pass over the bytes, no allocation per row.

use crate::csv::{fields, for_each_line, B};
use crate::key::Key;
use crate::dedup::Rec;
use crate::dims::Dims;
use crate::ts::{digits_i64, DayCache, LATE_MS, WEEK_MS};

pub const KIND_VIEW: u64 = 0;
pub const KIND_CART: u64 = 1;
pub const KIND_PURCHASE: u64 = 2;
pub const MAX_PRICE: i64 = 10_000_000;
/// ts of a copy that fails validation or is late. It still takes part in dedup.
pub const INVALID: i64 = i64::MIN;

/// pay = price (24 bits) | kind << 24 | category << 32
#[inline]
pub fn pay_price(p: u64) -> u64 {
    p & 0xFF_FFFF
}
#[inline]
pub fn pay_kind(p: u64) -> u64 {
    (p >> 24) & 0xFF
}
#[inline]
pub fn pay_cat(p: u64) -> u32 {
    (p >> 32) as u32
}

/// Records are routed into 2^SHARD_BITS hash shards while parsing, so dedup needs no copy.
pub const SHARD_BITS: u32 = 8;
pub const SHARDS: usize = 1 << SHARD_BITS;
#[inline]
pub fn shard_of(hash: u64) -> usize {
    (hash >> (64 - SHARD_BITS)) as usize
}

/// 64 bytes, no pointers: the event_id and user_id are stored inline as keys.
#[derive(Clone, Copy)]
pub struct Ev {
    pub hash: u64,
    pub ingest: i64,
    pub ts: i64,
    pub pay: u64,
    eid: Key,
    user: Key,
}
impl Ev {
    #[inline]
    pub fn user(&self) -> &Key {
        &self.user
    }
}
impl Rec for Ev {
    #[inline]
    fn hash(&self) -> u64 {
        self.hash
    }
    #[inline]
    fn same_key(&self, o: &Self) -> bool {
        self.eid.same(&o.eid)
    }
    #[inline]
    fn rank(&self) -> i64 {
        self.ingest
    }
    #[inline]
    fn kill(&mut self) {
        self.ts = INVALID;
    }
    #[inline]
    fn alive(&self) -> bool {
        self.ts != INVALID
    }
}

fn shard_buffers<R>(chunk_len: usize, bytes_per_row: usize) -> Vec<Vec<R>> {
    let per = chunk_len / bytes_per_row / SHARDS * 5 / 4 + 4;
    (0..SHARDS).map(|_| Vec::with_capacity(per)).collect()
}

pub fn parse_events(chunk: B, d: &Dims) -> Vec<Vec<Ev>> {
    let (ws, we) = (d.window_start, d.window_end);
    let mut out: Vec<Vec<Ev>> = shard_buffers(chunk.len(), 90);
    let mut days = DayCache::default();
    for_each_line(chunk, |line| {
        let Some([eid, user, tsr, ing, ty, prod, price]) = fields::<7>(line) else { return };
        if eid.is_empty() {
            return;
        }
        // ingest_ts is always valid F1; a bad one sorts last and fails the late check
        let ingest = days.parse(ing).unwrap_or(i64::MAX);
        let mut ts = INVALID;
        let mut pay = 0u64;
        'valid: {
            if user.is_empty() || prod.is_empty() {
                break 'valid;
            }
            let kind = match ty {
                b"view" => KIND_VIEW,
                b"cart" => KIND_CART,
                b"purchase" => KIND_PURCHASE,
                _ => break 'valid,
            };
            let Some(t) = days.parse(tsr) else { break 'valid };
            if t < ws || t >= we || ingest.saturating_sub(t) > LATE_MS {
                break 'valid;
            }
            let mut p = 0;
            if kind == KIND_PURCHASE {
                match digits_i64(price) {
                    Some(v) if (1..=MAX_PRICE).contains(&v) => p = v as u64,
                    _ => break 'valid,
                }
            }
            let cat = d.prod.get(&Key::hot(prod, line)).unwrap_or(d.unknown) as u64;
            ts = t;
            pay = p | (kind << 24) | (cat << 32);
        }
        let eid = Key::hot(eid, line);
        let user = Key::hot(user, line);
        let hash = eid.hash64();
        out[shard_of(hash)].push(Ev { hash, ingest, ts, pay, eid, user });
    });
    out
}

/// A valid click (56 bytes), stored like `Ev`.
#[derive(Clone, Copy)]
pub struct Ck {
    pub hash: u64,
    pub ts: i64,
    cid: Key,
    user: Key,
    pub camp: u32,
}
impl Ck {
    #[inline]
    pub fn cid(&self) -> &Key {
        &self.cid
    }
    #[inline]
    pub fn user(&self) -> &Key {
        &self.user
    }
}
impl Rec for Ck {
    #[inline]
    fn hash(&self) -> u64 {
        self.hash
    }
    #[inline]
    fn same_key(&self, o: &Self) -> bool {
        self.cid.same(&o.cid)
    }
    #[inline]
    fn rank(&self) -> i64 {
        0
    }
    #[inline]
    fn kill(&mut self) {
        self.camp = u32::MAX;
    }
    #[inline]
    fn alive(&self) -> bool {
        self.camp != u32::MAX
    }
}

/// Only valid clicks are kept: copies of a click are identical (rules.md), so dedup after
/// validation gives the same set as dedup before it.
pub fn parse_clicks(chunk: B, d: &Dims) -> Vec<Vec<Ck>> {
    let (lo, hi) = (d.window_start - WEEK_MS, d.window_end);
    let mut out: Vec<Vec<Ck>> = shard_buffers(chunk.len(), 60);
    let mut days = DayCache::default();
    for_each_line(chunk, |line| {
        let Some([cid, user, camp, tsr]) = fields::<4>(line) else { return };
        if cid.is_empty() || user.is_empty() {
            return;
        }
        let Some(&c) = d.camp.get(camp) else { return };
        let Some(ts) = days.parse(tsr) else { return };
        if ts < lo || ts >= hi {
            return;
        }
        let cid = Key::hot(cid, line);
        let user = Key::hot(user, line);
        let hash = cid.hash64();
        out[shard_of(hash)].push(Ck { hash, ts, cid, user, camp: c });
    });
    out
}

/// One valid link line: both IDs and their hashes.
#[derive(Clone, Copy)]
pub struct Link {
    pub a: Key,
    pub b: Key,
}

pub fn parse_links(chunk: B) -> Vec<Link> {
    let mut out = Vec::with_capacity(chunk.len() / 24 + 16);
    for_each_line(chunk, |line| {
        if let Some([a, b]) = fields::<2>(line) {
            if !a.is_empty() && !b.is_empty() {
                out.push(Link { a: Key::safe(a), b: Key::safe(b) });
            }
        }
    });
    out
}
