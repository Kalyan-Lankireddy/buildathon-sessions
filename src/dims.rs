//! Small inputs: manifest.json, products.csv, campaigns.csv.

use crate::csv::{self, for_each_line, fields, B};
use crate::ts::parse_ts;
use rustc_hash::FxHashMap;

pub struct Dims {
    pub window_start: i64,
    pub window_end: i64,
    /// category names in byte order, `unknown` included; a category id is its index here
    pub cat_names: Vec<B>,
    pub unknown: u32,
    pub prod: FxHashMap<B, u32>,
    pub camp_ids: Vec<B>,
    pub camp_channel: Vec<B>,
    pub camp: FxHashMap<B, u32>,
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
    let mut prod = FxHashMap::default();
    for r in &products {
        if !r[0].is_empty() {
            prod.insert(r[0], names.binary_search(&r[1]).unwrap() as u32);
        }
    }

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
