//! Working out what to build, and building it in dependency order.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};

use neki_derivation::Derivation;
use neki_hash::StorePath;
use neki_store::{LocalStore, Store};

use crate::{BuildEvent, BuildOptions, BuildResult, Error, Events, Result, builder};

/// Build `drvs` and every dependency whose outputs aren't valid yet, using
/// up to `opts.max_jobs` threads. Outputs that a substituter has are
/// downloaded instead, and then their dependencies aren't needed at all.
/// `events` is called from worker threads.
pub fn realise(
    store: &LocalStore,
    drvs: &[StorePath],
    opts: &BuildOptions,
    events: Events,
) -> Result<Vec<BuildResult>> {
    let _gc = store.gc_guard()?;
    let mut plan = Plan::default();
    for drv in drvs {
        plan.visit(store, drv, opts, events)?;
    }
    let built = run(store, &plan, opts, events)?;
    drvs.iter()
        .map(|drv| {
            let outputs = plan.drvs[drv].output_paths(store.store_dir())?;
            Ok(BuildResult {
                drv: drv.clone(),
                outputs,
                built: built.contains(drv),
            })
        })
        .collect()
}

#[derive(Default)]
struct Plan {
    drvs: BTreeMap<StorePath, Derivation>,
    /// Derivations to build, with the ones among them they wait for.
    todo: BTreeMap<StorePath, BTreeSet<StorePath>>,
}

impl Plan {
    /// Load `drv_path` and decide whether it (and its inputs) must be built.
    fn visit(
        &mut self,
        store: &LocalStore,
        drv_path: &StorePath,
        opts: &BuildOptions,
        events: Events,
    ) -> Result<()> {
        if self.drvs.contains_key(drv_path) {
            return Ok(());
        }
        let drv = store.read_derivation(drv_path)?;
        let outputs = drv.output_paths(store.store_dir())?;
        self.drvs.insert(drv_path.clone(), drv.clone());
        if outputs_valid(store, outputs.values(), opts, events)? {
            return Ok(());
        }
        for src in &drv.input_srcs {
            if !store.is_valid_path(src)? {
                return Err(neki_store::Error::NotValid(src.to_string()).into());
            }
        }
        let mut waits_for = BTreeSet::new();
        for input in drv.input_drvs.keys() {
            self.visit(store, input, opts, events)?;
            if self.todo.contains_key(input) {
                waits_for.insert(input.clone());
            }
        }
        self.todo.insert(drv_path.clone(), waits_for);
        Ok(())
    }
}

/// Whether every path in `outputs` is valid, after trying to substitute the
/// missing ones.
fn outputs_valid<'a>(
    store: &LocalStore,
    outputs: impl IntoIterator<Item = &'a StorePath>,
    opts: &BuildOptions,
    events: Events,
) -> Result<bool> {
    for path in outputs {
        if store.is_valid_path(path)? {
            continue;
        }
        let mut found = false;
        for substituter in &opts.substituters {
            match substituter.substitute(store, path) {
                Ok(true) => {
                    events(BuildEvent::Substituted { path: path.clone() });
                    found = true;
                    break;
                }
                Ok(false) => {}
                // A broken cache shouldn't stop the build; building is the fallback.
                Err(err) => events(BuildEvent::Log {
                    drv: path.clone(),
                    line: format!("warning: substituting {path} failed: {err}"),
                }),
            }
        }
        if !found {
            return Ok(false);
        }
    }
    Ok(true)
}

#[derive(Default)]
struct State {
    /// Planned derivations not started yet, with the unfinished ones they wait for.
    waiting: BTreeMap<StorePath, BTreeSet<StorePath>>,
    ready: VecDeque<StorePath>,
    running: usize,
    built: BTreeSet<StorePath>,
    error: Option<Error>,
}

/// Build everything in `plan.todo`, returning the derivations that ran.
fn run(
    store: &LocalStore,
    plan: &Plan,
    opts: &BuildOptions,
    events: Events,
) -> Result<BTreeSet<StorePath>> {
    let mut state = State::default();
    for (drv, waits_for) in &plan.todo {
        if waits_for.is_empty() {
            state.ready.push_back(drv.clone());
        } else {
            state.waiting.insert(drv.clone(), waits_for.clone());
        }
    }
    let state = Mutex::new(state);
    let changed = Condvar::new();
    // Set when a build fails, to stop the others; separate from `opts.cancel`
    // so the caller's flag isn't changed.
    let failed = AtomicBool::new(false);
    let stop = || failed.load(Ordering::Relaxed) || opts.cancel.load(Ordering::Relaxed);
    let lock = || state.lock().unwrap_or_else(|p| p.into_inner());
    std::thread::scope(|scope| {
        for _ in 0..opts.max_jobs.max(1).min(plan.todo.len().max(1)) {
            scope.spawn(|| {
                loop {
                    let drv = {
                        let mut s = lock();
                        loop {
                            if s.error.is_some() {
                                return;
                            }
                            if let Some(drv) = s.ready.pop_front() {
                                s.running += 1;
                                break drv;
                            }
                            if s.running == 0 {
                                return;
                            }
                            s = changed.wait(s).unwrap_or_else(|p| p.into_inner());
                        }
                    };
                    let result = if stop() {
                        Err(Error::Cancelled)
                    } else {
                        builder::build(
                            store,
                            &drv,
                            &plan.drvs[&drv],
                            &plan.drvs,
                            opts,
                            &stop,
                            events,
                        )
                    };
                    let mut s = lock();
                    s.running -= 1;
                    match result {
                        Ok(ran) => {
                            if ran {
                                s.built.insert(drv.clone());
                            }
                            let mut now_ready = Vec::new();
                            for (other, waits_for) in s.waiting.iter_mut() {
                                if waits_for.remove(&drv) && waits_for.is_empty() {
                                    now_ready.push(other.clone());
                                }
                            }
                            for other in now_ready {
                                s.waiting.remove(&other);
                                s.ready.push_back(other);
                            }
                        }
                        Err(err) => {
                            s.error.get_or_insert(err);
                            failed.store(true, Ordering::Relaxed);
                        }
                    }
                    changed.notify_all();
                }
            });
        }
    });
    let state = state.into_inner().unwrap_or_else(|p| p.into_inner());
    match state.error {
        Some(err) => Err(err),
        None => Ok(state.built),
    }
}
