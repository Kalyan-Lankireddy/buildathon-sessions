//! Files, chunks, lines and fields, all on bytes with no allocation per row.
//!
//! Dialect (rules.md): only `\n` ends a line, `\r` is data. A field is valid when it holds no
//! quote, or is wrapped in exactly one pair of quotes. The quotes are stripped. An empty field is
//! null (represented as an empty slice). A wrong field count or a bad quote drops the line.
//! The first line of every file is the header.

use memchr::memchr;
use memmap2::Mmap;
use std::fs::File;

/// Bytes that live for the whole run (mapped files are leaked on purpose).
pub type B = &'static [u8];

/// Every `*.csv` regular file directly in `dir`, sorted. A missing folder holds none.
pub fn list_csv(dir: &str) -> Vec<String> {
    let mut v: Vec<String> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".csv"))
            .filter(|e| std::fs::metadata(e.path()).map(|m| m.is_file()).unwrap_or(false))
            .map(|e| e.path().to_string_lossy().into_owned())
            .collect(),
        Err(_) => vec![],
    };
    v.sort();
    v
}

pub fn map_file(path: &str) -> B {
    let f = File::open(path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len == 0 {
        return &[];
    }
    let m = unsafe { Mmap::map(&f) }.unwrap_or_else(|e| panic!("mmap {path}: {e}"));
    #[cfg(unix)]
    let _ = m.advise(memmap2::Advice::Sequential);
    let m: &'static Mmap = Box::leak(Box::new(m));
    &m[..]
}

/// Split a file's body (after the header line) into pieces of about `target` bytes, each
/// ending on a `\n` (or at the end of the file).
pub fn chunks(file: B, target: usize) -> Vec<B> {
    let Some(h) = memchr(b'\n', file) else { return vec![] };
    let mut rest = &file[h + 1..];
    let mut out = vec![];
    while !rest.is_empty() {
        if rest.len() <= target {
            out.push(rest);
            break;
        }
        let cut = match memchr(b'\n', &rest[target..]) {
            Some(p) => target + p + 1,
            None => rest.len(),
        };
        out.push(&rest[..cut]);
        rest = &rest[cut..];
    }
    out
}

/// First `\n` at or after `from`, by 64 KB preads. `None` if there is none.
fn find_newline(f: &File, len: u64, from: u64) -> Option<u64> {
    use std::os::unix::fs::FileExt;
    let mut buf = vec![0u8; 1 << 16];
    let mut p = from;
    while p < len {
        let n = buf.len().min((len - p) as usize);
        f.read_exact_at(&mut buf[..n], p).expect("read");
        if let Some(i) = memchr(b'\n', &buf[..n]) {
            return Some(p + i as u64);
        }
        p += n as u64;
    }
    None
}

/// Start of the first line that begins at or after `pos` (lines begin at `body` and after `\n`).
fn line_start_at_or_after(f: &File, len: u64, body: u64, pos: u64) -> u64 {
    if pos <= body {
        return body;
    }
    match find_newline(f, len, pos - 1) {
        Some(i) => i + 1,
        None => len,
    }
}

/// Read every file in pieces of about `target` bytes, each ending on a line boundary, and hand
/// each piece to `f` as soon as it is read, while its bytes are still in cache. The header line
/// of every file is skipped. Pieces are read with pread() into their own 2 MB-aligned buffers
/// (no mmap, so no per-page faults on the input) and the buffers live for the whole run. The
/// results come back in file order, then piece order.
pub fn read_map<R: Send>(files: &[String], target: usize, f: impl Fn(B) -> R + Sync) -> Vec<R> {
    use rayon::prelude::*;
    use std::os::unix::fs::FileExt;
    use std::sync::Arc;
    struct Job {
        file: Arc<File>,
        a: u64,
        b: u64,
    }
    let per_file: Vec<Vec<Job>> = files
        .par_iter()
        .map(|path| {
            let file = Arc::new(File::open(path).unwrap_or_else(|e| panic!("open {path}: {e}")));
            let len = file.metadata().map(|m| m.len()).unwrap_or(0);
            let Some(h) = find_newline(&file, len, 0) else { return vec![] };
            let body = h + 1;
            if body >= len {
                return vec![];
            }
            let n = ((len - body) as usize).div_ceil(target).max(1);
            let mut starts: Vec<u64> =
                (0..n).map(|k| line_start_at_or_after(&file, len, body, body + (k * target) as u64)).collect();
            starts.push(len);
            (0..n)
                .filter(|&k| starts[k] < starts[k + 1])
                .map(|k| Job { file: file.clone(), a: starts[k], b: starts[k + 1] })
                .collect()
        })
        .collect();
    let jobs: Vec<Job> = per_file.into_iter().flatten().collect();
    jobs.par_iter()
        .map(|j| {
            let n = (j.b - j.a) as usize;
            let mut buf: Vec<u8> = Vec::with_capacity(n);
            // SAFETY: the bytes are written by read_exact_at before anything reads them.
            let dst = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr(), n) };
            j.file.read_exact_at(dst, j.a).expect("read input");
            unsafe { buf.set_len(n) };
            let leaked: B = Box::leak(buf.into_boxed_slice());
            f(leaked)
        })
        .collect()
}

