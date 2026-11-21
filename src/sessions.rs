//! Sessions from events sorted by (person, ts), and the user_daily rows built on the way.

use crate::parse::{pay_cat, pay_kind, pay_price, KIND_PURCHASE};
use crate::ts::{MS_DAY, SESSION_GAP_MS};
use rayon::prelude::*;

#[derive(Clone, Copy)]
pub struct Sess {
    pub start: i64,
    pub person: u32,
    /// day index relative to the window's first day
    pub day: u32,
    pub events: u32,
    pub purchases: u32,
    pub revenue: u64,
}

#[derive(Clone, Copy)]
pub struct UserDay {
    pub person: u32,
    pub day: u32,
    pub sessions: u32,
    pub events: u32,
    pub revenue: u64,
    /// category id, or u32::MAX for none
    pub top_cat: u32,
}

pub struct Scan {
    pub sessions: Vec<Vec<Sess>>,
    pub days: Vec<Vec<UserDay>>,
}

struct Group {
    day: u32,
    sessions: u32,
    events: u32,
    revenue: u64,
    cats: Vec<(u32, u64)>,
}

fn flush(g: &Group, person: u32, out: &mut Vec<UserDay>) {
    // highest total; a tie goes to the smallest category id (ids follow byte order)
    let mut best: Option<(u32, u64)> = None;
    for &(c, t) in &g.cats {
        best = match best {
            Some((bc, bt)) if bt > t || (bt == t && bc < c) => Some((bc, bt)),
            _ => Some((c, t)),
        };
    }
    out.push(UserDay {
        person,
        day: g.day,
        sessions: g.sessions,
        events: g.events,
        revenue: g.revenue,
        top_cat: best.map_or(u32::MAX, |b| b.0),
    });
}

/// `arr` holds (person << ts_bits | ts - ws, pay), sorted by the first field.
pub fn scan(arr: &[(u64, u64)], ts_bits: u32, ws: i64) -> Scan {
    let n = arr.len();
    let mask = if ts_bits == 0 { 0 } else { (1u64 << ts_bits) - 1 };
    let day0 = ws.div_euclid(MS_DAY);
    // cut at person boundaries so every range holds whole people
    let parts = (rayon::current_num_threads() * 8).max(1);
    let mut cuts = vec![0usize];
    for p in 1..parts {
        let mut i = (n * p / parts).max(*cuts.last().unwrap());
        if i > 0 && i < n {
            let prev = arr[i - 1].0 >> ts_bits;
            while i < n && arr[i].0 >> ts_bits == prev {
                i += 1;
            }
        }
        cuts.push(i);
    }
    cuts.push(n);
    let res: Vec<(Vec<Sess>, Vec<UserDay>)> = (0..parts)
        .into_par_iter()
        .map(|p| scan_range(&arr[cuts[p]..cuts[p + 1]], ts_bits, mask, ws, day0))
        .collect();
    let (sessions, days) = res.into_iter().unzip();
    Scan { sessions, days }
}

fn scan_range(arr: &[(u64, u64)], ts_bits: u32, mask: u64, ws: i64, day0: i64) -> (Vec<Sess>, Vec<UserDay>) {
    let mut sessions: Vec<Sess> = Vec::with_capacity(arr.len() / 4 + 8);
    let mut days: Vec<UserDay> = Vec::with_capacity(arr.len() / 6 + 8);
    let mut i = 0;
    while i < arr.len() {
        let person = (arr[i].0 >> ts_bits) as u32;
        let mut j = i + 1;
        while j < arr.len() && (arr[j].0 >> ts_bits) as u32 == person {
            j += 1;
        }
        let mut group: Option<Group> = None;
        let mut start = 0i64;
        let (mut ev, mut pu, mut rev) = (0u32, 0u32, 0u64);
        let mut sday = 0u32;
        let mut prev = 0i64;
        for (k, &(key, pay)) in arr[i..j].iter().enumerate() {
            let ts = ws + (key & mask) as i64;
            if k == 0 || ts - prev > SESSION_GAP_MS {
                if k > 0 {
                    sessions.push(Sess { start, person, day: sday, events: ev, purchases: pu, revenue: rev });
                }
                start = ts;
                ev = 0;
                pu = 0;
                rev = 0;
                sday = (ts.div_euclid(MS_DAY) - day0) as u32;
                match &mut group {
                    Some(g) if g.day == sday => g.sessions += 1,
                    _ => {
                        if let Some(g) = &group {
                            flush(g, person, &mut days);
                        }
                        group = Some(Group { day: sday, sessions: 1, events: 0, revenue: 0, cats: Vec::new() });
                    }
                }
            }
            prev = ts;
            ev += 1;
            let g = group.as_mut().unwrap();
            g.events += 1;
            if pay_kind(pay) == KIND_PURCHASE {
                let price = pay_price(pay);
                pu += 1;
                rev += price;
                g.revenue += price;
                let c = pay_cat(pay);
                match g.cats.iter_mut().find(|x| x.0 == c) {
                    Some(x) => x.1 += price,
                    None => g.cats.push((c, price)),
                }
            }
        }
        sessions.push(Sess { start, person, day: sday, events: ev, purchases: pu, revenue: rev });
        if let Some(g) = &group {
            flush(g, person, &mut days);
        }
        i = j;
    }
    (sessions, days)
}
