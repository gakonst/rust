//! This module defines parallel operations that are implemented in
//! one way for the serial compiler, and another way the parallel compiler.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

use parking_lot::{Condvar, Mutex};

use crate::FatalErrorMarker;
use crate::sync::{DynSend, DynSync, FromDyn, IntoDynSyncSend, mode};

/// A guard used to hold panics that occur during a parallel section to later by unwound.
/// This is used for the parallel compiler to prevent fatal errors from non-deterministically
/// hiding errors by ensuring that everything in the section has completed executing before
/// continuing with unwinding. It's also used for the non-parallel code to ensure error message
/// output match the parallel compiler for testing purposes.
pub struct ParallelGuard {
    panic: Mutex<Option<IntoDynSyncSend<Box<dyn Any + Send + 'static>>>>,
}

impl ParallelGuard {
    pub fn run<R>(&self, f: impl FnOnce() -> R) -> Option<R> {
        catch_unwind(AssertUnwindSafe(f))
            .map_err(|err| {
                let mut panic = self.panic.lock();
                if panic.is_none() || !(*err).is::<FatalErrorMarker>() {
                    *panic = Some(IntoDynSyncSend(err));
                }
            })
            .ok()
    }
}

/// This gives access to a fresh parallel guard in the closure and will unwind any panics
/// caught in it after the closure returns.
#[inline]
pub fn parallel_guard<R>(f: impl FnOnce(&ParallelGuard) -> R) -> R {
    let guard = ParallelGuard { panic: Mutex::new(None) };
    let ret = f(&guard);
    if let Some(IntoDynSyncSend(panic)) = guard.panic.into_inner() {
        resume_unwind(panic);
    }
    ret
}

fn serial_join<A, B, RA, RB>(oper_a: A, oper_b: B) -> (RA, RB)
where
    A: FnOnce() -> RA,
    B: FnOnce() -> RB,
{
    let (a, b) = parallel_guard(|guard| {
        let a = guard.run(oper_a);
        let b = guard.run(oper_b);
        (a, b)
    });
    (a.unwrap(), b.unwrap())
}

pub fn spawn(func: impl FnOnce() + DynSend + 'static) {
    if let Some(proof) = mode::check_dyn_thread_safe() {
        let func = proof.derive(func);
        rustc_thread_pool::spawn(|| {
            (func.into_inner())();
        });
    } else {
        func()
    }
}

/// Runs the functions in parallel.
///
/// The first function is executed immediately on the current thread.
/// Use that for the longest running function for better scheduling.
pub fn par_fns(funcs: &mut [&mut (dyn FnMut() + DynSend)]) {
    parallel_guard(|guard: &ParallelGuard| {
        if let Some(proof) = mode::check_dyn_thread_safe() {
            let funcs = proof.derive(funcs);
            rustc_thread_pool::scope(|s| {
                let Some((first, rest)) = funcs.into_inner().split_at_mut_checked(1) else {
                    return;
                };

                // Reverse the order of the later functions since Rayon executes them in reverse
                // order when using a single thread. This ensures the execution order matches
                // that of a single threaded rustc.
                for f in rest.iter_mut().rev() {
                    let f = proof.derive(f);
                    s.spawn(|_| {
                        guard.run(|| (f.into_inner())());
                    });
                }

                // Run the first function without spawning to
                // ensure it executes immediately on this thread.
                guard.run(|| first[0]());
            });
        } else {
            for f in funcs {
                guard.run(|| f());
            }
        }
    });
}

#[inline]
pub fn par_join<A, B, RA: DynSend, RB: DynSend>(oper_a: A, oper_b: B) -> (RA, RB)
where
    A: FnOnce() -> RA + DynSend,
    B: FnOnce() -> RB + DynSend,
{
    if let Some(proof) = mode::check_dyn_thread_safe() {
        let oper_a = proof.derive(oper_a);
        let oper_b = proof.derive(oper_b);
        let (a, b) = parallel_guard(|guard| {
            rustc_thread_pool::join(
                move || guard.run(move || proof.derive(oper_a.into_inner()())),
                move || guard.run(move || proof.derive(oper_b.into_inner()())),
            )
        });
        (a.unwrap().into_inner(), b.unwrap().into_inner())
    } else {
        serial_join(oper_a, oper_b)
    }
}

