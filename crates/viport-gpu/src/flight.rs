use std::cell::Cell;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub(crate) type Job = Box<dyn FnOnce() + Send>;

const STALL: Duration = Duration::from_secs(90);

thread_local! {
    static LANDING: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn landing_here() -> bool {
    LANDING.with(Cell::get)
}

#[derive(Default)]
pub(crate) struct Footprint {
    open: bool,
    ranges: Vec<(usize, usize)>,
}

impl Footprint {
    pub(crate) fn open_ended(&mut self) {
        self.open = true;
    }

    pub(crate) fn add(&mut self, from: usize, to: usize) {
        if from < to {
            self.ranges.push((from, to));
        }
    }

    fn seal(&mut self) {
        self.ranges.sort_unstable();
        let mut out = 0;
        for i in 1..self.ranges.len() {
            let (from, to) = self.ranges[i];
            if from <= self.ranges[out].1 {
                self.ranges[out].1 = self.ranges[out].1.max(to);
            } else {
                out += 1;
                self.ranges[out] = (from, to);
            }
        }
        self.ranges.truncate(out + usize::from(!self.ranges.is_empty()));
    }

    fn overlaps(&self, from: usize, to: usize) -> bool {
        if self.open {
            return true;
        }
        if from >= to {
            return false;
        }
        let at = self.ranges.partition_point(|&(_, end)| end <= from);
        self.ranges.get(at).is_some_and(|&(start, _)| start < to)
    }
}

struct State {
    in_air: bool,
    job: Option<Job>,
    lists: Vec<usize>,
    footprint: Footprint,
    after: Vec<Job>,
}

pub(crate) struct Air {
    state: Mutex<State>,
    work: Condvar,
    landed: Condvar,
}

impl Air {
    pub(crate) const fn new() -> Air {
        Air {
            state: Mutex::new(State {
                in_air: false,
                job: None,
                lists: Vec::new(),
                footprint: Footprint {
                    open: false,
                    ranges: Vec::new(),
                },
                after: Vec::new(),
            }),
            work: Condvar::new(),
            landed: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn wait_landed<'a>(&'a self, mut state: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        let since = Instant::now();
        while state.in_air {
            let (next, _) = self
                .landed
                .wait_timeout(state, Duration::from_secs(1))
                .unwrap_or_else(|e| e.into_inner());
            state = next;
            if state.in_air && since.elapsed() >= STALL {
                crate::host::fail(format_args!(
                    "a submission in the air has not landed in {} s: a landing waits for a lock a waiting thread holds, or the GPU hung",
                    STALL.as_secs()
                ));
            }
        }
        state
    }

    fn wait_while(&self, stop: impl FnOnce(&State) -> bool) {
        if landing_here() {
            return;
        }
        let state = self.lock();
        if state.in_air && stop(&state) {
            drop(self.wait_landed(state));
        }
    }

    pub(crate) fn settle(&self) {
        self.wait_while(|_| true);
    }

    pub(crate) fn gate(&self, from: usize, to: usize) {
        self.wait_while(|s| s.footprint.overlaps(from, to));
    }

    pub(crate) fn idle(&self) -> bool {
        !self.lock().in_air
    }

    pub(crate) fn gate_open(&self) {
        self.wait_while(|s| s.footprint.open);
    }

    pub(crate) fn wait_lists(&self, lists: &[usize]) {
        self.wait_while(|s| lists.iter().any(|l| s.lists.contains(l)));
    }

    pub(crate) fn fly(&self, lists: Vec<usize>, mut footprint: Footprint, landing: Job) {
        footprint.seal();
        let mut state = self.lock();
        if state.in_air {
            state = self.wait_landed(state);
        }
        state.in_air = true;
        state.lists = lists;
        state.footprint = footprint;
        state.job = Some(landing);
        drop(state);
        self.work.notify_one();
    }

    pub(crate) fn after_landing(&self, work: Job) {
        let mut state = self.lock();
        if state.in_air && !landing_here() {
            state.after.push(work);
            return;
        }
        drop(state);
        work();
    }

    fn take_job(&self) -> Job {
        let mut state = self.lock();
        loop {
            if let Some(job) = state.job.take() {
                return job;
            }
            state = self.work.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn land(&self) {
        let mut state = self.lock();
        state.in_air = false;
        state.lists.clear();
        state.footprint = Footprint::default();
        for work in std::mem::take(&mut state.after) {
            work();
        }
        drop(state);
        self.landed.notify_all();
    }

    pub(crate) fn serve(&self) -> ! {
        LANDING.with(|l| l.set(true));
        loop {
            let job = self.take_job();
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
                crate::host::fail(format_args!("the landing of a submission panicked"));
            }
            self.land();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};

    fn air() -> Arc<Air> {
        let air = Arc::new(Air::new());
        let serving = air.clone();
        std::thread::spawn(move || serving.serve());
        air
    }

    fn footprint(ranges: &[(usize, usize)]) -> Footprint {
        let mut f = Footprint::default();
        for &(a, b) in ranges {
            f.add(a, b);
        }
        f
    }

    fn held(air: &Air, lists: Vec<usize>, footprint: Footprint) -> mpsc::Sender<()> {
        let (release, wait) = mpsc::channel::<()>();
        air.fly(lists, footprint, Box::new(move || {
            wait.recv().unwrap();
        }));
        release
    }

    fn returns_soon(ask: impl FnOnce() + Send + 'static) -> (Arc<AtomicBool>, std::thread::JoinHandle<()>) {
        let done = Arc::new(AtomicBool::new(false));
        let flag = done.clone();
        let thread = std::thread::spawn(move || {
            ask();
            flag.store(true, Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(150));
        (done, thread)
    }

    #[test]
    fn ranges_are_merged_and_asked_by_overlap() {
        let mut f = footprint(&[(100, 200), (150, 300), (500, 600), (300, 310), (0, 0)]);
        f.seal();
        assert_eq!(f.ranges, [(100, 310), (500, 600)]);
        assert!(f.overlaps(0, 101));
        assert!(f.overlaps(309, 400));
        assert!(!f.overlaps(310, 500));
        assert!(!f.overlaps(0, 100));
        assert!(f.overlaps(599, 1000));
        assert!(!f.overlaps(600, 1000));
        assert!(!f.overlaps(120, 120));
        f.open_ended();
        assert!(f.overlaps(310, 500));
    }

    #[test]
    fn preparing_waits_only_where_the_landing_may_write() {
        let air = air();
        let release = held(&air, vec![1], footprint(&[(1000, 2000)]));
        let a = air.clone();
        let (outside, t1) = returns_soon(move || a.gate(5000, 6000));
        let a = air.clone();
        let (inside, t2) = returns_soon(move || a.gate(1500, 1600));
        let a = air.clone();
        let (computed, t3) = returns_soon(move || a.gate_open());
        assert!(outside.load(Ordering::SeqCst), "memory the landing cannot write is not waited for");
        assert!(computed.load(Ordering::SeqCst), "a closed footprint has no computed addresses");
        assert!(!inside.load(Ordering::SeqCst), "memory the landing writes waits for it");
        release.send(()).unwrap();
        t1.join().unwrap();
        t2.join().unwrap();
        t3.join().unwrap();
        assert!(inside.load(Ordering::SeqCst));
    }

    #[test]
    fn an_open_footprint_makes_every_gate_wait() {
        let air = air();
        let mut open = footprint(&[(1000, 2000)]);
        open.open_ended();
        let release = held(&air, vec![1], open);
        let a = air.clone();
        let (elsewhere, t1) = returns_soon(move || a.gate(5000, 6000));
        let a = air.clone();
        let (computed, t2) = returns_soon(move || a.gate_open());
        assert!(!elsewhere.load(Ordering::SeqCst));
        assert!(!computed.load(Ordering::SeqCst));
        release.send(()).unwrap();
        t1.join().unwrap();
        t2.join().unwrap();
    }

    #[test]
    fn a_list_in_the_air_is_waited_for_and_others_are_not() {
        let air = air();
        let release = held(&air, vec![0x10, 0x20], Footprint::default());
        let a = air.clone();
        let (other, t1) = returns_soon(move || a.wait_lists(&[0x30]));
        let a = air.clone();
        let (same, t2) = returns_soon(move || a.wait_lists(&[0x30, 0x20]));
        assert!(other.load(Ordering::SeqCst));
        assert!(!same.load(Ordering::SeqCst));
        release.send(()).unwrap();
        t1.join().unwrap();
        t2.join().unwrap();
    }

    #[test]
    fn work_after_a_landing_runs_in_order_after_what_the_landing_wrote() {
        let air = air();
        let written = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let log = written.clone();
        let (release, wait) = mpsc::channel::<()>();
        air.fly(
            vec![1],
            Footprint::default(),
            Box::new(move || {
                wait.recv().unwrap();
                log.lock().unwrap().push("copy back");
            }),
        );
        for what in ["signal 1", "signal 2"] {
            let log = written.clone();
            air.after_landing(Box::new(move || log.lock().unwrap().push(what)));
        }
        assert!(written.lock().unwrap().is_empty());
        release.send(()).unwrap();
        air.settle();
        assert_eq!(*written.lock().unwrap(), ["copy back", "signal 1", "signal 2"]);
        let log = written.clone();
        air.after_landing(Box::new(move || log.lock().unwrap().push("at once")));
        assert_eq!(written.lock().unwrap().last(), Some(&"at once"));
    }

    #[test]
    fn the_next_submission_flies_after_the_one_before_landed() {
        let air = air();
        let order = Arc::new(Mutex::new(Vec::<u32>::new()));
        let (release, wait) = mpsc::channel::<()>();
        let log = order.clone();
        air.fly(
            vec![1],
            Footprint::default(),
            Box::new(move || {
                wait.recv().unwrap();
                log.lock().unwrap().push(1);
            }),
        );
        let (a, log) = (air.clone(), order.clone());
        let (second, thread) = returns_soon(move || {
            a.fly(vec![2], Footprint::default(), Box::new(move || log.lock().unwrap().push(2)))
        });
        assert!(!second.load(Ordering::SeqCst), "one submission in the air at a time");
        release.send(()).unwrap();
        thread.join().unwrap();
        air.settle();
        assert_eq!(*order.lock().unwrap(), [1, 2]);
    }

    #[test]
    fn a_fence_moves_only_after_the_landing_has_copied_back() {
        use crate::object::{HRESULT, S_OK, This, slot};
        let null = std::ptr::null();
        let mut fence: This = null;
        let iid = &crate::fence::FENCE_IIDS[0];
        assert_eq!(crate::fence::create(null, 0, 0, iid, &mut fence), S_OK);
        let at = fence as usize;
        let completed = move || slot::<extern "system" fn(This) -> u64>(at as This, 0x40)(at as This);
        let air = air();
        let copied_back = Arc::new(AtomicBool::new(false));
        let seen_by_title = Arc::new(Mutex::new(None));
        let (release, wait) = mpsc::channel::<()>();
        let flag = copied_back.clone();
        air.fly(
            vec![1],
            Footprint::default(),
            Box::new(move || {
                wait.recv().unwrap();
                flag.store(true, Ordering::SeqCst);
            }),
        );
        let (flag, seen) = (copied_back.clone(), seen_by_title.clone());
        air.after_landing(Box::new(move || {
            *seen.lock().unwrap() = Some(flag.load(Ordering::SeqCst));
            slot::<extern "system" fn(This, u64) -> HRESULT>(at as This, 0x50)(at as This, 5);
        }));
        assert_eq!(completed(), 0, "the title is told nothing while the submission is in the air");
        release.send(()).unwrap();
        air.settle();
        assert_eq!(completed(), 5);
        assert_eq!(*seen_by_title.lock().unwrap(), Some(true), "copy-back was done when the value was set");
    }
}
