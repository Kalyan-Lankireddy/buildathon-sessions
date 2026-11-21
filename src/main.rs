//! Shopping Sessions: native pipeline. rules.md is the source of truth; reference/ is the oracle.
//!
//! One pass over mmapped bytes, integers everywhere:
//!   read+parse -> dedup -> links -> sessions -> credit+assists -> aggregate -> write
//! Per-stage wall times go to stderr.

mod credit;
mod csv;
mod dedup;
mod dims;
mod intern;
mod parse;
mod pq;
mod sessions;
mod ts;
mod util;

use bytes::Bytes;
use csv::B;
use parquet::data_type::ByteArray;
use pq::Column;
use rayon::prelude::*;
use std::time::Instant;
use ts::{MS_DAY, WEEK_MS};
use util::{bits_for, concat_par, SyncPtr};

const CHUNK: usize = 64 << 20;

fn ba(b: B) -> ByteArray {
    ByteArray::from(Bytes::from_static(b))
}

struct Lap(Instant, Instant, bool);
impl Lap {
    fn lap(&mut self, name: &str) {
        let now = Instant::now();
        if self.2 {
            eprintln!("[{:7.3}s] {:<26} {:7.3}s  {}", (now - self.0).as_secs_f64(), name, (now - self.1).as_secs_f64(), mem_note());
        }
        self.1 = now;
    }
}

/// Linux only: anonymous vs file-backed resident memory, and the peak so far (MB).
fn mem_note() -> String {
    let Ok(s) = std::fs::read_to_string("/proc/self/status") else { return String::new() };
    let get = |k: &str| -> u64 {
        s.lines().find(|l| l.starts_with(k)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0) / 1024
    };
    format!("anon {} MB, file {} MB, peak total {} MB", get("RssAnon:"), get("RssFile:"), get("VmHWM:"))
}

/// Give freed heap back to the OS (glibc keeps it in per-thread arenas otherwise).
fn trim() {
    #[cfg(target_os = "linux")]
    if std::env::var_os("NOTRIM").is_none() {
        extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        unsafe { malloc_trim(0) };
    }
}

/// user and system CPU seconds of this process (Linux).
fn cpu_note() -> String {
    let Ok(s) = std::fs::read_to_string("/proc/self/stat") else { return String::new() };
    let after = s.rsplit(')').next().unwrap_or("");
    let f: Vec<&str> = after.split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) / 100.0;
    format!("user {:.2}s sys {:.2}s", t(11), t(12))
}

fn find(p: &mut [u32], mut x: u32) -> u32 {
    while p[x as usize] != x {
        let g = p[p[x as usize] as usize];
        p[x as usize] = g;
        x = g;
    }
    x
}

