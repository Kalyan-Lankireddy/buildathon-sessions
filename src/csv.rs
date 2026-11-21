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

/// Map every file and cut it into chunks.
pub fn load_chunks(files: &[String], target: usize) -> Vec<B> {
    use rayon::prelude::*;
    let per_file: Vec<Vec<B>> = files.par_iter().map(|f| chunks(map_file(f), target)).collect();
    per_file.into_iter().flatten().collect()
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

#[inline]
fn unquote(f: B) -> Option<B> {
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

/// Split a line into exactly N fields and strip quotes. `None` drops the line.
#[inline]
pub fn fields<const N: usize>(line: B) -> Option<[B; N]> {
    let mut out: [B; N] = [&[]; N];
    let mut start = 0;
    let mut k = 0;
    for (i, &c) in line.iter().enumerate() {
        if c == b',' {
            if k + 1 >= N {
                return None;
            }
            out[k] = unquote(&line[start..i])?;
            k += 1;
            start = i + 1;
        }
    }
    if k != N - 1 {
        return None;
    }
    out[k] = unquote(&line[start..])?;
    Some(out)
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
}
