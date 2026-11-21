//! Credit and assists, per person, without any session x click join.
//!
//! Credit: sessions and clicks are both sorted by time, so two pointers find the latest click
//! at or before each session start; ties on ts go to the smallest click_id.
//! Assists: per (person, campaign) the clicks' intervals [ts, ts + 7 d] are merged; a session is
//! assisted by the campaign iff its start falls inside a merged interval. Each merged interval
//! counts its sessions with two binary searches and a walk over its days, so whale and
//! shared-device people cost the same as everyone else.

use crate::parse::Ck;
use crate::sessions::Sess;
use crate::ts::WEEK_MS;
use crate::util::SyncPtr;
use rayon::prelude::*;

pub struct Acc {
    /// (campaign + organic) x day
    pub cred_sessions: Vec<u64>,
    pub cred_purchasing: Vec<u64>,
    pub cred_revenue: Vec<u64>,
    /// campaign x day
    pub assisted: Vec<u64>,
}

impl Acc {
    fn new(nc: usize, nd: usize) -> Acc {
        Acc {
            cred_sessions: vec![0; (nc + 1) * nd],
            cred_purchasing: vec![0; (nc + 1) * nd],
            cred_revenue: vec![0; (nc + 1) * nd],
            assisted: vec![0; nc * nd],
        }
    }
    fn add(mut self, o: Acc) -> Acc {
        for (a, b) in self.cred_sessions.iter_mut().zip(&o.cred_sessions) {
            *a += b;
        }
        for (a, b) in self.cred_purchasing.iter_mut().zip(&o.cred_purchasing) {
            *a += b;
        }
        for (a, b) in self.cred_revenue.iter_mut().zip(&o.cred_revenue) {
            *a += b;
        }
        for (a, b) in self.assisted.iter_mut().zip(&o.assisted) {
            *a += b;
        }
        self
    }
}

/// `clicks_sorted` is (person << bits | ts - base, index into `clicks`), sorted by the key.
/// `credit_out[i]` receives the campaign index credited to `sessions[i]` (`nc` = organic).
#[allow(clippy::too_many_arguments)]
pub fn credit_and_assists(
    sessions: &[Sess],
    clicks: &[Ck],
    clicks_sorted: &[(u64, u32)],
    bits: u32,
    base: i64,
    nc: usize,
    nd: usize,
    credit_out: &mut [u32],
) -> Acc {
    let n = sessions.len();
    let parts = (rayon::current_num_threads() * 8).max(1);
    let mut cuts = vec![0usize];
    for p in 1..parts {
        let mut i = (n * p / parts).max(*cuts.last().unwrap());
        if i > 0 && i < n {
            let prev = sessions[i - 1].person;
            while i < n && sessions[i].person == prev {
                i += 1;
            }
        }
        cuts.push(i);
    }
    cuts.push(n);
    let out = SyncPtr(credit_out.as_mut_ptr());
    let mask = if bits == 0 { 0 } else { (1u64 << bits) - 1 };
    (0..parts)
        .into_par_iter()
        .map(|p| {
            let mut acc = Acc::new(nc, nd);
            let (a, b) = (cuts[p], cuts[p + 1]);
            if a == b {
                return acc;
            }
            let first = sessions[a].person as u64;
            let mut cp = clicks_sorted.partition_point(|x| (x.0 >> bits) < first);
            let mut reduced: Vec<(i64, u32)> = Vec::new();
            let mut by_camp: Vec<(u32, i64)> = Vec::new();
            let mut i = a;
            while i < b {
                let person = sessions[i].person;
                let mut j = i + 1;
                while j < b && sessions[j].person == person {
                    j += 1;
                }
                let ss = &sessions[i..j];
                while cp < clicks_sorted.len() && ((clicks_sorted[cp].0 >> bits) as u32) < person {
                    cp += 1;
                }
                let mut ce = cp;
                while ce < clicks_sorted.len() && (clicks_sorted[ce].0 >> bits) as u32 == person {
                    ce += 1;
                }
                let cs = &clicks_sorted[cp..ce];
                reduced.clear();
                by_camp.clear();
                let mut k = 0;
                while k < cs.len() {
                    let key = cs[k].0;
                    let mut best = cs[k].1 as usize;
                    let mut m = k + 1;
                    while m < cs.len() && cs[m].0 == key {
                        let c = cs[m].1 as usize;
                        if clicks[c].cid().bytes() < clicks[best].cid().bytes() {
                            best = c;
                        }
                        m += 1;
                    }
                    let ts = base + (key & mask) as i64;
                    reduced.push((ts, clicks[best].camp));
                    for x in &cs[k..m] {
                        by_camp.push((clicks[x.1 as usize].camp, ts));
                    }
                    k = m;
                }
                // credit
                let mut r = 0;
                for (o, s) in ss.iter().enumerate() {
                    while r < reduced.len() && reduced[r].0 <= s.start {
                        r += 1;
                    }
                    let camp = if r > 0 && s.start - reduced[r - 1].0 <= WEEK_MS { reduced[r - 1].1 as usize } else { nc };
                    unsafe { out.write(i + o, camp as u32) };
                    let cell = camp * nd + s.day as usize;
                    acc.cred_sessions[cell] += 1;
                    if s.purchases > 0 {
                        acc.cred_purchasing[cell] += 1;
                    }
                    acc.cred_revenue[cell] += s.revenue;
                }
                // assists
                by_camp.sort_unstable();
                let mut q = 0;
                while q < by_camp.len() {
                    let camp = by_camp[q].0;
                    let (mut l, mut rr) = (by_camp[q].1, by_camp[q].1 + WEEK_MS);
                    q += 1;
                    loop {
                        let next = if q < by_camp.len() && by_camp[q].0 == camp { Some(by_camp[q].1) } else { None };
                        match next {
                            Some(t) if t <= rr => {
                                rr = t + WEEK_MS;
                                q += 1;
                            }
                            _ => {
                                count_interval(ss, l, rr, camp as usize, nd, &mut acc.assisted);
                                match next {
                                    Some(t) => {
                                        l = t;
                                        rr = t + WEEK_MS;
                                        q += 1;
                                    }
                                    None => break,
                                }
                            }
                        }
                    }
                }
                i = j;
            }
            acc
        })
        .reduce(|| Acc::new(nc, nd), Acc::add)
}

/// Sessions of one person (sorted by start) with start in [l, r], counted per day.
#[inline]
fn count_interval(ss: &[Sess], l: i64, r: i64, camp: usize, nd: usize, assisted: &mut [u64]) {
    let lo = ss.partition_point(|s| s.start < l);
    let hi = ss.partition_point(|s| s.start <= r);
    let mut i = lo;
    while i < hi {
        let d = ss[i].day;
        let e = i + ss[i..hi].partition_point(|s| s.day <= d);
        assisted[camp * nd + d as usize] += (e - i) as u64;
        i = e;
    }
}