fn par_slice<I: DynSend>(
    items: &mut [I],
    guard: &ParallelGuard,
    for_each: impl Fn(&mut I) + DynSync + DynSend,
    proof: FromDyn<()>,
) {
    match items {
        [] => return,
        [item] => {
            guard.run(|| for_each(item));
            return;
        }
        _ => (),
    }

    let for_each = proof.derive(for_each);
    let mut items = for_each.derive(items);
    rustc_thread_pool::scope(|s| {
        let proof = items.derive(());

        const MAX_GROUP_COUNT: usize = 128;
        let group_size = items.len().div_ceil(MAX_GROUP_COUNT);
        let mut groups = items.chunks_mut(group_size);

        let Some(first_group) = groups.next() else { return };

        // Reverse the order of the later functions since Rayon executes them in reverse
        // order when using a single thread. This ensures the execution order matches
        // that of a single threaded rustc.
        for group in groups.rev() {
            let group = proof.derive(group);
            s.spawn(|_| {
                let mut group = group;
                for i in group.iter_mut() {
                    guard.run(|| for_each(i));
                }
            });
        }

        // Run the first function without spawning to avoid overwhelming stealing.
        for i in first_group.iter_mut() {
            guard.run(|| for_each(i));
        }
    });
}

pub fn par_for_each_in<I: DynSend, T: IntoIterator<Item = I>>(
    t: T,
    for_each: impl Fn(&I) + DynSync + DynSend,
) {
    parallel_guard(|guard| {
        if let Some(proof) = mode::check_dyn_thread_safe() {
            let mut items: Vec<_> = t.into_iter().collect();
            par_slice(&mut items, guard, |i| for_each(&*i), proof)
        } else {
            t.into_iter().for_each(|i| {
                guard.run(|| for_each(&i));
            });
        }
    });
}

/// Runs `for_each` on every item of `items`, in parallel if the parallel frontend is enabled.
///
/// Unlike [`par_for_each_in`], which splits the items into contiguous groups, this *starts* the
/// items in slice order: every worker repeatedly takes the next not-yet-started item. Putting the
/// most expensive items first therefore keeps a few large items from being started last and
/// dominating the wall time (greedy list scheduling). With a single thread, this is a sequential
/// loop over the items in order.
pub fn par_for_each_in_order<I: DynSync>(items: &[I], for_each: impl Fn(&I) + DynSync + DynSend) {
    parallel_guard(|guard| {
        if let Some(proof) = mode::check_dyn_thread_safe()
            && items.len() > 1
        {
            use std::sync::atomic::{AtomicUsize, Ordering};

            let next = AtomicUsize::new(0);
            let items = proof.derive(items);
            let for_each = proof.derive(for_each);
            let worker = || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    guard.run(|| for_each(item));
                }
            };
            let workers = rustc_thread_pool::current_num_threads().min(items.len());
            rustc_thread_pool::scope(|s| {
                for _ in 1..workers {
                    s.spawn(|_| worker());
                }
                worker();
            });
        } else {
            items.iter().for_each(|item| {
                guard.run(|| for_each(item));
            });
        }
    });
}

/// Processes a dynamically growing set of work items: `process` handles one item and pushes the
/// follow-up items it discovers onto the given vector; each of those is processed later in turn.
///
/// With the parallel frontend, every follow-up item becomes a separate task of one thread pool
/// scope, so any thread can pick up any part of the work and nested work never waits on a stack
/// (unlike recursive `par_for_each_in` calls, whose blocking waits nest). The thread that
/// discovered follow-up items continues with the first one itself. Without the parallel frontend,
/// items are processed depth-first: all follow-ups of an item (and theirs) before its next sibling.
pub fn par_work_queue<T: DynSend>(roots: Vec<T>, process: impl Fn(T, &mut Vec<T>) + DynSync + DynSend) {
    parallel_guard(|guard| {
        if let Some(proof) = mode::check_dyn_thread_safe() {
            let process = proof.derive(process);
            let roots = proof.derive(roots);
            rustc_thread_pool::scope(|s| {
                for root in roots.into_inner().into_iter().rev() {
                    work_queue_spawn(s, proof.derive(root), &process, guard);
                }
            });
        } else {
            let mut stack: Vec<T> = roots;
            stack.reverse();
            let mut next = Vec::new();
            while let Some(item) = stack.pop() {
                guard.run(|| process(item, &mut next));
                stack.extend(next.drain(..).rev());
            }
        }
    });
}

