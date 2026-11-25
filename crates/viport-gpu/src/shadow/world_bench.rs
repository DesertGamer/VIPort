//! A world frame's copies of title memory, replayed on a made-up heap:
//! the refreshes, snapshots and copy-back of 36 submissions, timed apart.
//! Its shape follows what world frames were measured to do (#400): one
//! 174 MB heap that every submission declares dozens of overlapping views
//! of as shader-written, refreshes asking for some 20 GB a frame of which
//! a few percent changed, and a few megabytes written by shaders. The
//! numbers are not the title's; they are the same for every build, so two
//! builds can be compared on one machine.
//!
//! It also checks what copy-back must keep (decision 0007, "Copies of title
//! memory"): bytes the GPU wrote reach title memory, and bytes the title
//! wrote while the GPU ran stay as the title left them.
//!
//! `cargo test --release -p viport-gpu world_frame_copies -- --ignored --nocapture`

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::*;

/// The heap every submission declares views of.
const HEAP: usize = 174 << 20;
const SUBMISSIONS: usize = 36;
/// Ranges each submission declares as shader-written (and refreshes before).
const DECLARED: usize = 24;
/// The views of the heap those ranges are taken from (passes declare the
/// same ranges again and again).
const VIEWS: usize = 64;
/// Pages the title's threads write between two submissions.
const TITLE_PAGES: usize = 256;
/// Places in the declared ranges the GPU writes per submission.
const GPU_WRITES: usize = 4;
/// The first frame takes the whole heap and fills the pools; not counted.
const FRAMES: usize = 6;

/// Xorshift, seeded the same in every build.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// A length between `lo` and `hi`, as often in each power of two.
    fn length(&mut self, lo: usize, hi: usize) -> usize {
        let (a, b) = (lo.ilog2(), hi.ilog2());
        let bits = a + self.below((b - a) as usize) as u32;
        (1 << bits) + self.below(1 << bits)
    }

    /// A range of the heap at `base`, starting on 256 bytes.
    fn range(&mut self, base: usize, lo: usize, hi: usize) -> (usize, usize) {
        let len = self.length(lo, hi).min(HEAP - 256);
        let at = self.below((HEAP - len) / 256) * 256;
        (base + at, len)
    }
}

#[derive(Default)]
struct Frame {
    refresh: Duration,
    refreshed: usize,
    asked: usize,
    snapshots: Duration,
    copy_back: Duration,
    written: usize,
}

fn time<T>(total: &mut Duration, f: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let r = f();
    *total += started.elapsed();
    r
}

fn put(at: usize, value: u64) {
    // SAFETY: an aligned address inside an allocation of the test's.
    unsafe { (at as *mut u64).write(value) }
}

fn get(at: usize) -> u64 {
    // SAFETY: as above.
    unsafe { (at as *const u64).read() }
}