fn main() {
    let data = std::env::var("DATA_DIR").unwrap_or_else(|_| "/data".into());
    let out = std::env::var("OUTPUT_DIR").unwrap_or_else(|_| "/output".into());
    let dump = std::env::var("DUMP_DIR").ok();
    let threads = std::env::var("NCPU").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8)
    });
    rayon::ThreadPoolBuilder::new().num_threads(threads).build_global().unwrap();
    let t0 = Instant::now();
    let mut lap = Lap(t0, t0, std::env::var("QUIET").is_err());

    // ---- read + parse
    let d = dims::load(&data);
    let (ws, we) = (d.window_start, d.window_end);
    let ev_chunks = csv::load_chunks(&csv::list_csv(&format!("{data}/events")), CHUNK);
    let ck_chunks = csv::load_chunks(&csv::list_csv(&format!("{data}/clicks")), CHUNK);
    let lk_chunks = csv::load_chunks(&csv::list_csv(&format!("{data}/links")), CHUNK);
    lap.lap("  map files");
    let evs: Vec<Vec<Vec<parse::Ev>>> = ev_chunks.par_iter().map(|c| parse::parse_events(c, &d)).collect();
    lap.lap("  parse events");
    let cks: Vec<Vec<Vec<parse::Ck>>> = ck_chunks.par_iter().map(|c| parse::parse_clicks(c, &d)).collect();
    let lks: Vec<Vec<parse::Link>> = lk_chunks.par_iter().map(|c| parse::parse_links(c)).collect();
    lap.lap("  parse clicks+links");

    // ---- dedup (events: smallest ingest_ts wins, then validity of the kept copy decides)
    let ev_parts: Vec<Vec<parse::EvOut>> = dedup::dedup(&evs, |e| {
        (e.ts != parse::INVALID).then(|| e.out())
    });
    let ck_parts: Vec<Vec<parse::Ck>> = dedup::dedup(&cks, |c| Some(*c));
    drop(evs);
    drop(cks);
    trim();
    lap.lap("dedup");

    // ---- links: intern every user string, union-find, name each person
    let nl: usize = lks.iter().map(|v| v.len()).sum();
    let ev_off: Vec<usize> = prefix(ev_parts.iter().map(|p| p.len()));
    let ck_off: Vec<usize> = prefix(ck_parts.iter().map(|p| p.len()));
    let n_ev = *ev_off.last().unwrap();
    let n_ck = *ck_off.last().unwrap();
    let total = 2 * nl + n_ev + n_ck;
    let mut keys: Vec<B> = Vec::with_capacity(total);
    let mut khash: Vec<u32> = Vec::with_capacity(total);
    {
        let kp = SyncPtr(keys.as_mut_ptr());
        let hp = SyncPtr(khash.as_mut_ptr());
        let lk_off = prefix(lks.iter().map(|p| p.len() * 2));
        lks.par_iter().enumerate().for_each(|(i, v)| {
            for (j, l) in v.iter().enumerate() {
                unsafe {
                    kp.write(lk_off[i] + 2 * j, l.a);
                    kp.write(lk_off[i] + 2 * j + 1, l.b);
                    hp.write(lk_off[i] + 2 * j, l.ha);
                    hp.write(lk_off[i] + 2 * j + 1, l.hb);
                }
            }
        });
        ev_parts.par_iter().enumerate().for_each(|(i, v)| {
            for (j, e) in v.iter().enumerate() {
                unsafe {
                    kp.write(2 * nl + ev_off[i] + j, e.user());
                    hp.write(2 * nl + ev_off[i] + j, e.uhash);
                }
            }
        });
        ck_parts.par_iter().enumerate().for_each(|(i, v)| {
            for (j, c) in v.iter().enumerate() {
                unsafe {
                    kp.write(2 * nl + n_ev + ck_off[i] + j, c.user());
                    hp.write(2 * nl + n_ev + ck_off[i] + j, c.uhash);
                }
            }
        });
        unsafe {
            keys.set_len(total);
            khash.set_len(total);
        }
    }
    lap.lap("  gather keys");
    let mut it = intern::intern(&keys, &khash);
    lap.lap("  intern");
    drop(keys);
    drop(khash);
    trim();
    let nid = it.names.len();
    let mut parent: Vec<u32> = (0..nid as u32).collect();
    let mut size = vec![1u32; nid];
    for k in 0..nl {
        let (a, b) = (find(&mut parent, it.ids[2 * k]), find(&mut parent, it.ids[2 * k + 1]));
        if a != b {
            let (big, small) = if size[a as usize] >= size[b as usize] { (a, b) } else { (b, a) };
            parent[small as usize] = big;
            size[big as usize] += size[small as usize];
        }
    }
    lap.lap("  union-find");
    let root: Vec<u32> = (0..nid as u32).map(|x| find(&mut parent, x)).collect();
    // a person is named by the byte-wise smallest of their IDs
    let mut name_of: Vec<u32> = vec![u32::MAX; nid];
    for i in 0..nid {
        let r = root[i] as usize;
        if name_of[r] == u32::MAX || it.names[i] < it.names[name_of[r] as usize] {
            name_of[r] = i as u32;
        }
    }
    lap.lap("links");

    // ---- sessions
    let span = (we - ws).max(1) as u64;
    let ts_bits = bits_for(span);
    let pbits = bits_for(nid as u64);
    assert!(ts_bits + pbits <= 64, "key does not fit in 64 bits");
    let mut arr: Vec<(u64, u64)> = Vec::with_capacity(n_ev);
    {
        let ap = SyncPtr(arr.as_mut_ptr());
        ev_parts.par_iter().enumerate().for_each(|(i, v)| {
            for (j, e) in v.iter().enumerate() {
                let p = root[it.ids[2 * nl + ev_off[i] + j] as usize] as u64;
                unsafe { ap.write(ev_off[i] + j, ((p << ts_bits) | (e.ts - ws) as u64, e.pay)) };
            }
        });
        unsafe { arr.set_len(n_ev) };
    }
    drop(ev_parts);
    trim();
    arr.par_sort_unstable_by_key(|x| x.0);
    let scan = sessions::scan(&arr, ts_bits, ws);
    let arr = if dump.is_some() { arr } else { Vec::new() };
    trim();
    let sess: Vec<sessions::Sess> = concat_par(&scan.sessions);
    let days: Vec<sessions::UserDay> = concat_par(&scan.days);
    drop(scan);
    lap.lap("sessions");

    // ---- credit + assists
    let cbase = ws - WEEK_MS;
    let cbits = bits_for((we - cbase) as u64);
    assert!(cbits + pbits <= 64, "click key does not fit in 64 bits");
    let clicks: Vec<parse::Ck> = concat_par(&ck_parts);
    drop(ck_parts);
    let mut csorted: Vec<(u64, u32)> = Vec::with_capacity(n_ck);
    {
        let cp = SyncPtr(csorted.as_mut_ptr());
        clicks.par_iter().enumerate().for_each(|(i, c)| {
            let p = root[it.ids[2 * nl + n_ev + i] as usize] as u64;
            unsafe { cp.write(i, ((p << cbits) | (c.ts - cbase) as u64, i as u32)) };
        });
        unsafe { csorted.set_len(n_ck) };
    }
    csorted.par_sort_unstable_by_key(|x| x.0);
    let link_ids: Vec<u32> = if dump.is_some() { it.ids[..2 * nl].to_vec() } else { Vec::new() };
    it.ids = Vec::new();
    let nc = d.camp_ids.len();
    let day0 = ws.div_euclid(MS_DAY);
    let nd = ((we - 1).div_euclid(MS_DAY) - day0 + 1).max(1) as usize;
    assert!((nc + 1) * nd < 200_000_000, "campaign x day table too large");
    let mut credited = vec![0u32; sess.len()];
    let acc = credit::credit_and_assists(&sess, &clicks, &csorted, cbits, cbase, nc, nd, &mut credited);
    drop(csorted);
    drop(clicks);
    trim();
    lap.lap("credit+assists");

    // ---- aggregate: columns for both tables
    let names = &it.names;
    let pname = |person: u32| -> B { names[name_of[person as usize] as usize] };
    let u_user: Vec<ByteArray> = days.par_iter().map(|r| ba(pname(r.person))).collect();
    let u_day: Vec<i32> = days.par_iter().map(|r| (day0 + r.day as i64) as i32).collect();
    let u_sess: Vec<i64> = days.par_iter().map(|r| r.sessions as i64).collect();
    let u_ev: Vec<i64> = days.par_iter().map(|r| r.events as i64).collect();
    let u_rev: Vec<i64> = days.par_iter().map(|r| r.revenue as i64).collect();
    let u_cat: Vec<Option<ByteArray>> =
        days.par_iter().map(|r| (r.top_cat != u32::MAX).then(|| ba(d.cat_names[r.top_cat as usize]))).collect();
    let mut c_rows: Vec<(u32, u32)> = vec![];
    for c in 0..=nc {
        for dd in 0..nd {
            if acc.cred_sessions[c * nd + dd] > 0 || (c < nc && acc.assisted[c * nd + dd] > 0) {
                c_rows.push((c as u32, dd as u32));
            }
        }
    }
    let organic: B = b"organic";
    let camp_name = |c: u32| -> B { if c as usize == nc { organic } else { d.camp_ids[c as usize] } };
    let camp_chan = |c: u32| -> B { if c as usize == nc { organic } else { d.camp_channel[c as usize] } };
    let c_id: Vec<ByteArray> = c_rows.iter().map(|r| ba(camp_name(r.0))).collect();
    let c_ch: Vec<ByteArray> = c_rows.iter().map(|r| ba(camp_chan(r.0))).collect();
    let c_day: Vec<i32> = c_rows.iter().map(|r| (day0 + r.1 as i64) as i32).collect();
    let cell = |r: &(u32, u32)| r.0 as usize * nd + r.1 as usize;
    let c_s: Vec<i64> = c_rows.iter().map(|r| acc.cred_sessions[cell(r)] as i64).collect();
    let c_p: Vec<i64> = c_rows.iter().map(|r| acc.cred_purchasing[cell(r)] as i64).collect();
    let c_r: Vec<i64> = c_rows.iter().map(|r| acc.cred_revenue[cell(r)] as i64).collect();
    let c_a: Vec<i64> =
        c_rows.iter().map(|r| if (r.0 as usize) < nc { acc.assisted[cell(r)] as i64 } else { 0 }).collect();
    lap.lap("aggregate");

    // ---- write
    let (su, sc) = (format!("{out}/user_daily"), format!("{out}/campaign_daily"));
    std::fs::create_dir_all(&su).expect("create user_daily");
    std::fs::create_dir_all(&sc).expect("create campaign_daily");
    let (r1, r2) = rayon::join(
        || {
            pq::write_table(
                &format!("{su}/part-0.parquet"),
                "message user_daily { required binary user_id (STRING); required int32 day (DATE); \
                 required int64 sessions; required int64 events; required int64 revenue_cents; \
                 optional binary top_category (STRING); }",
                &[Column::Str(u_user), Column::Date(u_day), Column::Int64(u_sess), Column::Int64(u_ev),
                  Column::Int64(u_rev), Column::OptStr(u_cat)],
            )
        },
        || {
            pq::write_table(
                &format!("{sc}/part-0.parquet"),
                "message campaign_daily { required binary campaign_id (STRING); required binary channel (STRING); \
                 required int32 day (DATE); required int64 sessions; required int64 purchasing_sessions; \
                 required int64 revenue_cents; required int64 assisted_sessions; }",
                &[Column::Str(c_id), Column::Str(c_ch), Column::Date(c_day), Column::Int64(c_s), Column::Int64(c_p),
                  Column::Int64(c_r), Column::Int64(c_a)],
            )
        },
    );
    r1.expect("write user_daily");
    r2.expect("write campaign_daily");
    lap.lap("write");

    if let Some(dir) = dump {
        std::fs::create_dir_all(&dir).unwrap();
        dump_stages(&dir, &d, &it, &link_ids, &root, &name_of, &arr, ts_bits, ws, &sess, &credited);
    }
    if lap.2 {
        eprintln!("cpu {}", cpu_note());
        eprintln!("total {:.3}s  events={} sessions={} clicks={} ids={}", t0.elapsed().as_secs_f64(), n_ev, sess.len(), n_ck, nid);
    }
}