/// Computes `compute(job)` for every job on the thread pool, at most `lookahead` jobs ahead of a
/// consumer that takes the results strictly in job order.
///
/// `consume` runs on the current thread and receives a `take` callback: every call returns the
/// result of the next job (in the order of `jobs`). If no worker has started that job yet, `take`
/// computes it on the current thread; while waiting for a worker to finish it, the current thread
/// computes other pending jobs within the lookahead. Unlike fixed batches with a barrier after each
/// batch, workers pick up the next job as soon as they finish one. A panic in `compute` resumes on
/// the current thread when `take` reaches the job that panicked (as if it had been computed there).
///
/// Without the parallel frontend (or with `workers == 0`), `take` simply computes the next job.
pub fn par_ordered_jobs<J: DynSend, R: DynSend, T>(
    jobs: Vec<J>,
    workers: usize,
    lookahead: usize,
    compute: impl Fn(J) -> R + DynSync + DynSend,
    consume: impl FnOnce(&mut dyn FnMut() -> R) -> T,
) -> T {
    let Some(proof) = mode::check_dyn_thread_safe().filter(|_| workers > 0 && jobs.len() > 1)
    else {
        let mut jobs = jobs.into_iter();
        return consume(&mut || compute(jobs.next().expect("`take` called more often than there are jobs")));
    };
    let n = jobs.len();
    let shared = proof.derive((
        Mutex::new(OrderedJobs {
            jobs: jobs.into_iter().map(Some).collect(),
            results: (0..n).map(|_| None).collect(),
            claimed: vec![false; n],
            next: 0,
            consumed: 0,
            active: 0,
        }),
        Condvar::new(),
    ));
    let compute = proof.derive(compute);
    let ctx = OrderedJobsCtx { shared: &shared, compute: &compute, workers, lookahead: lookahead.max(1) };
    rustc_thread_pool::in_place_scope(|s| {
        ctx.top_up(s, &mut ctx.shared.0.lock());
        consume(&mut || ctx.take(s))
    })
}

struct OrderedJobs<J, R> {
    jobs: Vec<Option<J>>,
    results: Vec<Option<Result<R, IntoDynSyncSend<Box<dyn Any + Send + 'static>>>>>,
    /// Whether a worker or the consumer has started the job.
    claimed: Vec<bool>,
    /// Index of the first job that may still be unclaimed.
    next: usize,
    /// Number of results taken by the consumer.
    consumed: usize,
    /// Number of spawned workers that have not finished.
    active: usize,
}

struct OrderedJobsCtx<'a, J, R, F> {
    shared: &'a FromDyn<(Mutex<OrderedJobs<J, R>>, Condvar)>,
    compute: &'a FromDyn<F>,
    workers: usize,
    lookahead: usize,
}

impl<J, R, F> Clone for OrderedJobsCtx<'_, J, R, F> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<J, R, F> Copy for OrderedJobsCtx<'_, J, R, F> {}

impl<'a, J: DynSend, R: DynSend, F: Fn(J) -> R + DynSync + DynSend> OrderedJobsCtx<'a, J, R, F> {
    /// Claims the next unclaimed job within the lookahead, if any.
    fn claim(&self, st: &mut OrderedJobs<J, R>) -> Option<(usize, J)> {
        while st.next < st.jobs.len() && st.claimed[st.next] {
            st.next += 1;
        }
        let k = st.next;
        if k >= st.jobs.len() || k >= st.consumed + self.lookahead {
            return None;
        }
        st.next += 1;
        st.claimed[k] = true;
        Some((k, st.jobs[k].take().unwrap()))
    }

    fn run(&self, job: J) -> Result<R, IntoDynSyncSend<Box<dyn Any + Send + 'static>>> {
        catch_unwind(AssertUnwindSafe(|| (**self.compute)(job))).map_err(IntoDynSyncSend)
    }

    fn top_up(&self, s: &rustc_thread_pool::Scope<'a>, st: &mut OrderedJobs<J, R>) {
        let available = st.jobs.len().min(st.consumed + self.lookahead);
        let unclaimed = (st.next..available).filter(|&k| !st.claimed[k]).count();
        while st.active < self.workers.min(unclaimed) {
            st.active += 1;
            let ctx = *self;
            s.spawn(move |_| ctx.work());
        }
    }

    fn work(&self) {
        let mut st = self.shared.0.lock();
        while let Some((k, job)) = self.claim(&mut st) {
            drop(st);
            let result = self.run(job);
            st = self.shared.0.lock();
            st.results[k] = Some(result);
            self.shared.1.notify_all();
        }
        st.active -= 1;
    }

    fn take(&self, s: &rustc_thread_pool::Scope<'a>) -> R {
        let mut st = self.shared.0.lock();
        let k = st.consumed;
        assert!(k < st.jobs.len(), "`take` called more often than there are jobs");
        let result = loop {
            if let Some(result) = st.results[k].take() {
                break result;
            }
            if !st.claimed[k] {
                st.claimed[k] = true;
                let job = st.jobs[k].take().unwrap();
                drop(st);
                let result = self.run(job);
                st = self.shared.0.lock();
                break result;
            }
            // Help with other pending jobs instead of idling until the next result is ready.
            if let Some((j, job)) = self.claim(&mut st) {
                drop(st);
                let result = self.run(job);
                st = self.shared.0.lock();
                st.results[j] = Some(result);
                continue;
            }
            self.shared.1.wait(&mut st);
        };
        st.consumed = k + 1;
        self.top_up(s, &mut st);
        drop(st);
        match result {
            Ok(r) => r,
            Err(IntoDynSyncSend(panic)) => resume_unwind(panic),
        }
    }
}