#[inline]
pub fn for_each_line(chunk: B, mut f: impl FnMut(B)) {
    let mut rest = chunk;
    while !rest.is_empty() {
        match memchr(b'\n', rest) {
            Some(p) => {
                f(&rest[..p]);
                rest = &rest[p + 1..];
            }
            None => {
                f(rest);
                break;
            }
        }
    }
}

const LO7: u64 = 0x7f7f_7f7f_7f7f_7f7f;
const COMMAS: u64 = 0x2c2c_2c2c_2c2c_2c2c;
const QUOTES: u64 = 0x2222_2222_2222_2222;

/// High bit set in each byte of `x` that is zero (exact, no false positives from carries).
#[inline(always)]
fn zero_bytes(x: u64) -> u64 {
    !(((x & LO7) + LO7) | x | LO7)
}

/// Split a line into exactly N fields and strip quotes. `None` drops the line.
///
/// Commas and quotes are located eight bytes at a time. Quote rule: a field holds no quote, or
/// exactly two, at its first and last byte (so `""` is an empty field). Anything else drops the
/// line. A line with more than N-1 commas or more than 2N quotes is dropped at once.
#[inline]
pub fn fields<const N: usize>(line: B) -> Option<[B; N]> {
    let mut commas = [0usize; N];
    let mut nc = 0usize;
    let mut quotes = [0usize; 16];
    let mut nq = 0usize;
    let mut base = 0usize;
    let mut words = line.chunks_exact(8);
    for w in &mut words {
        let x = u64::from_le_bytes(w.try_into().unwrap());
        let mut mc = zero_bytes(x ^ COMMAS);
        let mut mq = zero_bytes(x ^ QUOTES);
        while mc != 0 {
            if nc == N - 1 {
                return None; // one comma too many
            }
            commas[nc] = base + (mc.trailing_zeros() >> 3) as usize;
            nc += 1;
            mc &= mc - 1;
        }
        while mq != 0 {
            if nq == 2 * N.min(8) {
                return None;
            }
            quotes[nq] = base + (mq.trailing_zeros() >> 3) as usize;
            nq += 1;
            mq &= mq - 1;
        }
        base += 8;
    }
    for (i, &c) in words.remainder().iter().enumerate() {
        if c == b',' {
            if nc == N - 1 {
                return None;
            }
            commas[nc] = base + i;
            nc += 1;
        } else if c == b'"' {
            if nq == 2 * N.min(8) {
                return None;
            }
            quotes[nq] = base + i;
            nq += 1;
        }
    }
    if nc != N - 1 {
        return None;
    }
    let mut out: [B; N] = [&[]; N];
    let mut start = 0usize;
    let mut qi = 0usize;
    for k in 0..N {
        let end = if k < N - 1 { commas[k] } else { line.len() };
        let mut field = &line[start..end];
        // quotes inside [start, end)
        let q0 = qi;
        while qi < nq && quotes[qi] < end {
            qi += 1;
        }
        match qi - q0 {
            0 => {}
            2 if quotes[q0] == start && quotes[q0 + 1] == end - 1 => field = &line[start + 1..end - 1],
            _ => return None,
        }
        out[k] = field;
        start = end + 1;
    }
    Some(out)
}