#[test]
#[ignore = "benchmark: needs a host D3D12 device and ~600 MB; run in release with --ignored --nocapture"]
fn world_frame_copies() {
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
    };
    const MEM_WRITE_WATCH: u32 = 0x20_0000;
    if crate::host::host().is_none() {
        eprintln!("no host D3D12 device: skipped");
        return;
    }
    // Title memory, write-watched as the runtime reserves it.
    // SAFETY: a fresh allocation, freed at the end.
    let base = unsafe {
        VirtualAlloc(
            std::ptr::null(),
            HEAP,
            MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH,
            PAGE_READWRITE,
        )
    } as usize;
    assert_ne!(base, 0);
    for at in (base..base + HEAP).step_by(8) {
        put(at, at as u64);
    }
    register(0x5eed_0174, base as u64, HEAP as u64);
    let shadow = find(base as u64, 1).expect("a host copy of the heap");
    shadow.update(base, HEAP);

    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let views: Vec<(usize, usize)> = (0..VIEWS)
        .map(|_| rng.range(base, 64 << 10, 64 << 20))
        .collect();
    let mut stamp = 1u64 << 63;
    let mut frames = Vec::new();
    for _ in 0..FRAMES {
        let mut f = Frame::default();
        for _ in 0..SUBMISSIONS {
            for _ in 0..TITLE_PAGES {
                stamp += 1;
                put(base + rng.below(HEAP / 8) * 8, stamp);
            }
            let declared: Vec<(usize, usize)> =
                (0..DECLARED).map(|_| views[rng.below(VIEWS)]).collect();
            // Each range's draws read some of the heap first.
            for &(address, len) in &declared {
                let (read, n) = rng.range(base, 1 << 20, 128 << 20);
                f.asked += n;
                f.refreshed += time(&mut f.refresh, || shadow.update(read, n));
                time(&mut f.snapshots, || shadow.writing(address, len));
            }
            // The GPU writes into the declared ranges, while the title
            // writes the eight bytes after each place (same pages).
            let mut gpu = BTreeMap::new();
            let mut title = BTreeMap::new();
            for _ in 0..GPU_WRITES {
                let (address, len) = declared[rng.below(DECLARED)];
                let at = (address + rng.below(len - 16)) & !7;
                if gpu.contains_key(&(at + 8)) || title.contains_key(&at) {
                    continue;
                }
                stamp += 1;
                gpu.insert(at, stamp);
                put(shadow.data as usize + (at - base), stamp);
                stamp += 1;
                title.insert(at + 8, stamp);
                put(at + 8, stamp);
            }
            let mut once = declared.clone();
            once.sort_unstable();
            once.dedup();
            for (address, len) in once {
                f.written += time(&mut f.copy_back, || shadow.copy_back(address, len));
            }
            for (&at, &value) in &gpu {
                assert_eq!(
                    get(at),
                    value,
                    "a GPU write at heap offset {:#x} did not reach title memory",
                    at - base
                );
            }
            for (&at, &value) in &title {
                assert_eq!(
                    get(at),
                    value,
                    "the title's write at heap offset {:#x} was overwritten",
                    at - base
                );
            }
        }
        frames.push(f);
    }
    // SAFETY: our allocation; the copy keeps no pointer into it.
    unsafe { VirtualFree(base as *mut _, 0, MEM_RELEASE) };

    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    let mb = |n: usize| n as f64 / f64::from(1 << 20);
    println!(
        "frame | refresh ms (MB copied of GB asked) | snapshots ms | copy-back ms (KB written) | total ms"
    );
    for (i, f) in frames.iter().enumerate().skip(1) {
        println!(
            "{i} | {:.1} ({:.0} of {:.1}) | {:.1} | {:.1} ({:.0}) | {:.1}",
            ms(f.refresh),
            mb(f.refreshed),
            mb(f.asked) / 1024.0,
            ms(f.snapshots),
            ms(f.copy_back),
            mb(f.written) * 1024.0,
            ms(f.refresh + f.snapshots + f.copy_back)
        );
    }
    let mut totals: Vec<f64> = frames[1..]
        .iter()
        .map(|f| ms(f.refresh + f.snapshots + f.copy_back))
        .collect();
    totals.sort_by(f64::total_cmp);
    println!("median total: {:.1} ms per frame", totals[totals.len() / 2]);
}

const COPIES: usize = 300;
const LOOKUPS: usize = 10_000;
const SPARES: usize = 4000;

#[test]
#[ignore = "benchmark: needs a host D3D12 device; run in release with --ignored --nocapture"]
fn view_lookups_speed() {
    let Some(host) = crate::host::host() else {
        eprintln!("no host D3D12 device: skipped");
        return;
    };
    let pool = crate::watch::tests::pool(COPIES);
    let page = crate::watch::PAGE;
    for i in 0..COPIES {
        register(0xbe00 + i, (pool + i * page) as u64, page as u64);
    }
    let held: Vec<_> = (0..COPIES)
        .map(|i| find((pool + i * page) as u64, 1).expect("a copy"))
        .collect();
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let addresses: Vec<u64> = (0..LOOKUPS)
        .map(|_| (pool + rng.below(COPIES) * page + rng.below(page / 256 - 1) * 256) as u64)
        .collect();

    let started = Instant::now();
    for &address in &addresses {
        assert!(find(address, 16).is_some());
    }
    let find_all = started.elapsed();

    let started = Instant::now();
    for &address in &addresses {
        assert!(for_view(address, 256, 16).is_some());
    }
    let aligned = started.elapsed();

    let off: Vec<u64> = addresses.iter().take(64).map(|a| a + 4).collect();
    let views: Vec<_> = off.iter().map(|&a| for_view(a, 256, 16).expect("a view copy")).collect();
    let started = Instant::now();
    for i in 0..LOOKUPS {
        assert!(for_view(off[i % off.len()], 256, 16).is_some());
    }
    let exact = started.elapsed();

    let mut spare_buffers = Vec::new();
    for _ in 0..SPARES {
        let (buffer, data) = host.shared_buffer(256).expect("a buffer");
        // SAFETY: a live buffer.
        let gpu = unsafe { buffer.GetGPUVirtualAddress() };
        let mut shadow = Shadow::new(buffer, data, gpu, 0, 256);
        shadow.spare_capacity = 256;
        spare_buffers.push(shadow);
    }
    drop(spare_buffers);
    let started = Instant::now();
    for _ in 0..LOOKUPS {
        assert!(take_spare(512).is_none());
    }
    let spare_miss = started.elapsed();

    drop(views);
    drop(held);
    for i in 0..COPIES {
        unregister(0xbe00 + i, (pool + i * page) as u64);
    }
    let us = |d: Duration| d.as_secs_f64() * 1e6 / LOOKUPS as f64;
    println!(
        "per call, us: find {:.3}, for_view on elements {:.3}, for_view off elements (cached copy) {:.3}, take_spare miss with {SPARES} spares {:.3}",
        us(find_all),
        us(aligned),
        us(exact),
        us(spare_miss)
    );
}
