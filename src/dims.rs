//! Small inputs: manifest.json, products.csv, campaigns.csv.

use crate::csv::{self, for_each_line, fields, B};
use crate::key::Key;
use crate::ts::parse_ts;
use rustc_hash::FxHashMap;

pub struct Dims {
    pub window_start: i64,
    pub window_end: i64,
    /// category names in byte order, `unknown` included; a category id is its index here
    pub cat_names: Vec<B>,
    pub unknown: u32,
    pub prod: ProductTable,
    pub camp_ids: Vec<B>,
    pub camp_channel: Vec<B>,
    pub camp: FxHashMap<B, u32>,
}

/// product_id -> category id. Open addressing over inline keys; exact (keys compare in full).
pub struct ProductTable {
    keys: Vec<Key>,
    vals: Vec<u32>,
    mask: usize,
}

impl ProductTable {
    fn new(entries: &[(B, u32)]) -> ProductTable {
        let cap = (entries.len() * 2).next_power_of_two().max(16);
        let mut t = ProductTable { keys: vec![Key::safe(b"")], vals: vec![u32::MAX; cap], mask: cap - 1 };
        t.keys = vec![Key::safe(b""); cap];
        for &(b, v) in entries {
            let k = Key::safe(b);
            let mut p = k.hash64() as usize & t.mask;
            loop {
                if t.vals[p] == u32::MAX {
                    t.keys[p] = k;
                    t.vals[p] = v;
                    break;
                }
                if t.keys[p].same(&k) {
                    t.vals[p] = v; // a repeated product_id: the last row wins
                    break;
                }
                p = (p + 1) & t.mask;
            }
        }
        t
    }

    #[inline]
    pub fn get(&self, k: &Key) -> Option<u32> {
        let mut p = k.hash64() as usize & self.mask;
        loop {
            let v = self.vals[p];
            if v == u32::MAX {
                return None;
            }
            if self.keys[p].same(k) {
                return Some(v);
            }
            p = (p + 1) & self.mask;
        }
    }
}

fn json_string(text: &str, key: &str) -> Option<String> {
    let at = text.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = &text[at..];
    let q1 = rest.find('"')? + 1;
    let q2 = rest[q1..].find('"')? + q1;
    Some(rest[q1..q2].to_string())
}

fn rows2(path: &str) -> Vec<[B; 2]> {
    let mut v = vec![];
    for c in csv::chunks(csv::map_file(path), usize::MAX) {
        for_each_line(c, |l| {
            if let Some(f) = fields::<2>(l) {
                v.push(f);
            }
        });
    }
    v
}

pub fn load(data: &str) -> Dims {
    let m = std::fs::read_to_string(format!("{data}/manifest.json")).expect("manifest.json");
    let ws = parse_ts(json_string(&m, "window_start").expect("window_start").as_bytes()).expect("bad window_start");
    let we = parse_ts(json_string(&m, "window_end").expect("window_end").as_bytes()).expect("bad window_end");

    let products = rows2(&format!("{data}/products.csv"));
    let mut names: Vec<B> = products.iter().map(|r| r[1]).collect();
    names.push(b"unknown");
    names.sort_unstable();
    names.dedup();
    let unknown = names.binary_search(&&b"unknown"[..]).unwrap() as u32;
    let entries: Vec<(B, u32)> = products
        .iter()
        .filter(|r| !r[0].is_empty())
        .map(|r| (r[0], names.binary_search(&r[1]).unwrap() as u32))
        .collect();
    let prod = ProductTable::new(&entries);

    let camps = rows2(&format!("{data}/campaigns.csv"));
    let mut camp_ids = vec![];
    let mut camp_channel = vec![];
    let mut camp = FxHashMap::default();
    for r in &camps {
        if !r[0].is_empty() && !camp.contains_key(r[0]) {
            camp.insert(r[0], camp_ids.len() as u32);
            camp_ids.push(r[0]);
            camp_channel.push(r[1]);
        }
    }
    Dims { window_start: ws, window_end: we, cat_names: names, unknown, prod, camp_ids, camp_channel, camp }
}
