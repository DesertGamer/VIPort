use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use super::make::{self, Pending};

static IMAGE: Mutex<Option<(usize, usize)>> = Mutex::new(None);

const FILE: &str = "XboxBvhBuilder.dll";

pub fn note_title_dll(file: &str, base: usize, size: usize) {
    if file.eq_ignore_ascii_case(FILE) {
        tracing::info!(base = format_args!("{base:#x}"), size, "BVH builder mapped");
        *IMAGE.lock().unwrap_or_else(|e| e.into_inner()) = Some((base, base + size));
    }
}

fn image() -> Option<(usize, usize)> {
    *IMAGE.lock().unwrap_or_else(|e| e.into_inner())
}

static BUILT: AtomicBool = AtomicBool::new(false);

pub(crate) fn built() -> bool {
    BUILT.load(Ordering::Relaxed) || image().is_none()
}

#[inline(never)]
pub(crate) fn created_by_builder(code: &[*const u8]) -> bool {
    let Some((start, end)) = image() else {
        return false;
    };
    let inside = |a: usize| (start..end).contains(&a);
    if code.iter().any(|&c| inside(c as usize)) {
        return true;
    }
    callers().into_iter().any(inside)
}

fn callers() -> Vec<usize> {
    use windows_sys::Win32::System::Diagnostics::Debug::RtlCaptureStackBackTrace;
    let mut frames = [std::ptr::null_mut(); 24];
    let n = unsafe {
        RtlCaptureStackBackTrace(
            1,
            frames.len() as u32,
            frames.as_mut_ptr(),
            std::ptr::null_mut(),
        )
    };
    frames[..usize::from(n)]
        .iter()
        .map(|&f| f as usize)
        .collect()
}

static PIPELINES: Mutex<Vec<Weak<Pending>>> = Mutex::new(Vec::new());

pub(crate) fn note(p: &Arc<Pending>) {
    PIPELINES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Arc::downgrade(p));
    make::begin(p);
}

static DECIDED: Mutex<(u64, bool)> = Mutex::new((u64::MAX, true));

pub(crate) fn runs() -> bool {
    static SAID: AtomicBool = AtomicBool::new(false);
    let frame = crate::frame::current();
    let mut decided = DECIDED.lock().unwrap_or_else(|e| e.into_inner());
    if decided.0 == frame {
        return decided.1;
    }
    let states: Vec<Option<bool>> = {
        let mut pipelines = PIPELINES.lock().unwrap_or_else(|e| e.into_inner());
        pipelines.retain(|p| p.strong_count() > 0);
        pipelines
            .iter()
            .filter_map(Weak::upgrade)
            .map(|p| make::state(&p))
            .collect()
    };
    let open = all_made(&states);
    if open != decided.1 || !SAID.swap(true, Ordering::Relaxed) {
        let (made, refused) = (
            states.iter().filter(|s| **s == Some(true)).count(),
            states.iter().filter(|s| **s == Some(false)).count(),
        );
        tracing::info!(
            frame,
            pipelines = states.len(),
            made,
            refused,
            "BVH builder {}",
            if open {
                "runs"
            } else {
                "held back: not all its pipelines are made"
            }
        );
    }
    *decided = (frame, open);
    if open && !states.is_empty() && !BUILT.swap(true, Ordering::Relaxed) {
        tracing::info!(
            frame,
            "BVH builder ran whole: programs that walk its structures run from here on"
        );
    }
    open
}

fn all_made(states: &[Option<bool>]) -> bool {
    states.iter().all(|s| *s == Some(true))
}

#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    *DECIDED.lock().unwrap_or_else(|e| e.into_inner()) = (u64::MAX, true);
    BUILT.store(false, Ordering::Relaxed);
    guard
}

#[cfg(test)]
pub(crate) fn clear_image_for_tests() {
    *IMAGE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builder_runs_only_with_every_pipeline_made() {
        assert!(all_made(&[]));
        assert!(all_made(&[Some(true), Some(true)]));
        assert!(!all_made(&[Some(true), Some(false)]));
        assert!(!all_made(&[Some(true), None]));
    }

    #[test]
    fn builder_pipelines_are_known_by_their_origin() {
        let _one = test_lock();
        let here = builder_pipelines_are_known_by_their_origin as *const () as usize;
        let elsewhere = [0x7_0000_0000usize as *const u8];
        *IMAGE.lock().unwrap() = None;
        assert!(!created_by_builder(&elsewhere));
        *IMAGE.lock().unwrap() = Some((0x7_0000_0000, 0x7_0001_0000));
        assert!(created_by_builder(&elsewhere));
        assert!(!created_by_builder(&[0x8_0000_0000usize as *const u8]));
        *IMAGE.lock().unwrap() = Some((here - 0x10_0000, here + 0x10_0000));
        assert!(created_by_builder(&[]));
        *IMAGE.lock().unwrap() = None;
    }
}
