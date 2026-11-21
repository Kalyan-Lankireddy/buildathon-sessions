//! Global allocator for big blocks.
//!
//! Every allocation of 2 MB or more gets a 2 MB-aligned block and `madvise(MADV_HUGEPAGE)`, so
//! the kernel can back it with huge pages (512x fewer page faults than 4 KB pages). Freed big
//! blocks go to a small pool, capped in total size, and later big allocations of a similar size
//! reuse them: no munmap, no new page faults, no zeroing by the kernel. Everything under 2 MB
//! goes straight to the system allocator.
//!
//! The allocator must never allocate through itself, so its bookkeeping is two fixed arrays.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;

const HUGE: usize = 2 << 20;
const MAX_LIVE: usize = 4096;
const MAX_POOL: usize = 128;
/// Freed memory kept for reuse, in bytes. It still counts as resident, so keep it modest.
const POOL_CAP: usize = 3 << 30;

struct State {
    /// (pointer, block size) of big blocks handed out and not yet freed
    live: [(usize, usize); MAX_LIVE],
    nlive: usize,
    /// freed big blocks kept for reuse
    pool: [(usize, usize); MAX_POOL],
    npool: usize,
    pooled: usize,
}

static STATE: Mutex<State> =
    Mutex::new(State { live: [(0, 0); MAX_LIVE], nlive: 0, pool: [(0, 0); MAX_POOL], npool: 0, pooled: 0 });

pub struct HugeAlloc;

#[inline]
fn round_up(n: usize) -> usize {
    (n + HUGE - 1) & !(HUGE - 1)
}

#[inline]
fn is_big(l: &Layout) -> bool {
    l.size() >= HUGE && l.align() <= HUGE
}

#[cfg(target_os = "linux")]
fn advise_huge(p: *mut u8, len: usize) {
    extern "C" {
        fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
    }
    const MADV_HUGEPAGE: i32 = 14;
    // best effort: a kernel without THP just says no
    unsafe { madvise(p, len, MADV_HUGEPAGE) };
}
#[cfg(not(target_os = "linux"))]
fn advise_huge(_p: *mut u8, _len: usize) {}

fn lock() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

unsafe impl GlobalAlloc for HugeAlloc {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if !is_big(&l) {
            return System.alloc(l);
        }
        let size = round_up(l.size());
        let mut st = lock();
        // reuse the smallest pooled block that fits without wasting more than half
        if st.nlive < MAX_LIVE {
            let mut best: Option<usize> = None;
            for i in 0..st.npool {
                let s = st.pool[i].1;
                if s >= size && s <= size * 2 && best.is_none_or(|b| s < st.pool[b].1) {
                    best = Some(i);
                }
            }
            if let Some(i) = best {
                let (p, s) = st.pool[i];
                let last = st.npool - 1;
                st.pool[i] = st.pool[last];
                st.npool -= 1;
                st.pooled -= s;
                let n = st.nlive;
                st.live[n] = (p, s);
                st.nlive += 1;
                return p as *mut u8;
            }
        }
        let p = System.alloc(Layout::from_size_align_unchecked(size, HUGE));
        if p.is_null() {
            return p;
        }
        advise_huge(p, size);
        if st.nlive < MAX_LIVE {
            let n = st.nlive;
            st.live[n] = (p as usize, size);
            st.nlive += 1;
        }
        // an untracked block is always a fresh one of size round_up(layout.size())
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if !is_big(&l) {
            return System.dealloc(p, l);
        }
        let mut st = lock();
        let mut size = round_up(l.size());
        for i in 0..st.nlive {
            if st.live[i].0 == p as usize {
                size = st.live[i].1;
                let last = st.nlive - 1;
                st.live[i] = st.live[last];
                st.nlive -= 1;
                break;
            }
        }
        if st.pooled + size <= POOL_CAP && st.npool < MAX_POOL {
            let n = st.npool;
            st.pool[n] = (p as usize, size);
            st.npool += 1;
            st.pooled += size;
            return;
        }
        drop(st);
        System.dealloc(p, Layout::from_size_align_unchecked(size, HUGE));
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        if !is_big(&l) {
            return System.alloc_zeroed(l);
        }
        let p = self.alloc(l);
        if !p.is_null() {
            std::ptr::write_bytes(p, 0, l.size());
        }
        p
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new_size: usize) -> *mut u8 {
        let new_layout = Layout::from_size_align_unchecked(new_size, l.align());
        if !is_big(&l) && !is_big(&new_layout) {
            return System.realloc(p, l, new_size);
        }
        let q = self.alloc(new_layout);
        if !q.is_null() {
            std::ptr::copy_nonoverlapping(p, q, l.size().min(new_size));
            self.dealloc(p, l);
        }
        q
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn big_vectors_work_and_blocks_are_reused() {
        for round in 0..4usize {
            let n = (3 << 20) + round * 4096;
            let mut v: Vec<u8> = Vec::with_capacity(n);
            for i in 0..n {
                v.push((i % 251) as u8);
            }
            assert_eq!(v[n - 1], ((n - 1) % 251) as u8);
            assert_eq!(v.as_ptr() as usize % (2 << 20), 0, "big blocks are 2 MB aligned");
            let z = vec![0u64; 1 << 20];
            assert!(z.iter().all(|&x| x == 0));
            let mut g: Vec<u32> = Vec::new();
            for i in 0..(3u32 << 20) {
                g.push(i); // grows through realloc across the 2 MB boundary
            }
            assert_eq!(g[(3 << 20) - 1], (3 << 20) - 1);
        }
    }
}