fn prefix(it: impl Iterator<Item = usize>) -> Vec<usize> {
    let mut v = vec![0];
    let mut run = 0;
    for x in it {
        run += x;
        v.push(run);
    }
    v
}

/// Intermediate tables for scripts/stage_check.py (only when DUMP_DIR is set).
#[allow(clippy::too_many_arguments)]
fn dump_stages(
    dir: &str, d: &dims::Dims, it: &intern::Interned, link_ids: &[u32], root: &[u32], name_of: &[u32],
    arr: &[(u64, u64)], ts_bits: u32, ws: i64, sess: &[sessions::Sess], credited: &[u32],
) {
    let pname = |p: u32| -> B { it.names[name_of[p as usize] as usize] };
    let mask = if ts_bits == 0 { 0 } else { (1u64 << ts_bits) - 1 };
    // persons: every ID that appears in a valid link line, with its person name
    let mut seen: Vec<u32> = link_ids.to_vec();
    seen.sort_unstable();
    seen.dedup();
    let w = |name: &str, schema: &str, cols: Vec<Column>| {
        pq::write_table(&format!("{dir}/{name}.parquet"), schema, &cols).unwrap();
    };
    w(
        "persons",
        "message t { required binary id (STRING); required binary person (STRING); }",
        vec![
            Column::Str(seen.iter().map(|&i| ba(it.names[i as usize])).collect()),
            Column::Str(seen.iter().map(|&i| ba(pname(root[i as usize]))).collect()),
        ],
    );
    w(
        "events",
        "message t { required binary person (STRING); required int64 ts; required int64 kind; \
         required binary category (STRING); required int64 price; }",
        vec![
            Column::Str(arr.iter().map(|x| ba(pname((x.0 >> ts_bits) as u32))).collect()),
            Column::Int64(arr.iter().map(|x| ws + (x.0 & mask) as i64).collect()),
            Column::Int64(arr.iter().map(|x| parse::pay_kind(x.1) as i64).collect()),
            Column::Str(arr.iter().map(|x| ba(d.cat_names[parse::pay_cat(x.1) as usize])).collect()),
            Column::Int64(arr.iter().map(|x| parse::pay_price(x.1) as i64).collect()),
        ],
    );
    let nc = d.camp_ids.len();
    let day0 = ws.div_euclid(MS_DAY);
    w(
        "sessions",
        "message t { required binary person (STRING); required int64 start; required int32 day (DATE); \
         required int64 events; required int64 purchases; required int64 revenue; required binary campaign (STRING); }",
        vec![
            Column::Str(sess.iter().map(|s| ba(pname(s.person))).collect()),
            Column::Int64(sess.iter().map(|s| s.start).collect()),
            Column::Date(sess.iter().map(|s| (day0 + s.day as i64) as i32).collect()),
            Column::Int64(sess.iter().map(|s| s.events as i64).collect()),
            Column::Int64(sess.iter().map(|s| s.purchases as i64).collect()),
            Column::Int64(sess.iter().map(|s| s.revenue as i64).collect()),
            Column::Str(credited.iter().map(|&c| if c as usize == nc { ba(b"organic") } else { ba(d.camp_ids[c as usize]) }).collect()),
        ],
    );
}