/// Bytes stored as (pointer, length) inside a leaked mmap.
#[inline]
pub fn line_at(ptr: usize, len: u32) -> B {
    // SAFETY: ptr/len always come from a `B` slice of a mapping that is never unmapped.
    unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialect() {
        assert_eq!(fields::<2>(b"a,b"), Some([&b"a"[..], b"b"]));
        assert_eq!(fields::<2>(b"\"a\",\"\""), Some([&b"a"[..], b""]));
        assert_eq!(fields::<2>(b" a ,b\r"), Some([&b" a "[..], b"b\r"]));
        assert_eq!(fields::<2>(b"a,b,c"), None);
        assert_eq!(fields::<2>(b""), None);
        assert_eq!(fields::<2>(b"a\"x,b"), None);
        assert_eq!(fields::<2>(b"\"a,b"), None);
        assert_eq!(fields::<2>(b"\"a\"x,b"), None);
        assert_eq!(fields::<2>(b"\",b"), None);
        assert_eq!(fields::<2>(b"a,\"\"\""), None);
        assert_eq!(fields::<2>(b"\"\",\"\""), Some([&b""[..], b""]));
        assert_eq!(fields::<3>(b"x,\"yyyyyyyyyyyy\",zzzzzzzzzzzzzzzz"), Some([&b"x"[..], b"yyyyyyyyyyyy", b"zzzzzzzzzzzzzzzz"]));
        assert_eq!(fields::<1>(b"abc"), Some([&b"abc"[..]]));
        assert_eq!(fields::<1>(b"\""), None);
    }

    /// The word-at-a-time splitter against a plain byte-by-byte one, on random lines.
    #[test]
    fn matches_simple_splitter() {
        fn simple<const N: usize>(line: &[u8]) -> Option<[Vec<u8>; N]> {
            let parts: Vec<&[u8]> = line.split(|&c| c == b',').collect();
            if parts.len() != N {
                return None;
            }
            let mut out: [Vec<u8>; N] = std::array::from_fn(|_| vec![]);
            for (k, f) in parts.iter().enumerate() {
                let q = f.iter().filter(|&&c| c == b'"').count();
                out[k] = match q {
                    0 => f.to_vec(),
                    2 if f.len() >= 2 && f[0] == b'"' && f[f.len() - 1] == b'"' => f[1..f.len() - 1].to_vec(),
                    _ => return None,
                };
            }
            Some(out)
        }
        let alphabet = b"ab,,\",\"1 \r";
        let mut seed = 12345u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..200_000 {
            let len = (next() % 40) as usize;
            let line: Vec<u8> = (0..len).map(|_| alphabet[(next() % alphabet.len() as u64) as usize]).collect();
            let leaked: B = Box::leak(line.into_boxed_slice());
            for n in [1usize, 2, 4] {
                let got: Option<Vec<Vec<u8>>> = match n {
                    1 => fields::<1>(leaked).map(|a| a.iter().map(|x| x.to_vec()).collect()),
                    2 => fields::<2>(leaked).map(|a| a.iter().map(|x| x.to_vec()).collect()),
                    _ => fields::<4>(leaked).map(|a| a.iter().map(|x| x.to_vec()).collect()),
                };
                let want: Option<Vec<Vec<u8>>> = match n {
                    1 => simple::<1>(leaked).map(|a| a.to_vec()),
                    2 => simple::<2>(leaked).map(|a| a.to_vec()),
                    _ => simple::<4>(leaked).map(|a| a.to_vec()),
                };
                assert_eq!(got, want, "line {:?} n {}", String::from_utf8_lossy(leaked), n);
            }
        }
    }

    #[test]
    fn read_map_covers_every_line_once() {
        let dir = std::env::temp_dir().join(format!("rm_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("empty.csv", vec![]),
            ("header_only.csv", b"a,b".to_vec()),
            ("header_nl.csv", b"a,b\n".to_vec()),
            ("one.csv", b"a,b\n1,2".to_vec()),
            ("many.csv", {
                let mut v = b"h,h\n".to_vec();
                for i in 0..5000 {
                    v.extend_from_slice(format!("{i},{}\n", "x".repeat(i % 37)).as_bytes());
                }
                v
            }),
            ("no_trailing_nl.csv", {
                let mut v = b"h\n".to_vec();
                for i in 0..777 {
                    v.extend_from_slice(format!("line{i}\n").as_bytes());
                }
                v.extend_from_slice(b"last");
                v
            }),
        ];
        let mut paths = vec![];
        for (name, bytes) in &cases {
            let p = dir.join(name);
            std::fs::write(&p, bytes).unwrap();
            paths.push(p.to_string_lossy().into_owned());
        }
        for target in [1usize, 7, 64, 1000, 4096, 1 << 20] {
            let pieces = read_map(&paths, target, |c| c.to_vec());
            let got: Vec<u8> = pieces.concat();
            let mut want = vec![];
            for (_, bytes) in &cases {
                if let Some(h) = bytes.iter().position(|&c| c == b'\n') {
                    want.extend_from_slice(&bytes[h + 1..]);
                }
            }
            assert_eq!(got, want, "target {target}");
            for p in &pieces {
                assert!(!p.is_empty());
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn chunking() {
        let f: B = b"h,h\n1,1\n2,2\n3,3\n";
        let c = chunks(f, 4);
        assert_eq!(c.concat(), b"1,1\n2,2\n3,3\n");
        assert!(c.iter().all(|x| x.ends_with(b"\n")));
        assert!(chunks(b"header only", 4).is_empty());
        assert!(chunks(b"", 4).is_empty());
    }

    fn unquote_old(f: B) -> Option<B> {
        if f.first() == Some(&b'"') {
            if f.len() < 2 || f[f.len() - 1] != b'"' {
                return None;
            }
            let inner = &f[1..f.len() - 1];
            if inner.contains(&b'"') {
                return None;
            }
            return Some(inner);
        }
        if f.contains(&b'"') {
            return None;
        }
        Some(f)
    }

    fn fields_old<const N: usize>(line: B) -> Option<[B; N]> {
        let mut out: [B; N] = [&[]; N];
        let mut start = 0;
        let mut k = 0;
        for (i, &c) in line.iter().enumerate() {
            if c == b',' {
                if k + 1 >= N {
                    return None;
                }
                out[k] = unquote_old(&line[start..i])?;
                k += 1;
                start = i + 1;
            }
        }
        if k != N - 1 {
            return None;
        }
        out[k] = unquote_old(&line[start..])?;
        Some(out)
    }

    /// Single-thread speed of the splitters on a real chunk: BENCH_FILE=path cargo test --release bench_split -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_split() {
        let path = std::env::var("BENCH_FILE").unwrap();
        let data = map_file(&path);
        let body = chunks(data, 48 << 20).into_iter().next().unwrap();
        for round in 0..3 {
            let t = std::time::Instant::now();
            let mut n = 0usize;
            for_each_line(body, |_| n += 1);
            let t_scan = t.elapsed();
            let t = std::time::Instant::now();
            let mut a = 0usize;
            for_each_line(body, |l| {
                if let Some(f) = fields_old::<7>(l) {
                    a += f[1].len();
                }
            });
            let t_old = t.elapsed();
            let t = std::time::Instant::now();
            let mut b = 0usize;
            for_each_line(body, |l| {
                if let Some(f) = fields::<7>(l) {
                    b += f[1].len();
                }
            });
            let t_new = t.elapsed();
            assert_eq!(a, b);
            eprintln!("round {round}: {n} lines, scan only {t_scan:?}, old split {t_old:?}, new split {t_new:?}");
        }
    }
}
