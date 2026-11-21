//! Row parsers for events, clicks and links. One pass over the bytes, no allocation per row.

use crate::csv::{fields, for_each_line, B};
use crate::dedup::Rec;
use crate::dims::Dims;
use crate::ts::{digits_i64, parse_ts, LATE_MS, WEEK_MS};
use crate::util::hash_bytes;

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

#[derive(Clone, Copy)]
pub struct Ev {
    pub hash: u64,
    pub ingest: i64,
    pub ts: i64,
    pub pay: u64,
    pub eid: B,
    pub user: B,
}
impl Rec for Ev {
    #[inline]
    fn hash(&self) -> u64 {
        self.hash
    }
    #[inline]
    fn key(&self) -> B {
        self.eid
    }
    #[inline]
    fn rank(&self) -> i64 {
        self.ingest
    }
}

/// A surviving event.
#[derive(Clone, Copy)]
pub struct EvOut {
    pub user: B,
    pub ts: i64,
    pub pay: u64,
}

pub fn parse_events(chunk: B, d: &Dims) -> Vec<Ev> {
    let (ws, we) = (d.window_start, d.window_end);
    let mut out = Vec::with_capacity(chunk.len() / 56 + 16);
    for_each_line(chunk, |line| {
        let Some([eid, user, tsr, ing, ty, prod, price]) = fields::<7>(line) else { return };
        if eid.is_empty() {
            return;
        }
        // ingest_ts is always valid F1; a bad one sorts last and fails the late check
        let ingest = parse_ts(ing).unwrap_or(i64::MAX);
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
            let Some(t) = parse_ts(tsr) else { break 'valid };
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
            let cat = d.prod.get(prod).copied().unwrap_or(d.unknown) as u64;
            ts = t;
            pay = p | (kind << 24) | (cat << 32);
        }
        out.push(Ev { hash: hash_bytes(eid), ingest, ts, pay, eid, user });
    });
    out
}

#[derive(Clone, Copy)]
pub struct Ck {
    pub hash: u64,
    pub cid: B,
    pub user: B,
    pub camp: u32,
    pub ts: i64,
}
impl Rec for Ck {
    #[inline]
    fn hash(&self) -> u64 {
        self.hash
    }
    #[inline]
    fn key(&self) -> B {
        self.cid
    }
    #[inline]
    fn rank(&self) -> i64 {
        0
    }
}

/// Only valid clicks are kept: copies of a click are identical (rules.md), so dedup after
/// validation gives the same set as dedup before it.
pub fn parse_clicks(chunk: B, d: &Dims) -> Vec<Ck> {
    let (lo, hi) = (d.window_start - WEEK_MS, d.window_end);
    let mut out = Vec::with_capacity(chunk.len() / 40 + 16);
    for_each_line(chunk, |line| {
        let Some([cid, user, camp, tsr]) = fields::<4>(line) else { return };
        if cid.is_empty() || user.is_empty() {
            return;
        }
        let Some(&c) = d.camp.get(camp) else { return };
        let Some(ts) = parse_ts(tsr) else { return };
        if ts < lo || ts >= hi {
            return;
        }
        out.push(Ck { hash: hash_bytes(cid), cid, user, camp: c, ts });
    });
    out
}

pub fn parse_links(chunk: B) -> Vec<(B, B)> {
    let mut out = Vec::with_capacity(chunk.len() / 24 + 16);
    for_each_line(chunk, |line| {
        if let Some([a, b]) = fields::<2>(line) {
            if !a.is_empty() && !b.is_empty() {
                out.push((a, b));
            }
        }
    });
    out
}