fn work_queue_spawn<'s, T: DynSend + 's, F: Fn(T, &mut Vec<T>) + DynSync + DynSend + 's>(
    s: &rustc_thread_pool::Scope<'s>,
    item: FromDyn<T>,
    process: &'s FromDyn<F>,
    guard: &'s ParallelGuard,
) {
    s.spawn(move |s| {
        let mut item = item;
        let mut next = Vec::new();
        loop {
            guard.run(|| (**process)(item.into_inner(), &mut next));
            if next.is_empty() {
                break;
            }
            let mut rest = next.drain(..);
            let first = rest.next().unwrap();
            // Spawned in reverse so that this thread pops them (LIFO) in order after `first`.
            for n in rest.rev() {
                work_queue_spawn(s, process.derive(n), process, guard);
            }
            item = process.derive(first);
        }
    });
}

// FIXME: actually make parallel and `T: DynSend`
pub fn par_for_each_slice<T>(items: &mut [T], for_each: impl Fn(&mut T)) {
    parallel_guard(|guard| {
        items.iter_mut().for_each(|i| {
            guard.run(|| for_each(i));
        });
    });
}

/// This runs `for_each` in parallel for each iterator item. If one or more of the
/// `for_each` calls returns `Err`, the function will also return `Err`. The error returned
/// will be non-deterministic, but this is expected to be used with `ErrorGuaranteed` which
/// are all equivalent.
pub fn try_par_for_each_in<T: IntoIterator, E: DynSend>(
    t: T,
    for_each: impl Fn(&<T as IntoIterator>::Item) -> Result<(), E> + DynSync + DynSend,
) -> Result<(), E>
where
    <T as IntoIterator>::Item: DynSend,
{
    parallel_guard(|guard| {
        if let Some(proof) = mode::check_dyn_thread_safe() {
            let mut items: Vec<_> = t.into_iter().collect();

            let error = Mutex::new(None);

            par_slice(
                &mut items,
                guard,
                |i| {
                    if let Err(err) = for_each(&*i) {
                        *error.lock() = Some(err);
                    }
                },
                proof,
            );

            if let Some(err) = error.into_inner() { Err(err) } else { Ok(()) }
        } else {
            t.into_iter().filter_map(|i| guard.run(|| for_each(&i))).fold(Ok(()), Result::and)
        }
    })
}

pub fn par_map<I: DynSend, T: IntoIterator<Item = I>, R: DynSend, C: FromIterator<R>>(
    t: T,
    map: impl Fn(I) -> R + DynSync + DynSend,
) -> C {
    parallel_guard(|guard| {
        if let Some(proof) = mode::check_dyn_thread_safe() {
            let map = proof.derive(map);

            let mut items: Vec<(Option<I>, Option<R>)> =
                t.into_iter().map(|i| (Some(i), None)).collect();

            par_slice(
                &mut items,
                guard,
                |i| {
                    i.1 = Some(map(i.0.take().unwrap()));
                },
                proof,
            );

            items.into_iter().filter_map(|i| i.1).collect()
        } else {
            t.into_iter().filter_map(|i| guard.run(|| map(i))).collect()
        }
    })
}

pub fn broadcast<R: DynSend>(op: impl Fn(usize) -> R + DynSync) -> Vec<R> {
    if let Some(proof) = mode::check_dyn_thread_safe() {
        let op = proof.derive(op);
        let results = rustc_thread_pool::broadcast(|context| op.derive(op(context.index())));
        results.into_iter().map(|r| r.into_inner()).collect()
    } else {
        vec![op(0)]
    }
}
