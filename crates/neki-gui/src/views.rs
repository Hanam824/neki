//! The Builds, Store and Profiles pages. Their data lives in a thread-local
//! on the UI thread; worker threads hand results back through
//! `upgrade_in_event_loop`, so it is only ever touched there.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use neki_core::{Event, GcOptions, Generation, Neki, StorePath};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::app::{Level, kv, model, push_log, spawn_task};
use crate::{
    AppState, AppWindow, BuildItem, GenerationItem, GraphNode, KeyValue, PathDetail, PathItem,
};

/// Build log lines kept per derivation.
const MAX_LOG_LINES: usize = 5000;

/// Paths shown in the dependency tree, at most.
const MAX_TREE_LINES: usize = 500;

/// A store path with its size, for the store list.
#[derive(Clone)]
struct PathRow {
    path: StorePath,
    nar_size: u64,
}

#[derive(Default)]
struct Data {
    /// Log lines of every build, by derivation.
    build_logs: HashMap<String, Vec<SharedString>>,
    /// Every valid path, sorted by name.
    paths: Vec<PathRow>,
    /// Indices into `paths` of the ones the search shows.
    shown: Vec<usize>,
    /// The loaded profile's generations.
    generations: Vec<Generation>,
    /// The derivations of the current (or last) build, for the graph.
    graph: Vec<GraphEntry>,
}

/// A derivation in the build graph.
struct GraphEntry {
    drv: String,
    name: String,
    waits_for: Vec<String>,
    /// "waiting", "running", "built" or "failed".
    status: String,
}

/// Size of a node in the build graph, and the gaps between nodes.
const NODE_WIDTH: f32 = 190.0;
const NODE_HEIGHT: f32 = 40.0;
const GAP_X: f32 = 50.0;
const GAP_Y: f32 = 14.0;

thread_local! {
    static DATA: RefCell<Data> = RefCell::default();
}

fn with<T>(f: impl FnOnce(&mut Data) -> T) -> T {
    DATA.with(|d| f(&mut d.borrow_mut()))
}

pub fn install_models(ui: &AppWindow) {
    let app = ui.global::<AppState>();
    app.set_builds(ModelRc::new(VecModel::<BuildItem>::default()));
    app.set_build_log(ModelRc::new(VecModel::<SharedString>::default()));
}

/// Human-readable size.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `YYYY-MM-DD HH:MM UTC` for Unix seconds.
pub fn time(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rest = secs.rem_euclid(86400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        rest / 3600,
        rest % 3600 / 60
    )
}

// ----- builds -----

fn builds(ui: &AppWindow) -> ModelRc<BuildItem> {
    ui.global::<AppState>().get_builds()
}

fn build_index(ui: &AppWindow, drv: &str) -> Option<usize> {
    builds(ui).iter().position(|b| b.drv == drv)
}

fn set_status(ui: &AppWindow, index: usize, status: &str) {
    let model = builds(ui);
    if let Some(mut item) = model.row_data(index) {
        item.status = status.into();
        model.set_row_data(index, item);
    }
}

/// Update the Builds page for a backend event.
pub fn track_build(ui: &AppWindow, event: Event) {
    let app = ui.global::<AppState>();
    let model = builds(ui);
    let Some(list) = model.as_any().downcast_ref::<VecModel<BuildItem>>() else {
        return;
    };
    match event {
        Event::BuildPlanned { drv, waits_for } => {
            with(|d| {
                // A new build: the previous graph is finished.
                if d.graph
                    .iter()
                    .all(|e| e.status != "waiting" && e.status != "running")
                {
                    d.graph.clear();
                }
                d.graph.push(GraphEntry {
                    drv: drv.to_string(),
                    name: drv.name().trim_end_matches(".drv").to_string(),
                    waits_for: waits_for.iter().map(ToString::to_string).collect(),
                    status: "waiting".into(),
                });
            });
            show_graph(ui);
        }
        Event::BuildStarted { drv } => {
            set_graph_status(ui, &drv.to_string(), "running");
            let key = drv.to_string();
            let name = drv.name().trim_end_matches(".drv").to_string();
            let index = match build_index(ui, &key) {
                Some(i) => {
                    set_status(ui, i, "running");
                    i
                }
                None => {
                    list.insert(
                        0,
                        BuildItem {
                            drv: key.as_str().into(),
                            name: name.into(),
                            status: "running".into(),
                        },
                    );
                    // Items moved down; keep the selection on the same build.
                    let selected = app.get_selected_build();
                    if selected >= 0 {
                        app.set_selected_build(selected + 1);
                    }
                    0
                }
            };
            with(|d| d.build_logs.insert(key, Vec::new()));
            // Follow the newest build unless another one is being read.
            let selected = app.get_selected_build();
            let following = usize::try_from(selected)
                .ok()
                .and_then(|i| model.row_data(i))
                .is_none_or(|b| b.status != "running" || selected == index as i32);
            if following {
                select_build(ui, index as i32);
            }
        }
        Event::BuildLog { drv, line } => {
            let key = drv.to_string();
            let line: SharedString = line.into();
            with(|d| {
                let lines = d.build_logs.entry(key.clone()).or_default();
                if lines.len() < MAX_LOG_LINES {
                    lines.push(line.clone());
                }
            });
            let selected = usize::try_from(app.get_selected_build()).ok();
            if selected
                .and_then(|i| model.row_data(i))
                .is_some_and(|b| b.drv == key)
            {
                let log = app.get_build_log();
                if let Some(log) = log.as_any().downcast_ref::<VecModel<SharedString>>()
                    && log.row_count() < MAX_LOG_LINES
                {
                    log.push(line);
                }
            }
        }
        Event::BuildFinished { drv, success } => {
            let status = if success { "built" } else { "failed" };
            set_graph_status(ui, &drv.to_string(), status);
            if let Some(i) = build_index(ui, &drv.to_string()) {
                set_status(ui, i, status);
            }
        }
        Event::Substituted { path } => {
            list.insert(
                0,
                BuildItem {
                    drv: path.to_string().into(),
                    name: path.name().into(),
                    status: "downloaded".into(),
                },
            );
            let selected = app.get_selected_build();
            if selected >= 0 {
                app.set_selected_build(selected + 1);
            }
        }
        Event::Download { .. } | Event::Message(_) => {}
    }
}

fn set_graph_status(ui: &AppWindow, drv: &str, status: &str) {
    let changed = with(|d| match d.graph.iter_mut().find(|e| e.drv == drv) {
        Some(entry) => {
            entry.status = status.to_string();
            true
        }
        None => false,
    });
    if changed {
        show_graph(ui);
    }
}

/// A node's column: how many derivations of the graph it waits for, one
/// after another (0 for those that wait for none).
fn depth(graph: &[GraphEntry], index: usize, memo: &mut HashMap<usize, usize>) -> usize {
    if let Some(&d) = memo.get(&index) {
        return d;
    }
    // Cycles can't happen (hashes), but don't loop if the data is odd.
    memo.insert(index, 0);
    let d = graph[index]
        .waits_for
        .iter()
        .filter_map(|w| graph.iter().position(|e| e.drv == *w))
        .map(|i| depth(graph, i, memo) + 1)
        .max()
        .unwrap_or(0);
    memo.insert(index, d);
    d
}

/// Lay out the build graph: inputs on the left, what uses them to the
/// right; one line from each input to each derivation waiting for it.
fn graph_layout(graph: &[GraphEntry]) -> (Vec<GraphNode>, String, f32, f32) {
    let mut memo = HashMap::new();
    let depths: Vec<usize> = (0..graph.len())
        .map(|i| depth(graph, i, &mut memo))
        .collect();
    let mut columns: Vec<Vec<usize>> = Vec::new();
    for (i, &d) in depths.iter().enumerate() {
        if columns.len() <= d {
            columns.resize(d + 1, Vec::new());
        }
        columns[d].push(i);
    }
    let mut position = vec![(0.0, 0.0); graph.len()];
    for (col, members) in columns.iter_mut().enumerate() {
        members.sort_by(|a, b| graph[*a].name.cmp(&graph[*b].name));
        for (row, &i) in members.iter().enumerate() {
            position[i] = (
                col as f32 * (NODE_WIDTH + GAP_X),
                row as f32 * (NODE_HEIGHT + GAP_Y),
            );
        }
    }
    let mut edges = String::new();
    for (i, entry) in graph.iter().enumerate() {
        for w in &entry.waits_for {
            if let Some(j) = graph.iter().position(|e| e.drv == *w) {
                let (x1, y1) = (
                    position[j].0 + NODE_WIDTH,
                    position[j].1 + NODE_HEIGHT / 2.0,
                );
                let (x2, y2) = (position[i].0, position[i].1 + NODE_HEIGHT / 2.0);
                edges.push_str(&format!("M {x1} {y1} L {x2} {y2} "));
            }
        }
    }
    let nodes = graph
        .iter()
        .zip(&position)
        .map(|(e, (x, y))| GraphNode {
            drv: e.drv.as_str().into(),
            name: e.name.as_str().into(),
            status: e.status.as_str().into(),
            x: *x,
            y: *y,
        })
        .collect();
    let width = columns.len() as f32 * (NODE_WIDTH + GAP_X);
    let height = columns.iter().map(Vec::len).max().unwrap_or(0) as f32 * (NODE_HEIGHT + GAP_Y);
    (nodes, edges, width, height)
}

fn show_graph(ui: &AppWindow) {
    let (nodes, edges, width, height) = with(|d| graph_layout(&d.graph));
    let app = ui.global::<AppState>();
    app.set_graph_nodes(model(nodes));
    app.set_graph_edges(edges.into());
    app.set_graph_width(width);
    app.set_graph_height(height);
}

/// Show the log of the build of `drv` (clicked in the graph).
pub fn select_build_drv(ui: &AppWindow, drv: &str) {
    if let Some(index) = build_index(ui, drv) {
        select_build(ui, index as i32);
        ui.global::<AppState>().set_builds_tab(0);
    }
}

pub fn select_build(ui: &AppWindow, index: i32) {
    let app = ui.global::<AppState>();
    let Some(item) = usize::try_from(index)
        .ok()
        .and_then(|i| builds(ui).row_data(i))
    else {
        return;
    };
    app.set_selected_build(index);
    let lines = with(|d| {
        d.build_logs
            .get(item.drv.as_str())
            .cloned()
            .unwrap_or_default()
    });
    app.set_build_log(ModelRc::new(VecModel::from(lines)));
}

// ----- store -----

pub fn load_store(ui: &AppWindow, neki: &Arc<Neki>) {
    spawn_task(
        ui,
        neki,
        "Reading the store",
        |neki, _| {
            let mut rows = Vec::new();
            for path in neki.store_paths()? {
                let info = neki.path_info(&path.to_string())?;
                rows.push(PathRow {
                    path,
                    nar_size: info.nar_size,
                });
            }
            rows.sort_by(|a, b| a.path.name().cmp(b.path.name()).then(a.path.cmp(&b.path)));
            Ok(rows)
        },
        |ui, rows| {
            let total: u64 = rows.iter().map(|r| r.nar_size).sum();
            let app = ui.global::<AppState>();
            app.set_store_summary(format!("{} paths, {}", rows.len(), size(total)).into());
            with(|d| d.paths = rows);
            filter_store(ui, &app.get_store_filter());
        },
    );
}

pub fn filter_store(ui: &AppWindow, filter: &str) {
    let filter = filter.to_lowercase();
    let items = with(|d| {
        d.shown = (0..d.paths.len())
            .filter(|&i| {
                filter.is_empty() || d.paths[i].path.to_string().to_lowercase().contains(&filter)
            })
            .collect();
        d.shown
            .iter()
            .map(|&i| {
                let row = &d.paths[i];
                PathItem {
                    name: row.path.name().into(),
                    hash: row.path.hash_part().into(),
                    size: size(row.nar_size).into(),
                }
            })
            .collect::<Vec<_>>()
    });
    let app = ui.global::<AppState>();
    app.set_store_paths(model(items));
    app.set_selected_path(-1);
}

fn shown_path(index: i32) -> Option<StorePath> {
    with(|d| {
        let i = *d.shown.get(usize::try_from(index).ok()?)?;
        Some(d.paths.get(i)?.path.clone())
    })
}

pub fn select_path(ui: &AppWindow, neki: &Arc<Neki>, index: i32) {
    let Some(path) = shown_path(index) else {
        return;
    };
    let store_dir = neki.config().store_dir.clone();
    spawn_task(
        ui,
        neki,
        format!("Reading {}", path.name()),
        move |neki, _| {
            let info = neki.path_info(&path.to_string())?;
            let closure = neki.closure(&path.to_string())?;
            let sizes: HashMap<StorePath, (u64, BTreeSet<StorePath>)> = closure
                .iter()
                .map(|i| (i.path.clone(), (i.nar_size, i.references.clone())))
                .collect();
            // The closure as a tree; a path seen before isn't expanded again.
            let mut tree = Vec::new();
            let mut seen = BTreeSet::new();
            let mut stack = vec![(path.clone(), 0usize)];
            while let Some((p, depth)) = stack.pop() {
                if tree.len() >= MAX_TREE_LINES {
                    tree.push("…".to_string());
                    break;
                }
                let (nar_size, refs) = sizes.get(&p).cloned().unwrap_or_default();
                let again = !seen.insert(p.clone());
                tree.push(format!(
                    "{}{} ({}){}",
                    "  ".repeat(depth),
                    p.name(),
                    size(nar_size),
                    if again && !refs.is_empty() {
                        " …"
                    } else {
                        ""
                    }
                ));
                if !again {
                    for r in refs.iter().rev().filter(|r| **r != p) {
                        stack.push((r.clone(), depth + 1));
                    }
                }
            }
            let deriver = info.deriver.map_or("(none)".to_string(), |d| d.to_string());
            Ok(PathDetailData {
                path: store_dir.join(path.to_string()).display().to_string(),
                nar_hash: info.nar_hash.to_sri(),
                nar_size: size(info.nar_size),
                closure_size: format!(
                    "{} in {} paths",
                    size(closure.iter().map(|i| i.nar_size).sum()),
                    closure.len()
                ),
                deriver,
                references: info
                    .references
                    .iter()
                    .filter(|r| **r != path)
                    .map(ToString::to_string)
                    .collect(),
                tree,
            })
        },
        move |ui, data| {
            let app = ui.global::<AppState>();
            app.set_path_detail(data.into_detail());
            app.set_selected_path(index);
            app.set_why_chains(model(Vec::new()));
        },
    );
}

/// A path's details, computed on a worker thread (Slint models can't be
/// sent between threads, so they are made on the UI thread).
struct PathDetailData {
    path: String,
    nar_hash: String,
    nar_size: String,
    closure_size: String,
    deriver: String,
    references: Vec<String>,
    tree: Vec<String>,
}

impl PathDetailData {
    fn into_detail(self) -> PathDetail {
        PathDetail {
            path: self.path.into(),
            nar_hash: self.nar_hash.into(),
            nar_size: self.nar_size.into(),
            closure_size: self.closure_size.into(),
            deriver: self.deriver.into(),
            references: model(
                self.references
                    .into_iter()
                    .map(SharedString::from)
                    .collect(),
            ),
            tree: model(self.tree.into_iter().map(SharedString::from).collect()),
        }
    }
}

pub fn why_depends(ui: &AppWindow, neki: &Arc<Neki>, target: &str) {
    let Some(from) = shown_path(ui.global::<AppState>().get_selected_path()) else {
        return;
    };
    let target = target.trim().to_string();
    spawn_task(
        ui,
        neki,
        format!("Why does {} depend on {target}?", from.name()),
        move |neki, _| neki.why_depends(&from.to_string(), &target),
        |ui, chains| {
            let lines: Vec<SharedString> = if chains.is_empty() {
                vec!["It doesn't depend on that path.".into()]
            } else {
                chains
                    .iter()
                    .map(|c| {
                        c.iter()
                            .map(StorePath::name)
                            .collect::<Vec<_>>()
                            .join(" → ")
                            .into()
                    })
                    .collect()
            };
            ui.global::<AppState>().set_why_chains(model(lines));
        },
    );
}

pub fn store_action(ui: &AppWindow, neki: &Arc<Neki>, action: &str) {
    match action {
        "verify" => spawn_task(
            ui,
            neki,
            "Verifying the store",
            |neki, _| neki.verify(),
            |ui, modified| {
                if modified.is_empty() {
                    push_log(ui, Level::Ok, "All store paths are intact");
                }
                for path in modified {
                    push_log(ui, Level::Error, format!("Modified: {path}"));
                }
            },
        ),
        "repair" => spawn_task(
            ui,
            neki,
            "Repairing the store",
            |neki, events| neki.repair(&[], events),
            |ui, report| {
                if report.repaired.is_empty() && report.failed.is_empty() {
                    push_log(
                        ui,
                        Level::Ok,
                        "Nothing to repair: all store paths are intact",
                    );
                }
                for path in &report.repaired {
                    push_log(ui, Level::Ok, format!("Repaired {path}"));
                }
                for (path, why) in &report.failed {
                    push_log(ui, Level::Error, format!("Cannot repair {path}: {why}"));
                }
            },
        ),
        "gc-preview" => spawn_task(
            ui,
            neki,
            "Working out what garbage collection would delete",
            |neki, _| {
                let result = neki.gc(&GcOptions {
                    dry_run: true,
                    ..GcOptions::default()
                })?;
                let mut rows = Vec::new();
                for path in &result.deleted {
                    let info = neki.path_info(&path.to_string())?;
                    rows.push((path.to_string(), info.nar_size));
                }
                rows.sort_by_key(|row| std::cmp::Reverse(row.1));
                Ok((rows, result.bytes_freed))
            },
            |ui, (rows, freed)| {
                let app = ui.global::<AppState>();
                app.set_gc_summary(
                    if rows.is_empty() {
                        "Nothing to delete: every path is reachable from a GC root.".to_string()
                    } else {
                        format!("{} paths ({}) would be deleted", rows.len(), size(freed))
                    }
                    .into(),
                );
                let items: Vec<KeyValue> = rows.into_iter().map(|(p, s)| kv(p, size(s))).collect();
                app.set_gc_preview(model(items));
            },
        ),
        "gc" => spawn_task(
            ui,
            neki,
            "Collecting garbage",
            |neki, _| neki.gc(&GcOptions::default()),
            |ui, result| {
                push_log(
                    ui,
                    Level::Ok,
                    format!(
                        "{} paths deleted, {} freed",
                        result.deleted.len(),
                        size(result.bytes_freed)
                    ),
                );
                let app = ui.global::<AppState>();
                app.set_gc_summary(SharedString::new());
                app.set_gc_preview(model(Vec::new()));
                app.invoke_load_store();
            },
        ),
        "optimise" => spawn_task(
            ui,
            neki,
            "Optimising the store",
            |neki, _| neki.optimise(),
            |ui, saved| push_log(ui, Level::Ok, format!("{} saved", size(saved))),
        ),
        _ => {}
    }
}

// ----- profiles -----

pub fn profile_action(ui: &AppWindow, neki: &Arc<Neki>, action: &str, profile: String, arg: &str) {
    let packages: Vec<String> = arg.split_whitespace().map(str::to_string).collect();
    // After a change, show the profile's generations again.
    let reload = |ui: &AppWindow, profile: &str| {
        ui.global::<AppState>().invoke_profile_action(
            "history".into(),
            profile.into(),
            SharedString::new(),
        );
    };
    match action {
        "history" => spawn_task(
            ui,
            neki,
            format!("Loading profile {profile}"),
            move |neki, _| {
                let p = neki.profile(&profile);
                let link = p.current_link().join("bin").display().to_string();
                let profiles: Vec<String> = neki.profiles()?.into_iter().map(|p| p.name).collect();
                Ok((
                    p.generations()?,
                    p.current()?.map(|g| g.number),
                    link,
                    profiles,
                ))
            },
            |ui, (generations, current, link, profiles)| {
                let app = ui.global::<AppState>();
                let items: Vec<GenerationItem> = generations
                    .iter()
                    .rev()
                    .map(|g| GenerationItem {
                        number: g.number as i32,
                        created: time(g.created).into(),
                        packages: g.packages.join(" ").into(),
                        current: Some(g.number) == current,
                    })
                    .collect();
                app.set_current_link(if current.is_some() {
                    link.into()
                } else {
                    SharedString::new()
                });
                app.set_generations(model(items));
                app.set_profiles(model(
                    profiles.into_iter().map(SharedString::from).collect(),
                ));
                app.set_generation_diff(model(Vec::new()));
                with(|d| d.generations = generations);
            },
        ),
        "install" | "remove" => {
            let install = action == "install";
            let name = profile.clone();
            spawn_task(
                ui,
                neki,
                format!(
                    "{} {}",
                    if install {
                        "Installing into"
                    } else {
                        "Removing from"
                    },
                    profile
                ),
                move |neki, events| {
                    if install {
                        neki.profile_install(&profile, &packages, events)
                    } else {
                        neki.profile_remove(&profile, &packages, events)
                    }
                },
                move |ui, generation| {
                    push_log(
                        ui,
                        Level::Ok,
                        format!("Created generation {}", generation.number),
                    );
                    ui.global::<AppState>()
                        .set_profile_packages(SharedString::new());
                    reload(ui, &name);
                },
            );
        }
        "rollback" | "switch" => {
            let number: Option<u32> = arg.trim().parse().ok();
            let name = profile.clone();
            spawn_task(
                ui,
                neki,
                format!("Switching profile {profile}"),
                move |neki, _| match number {
                    Some(n) => neki.profile_switch(&profile, n),
                    None => neki.profile_rollback(&profile),
                },
                move |ui, generation| {
                    push_log(
                        ui,
                        Level::Ok,
                        format!("Switched to generation {}", generation.number),
                    );
                    reload(ui, &name);
                },
            );
        }
        _ => {}
    }
}

/// What changed from generation `a` to generation `b`.
pub fn diff_generations(ui: &AppWindow, a: i32, b: i32) {
    let find = |n: i32| with(|d| d.generations.iter().find(|g| g.number as i32 == n).cloned());
    let app = ui.global::<AppState>();
    let (Some(from), Some(to)) = (find(a), find(b)) else {
        app.set_generation_diff(model(vec![kv(
            "",
            format!("Load the profile; there is no generation {a} or {b}."),
        )]));
        return;
    };
    let old: BTreeSet<&String> = from.packages.iter().collect();
    let new: BTreeSet<&String> = to.packages.iter().collect();
    let mut items: Vec<KeyValue> = new
        .difference(&old)
        .map(|p| kv("added", p.as_str()))
        .collect();
    items.extend(old.difference(&new).map(|p| kv("removed", p.as_str())));
    if items.is_empty() {
        items.push(kv(
            "",
            if from.path == to.path {
                "The same environment.".to_string()
            } else {
                "The same packages, built from different inputs.".to_string()
            },
        ));
    }
    app.set_generation_diff(model(items));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real window, without a window system: build events fill the
    /// Builds page, and generations can be compared.
    #[test]
    fn pages_follow_backend_data() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        install_models(&ui);
        let app = ui.global::<AppState>();

        let drv = StorePath::parse("0123456789abcdfghijklmnpqrsvwxyz-tool.drv").unwrap();
        track_build(&ui, Event::BuildStarted { drv: drv.clone() });
        track_build(
            &ui,
            Event::BuildLog {
                drv: drv.clone(),
                line: "compiling".into(),
            },
        );
        assert_eq!(app.get_selected_build(), 0, "follows the new build");
        assert_eq!(app.get_build_log().row_data(0).unwrap(), "compiling");
        track_build(
            &ui,
            Event::BuildFinished {
                drv,
                success: false,
            },
        );
        let item = app.get_builds().row_data(0).unwrap();
        assert_eq!(
            (item.name.as_str(), item.status.as_str()),
            ("tool", "failed")
        );

        let generation = |number, packages: &[&str]| Generation {
            number,
            created: 0,
            path: StorePath::parse("0123456789abcdfghijklmnpqrsvwxyz-env").unwrap(),
            packages: packages.iter().map(|p| p.to_string()).collect(),
        };
        with(|d| d.generations = vec![generation(1, &["pkgs.a"]), generation(2, &["pkgs.b"])]);
        diff_generations(&ui, 1, 2);
        let diff: Vec<(String, String)> = app
            .get_generation_diff()
            .iter()
            .map(|kv| (kv.key.to_string(), kv.value.to_string()))
            .collect();
        assert_eq!(
            diff,
            [
                ("added".into(), "pkgs.b".into()),
                ("removed".into(), "pkgs.a".into())
            ]
        );

        // The build graph: `app` waits for `lib`, so `lib` is left of it.
        let lib = StorePath::parse("11111111111111111111111111111111-lib.drv").unwrap();
        let app_drv = StorePath::parse("22222222222222222222222222222222-app.drv").unwrap();
        track_build(
            &ui,
            Event::BuildPlanned {
                drv: lib.clone(),
                waits_for: vec![],
            },
        );
        track_build(
            &ui,
            Event::BuildPlanned {
                drv: app_drv.clone(),
                waits_for: vec![lib.clone()],
            },
        );
        let nodes: Vec<GraphNode> = app.get_graph_nodes().iter().collect();
        assert_eq!(nodes.len(), 2);
        let x = |name: &str| nodes.iter().find(|n| n.name == name).unwrap().x;
        assert_eq!(x("lib"), 0.0);
        assert_eq!(x("app"), NODE_WIDTH + GAP_X);
        assert!(app.get_graph_edges().starts_with("M 190 20 L 240 20"));
        track_build(&ui, Event::BuildStarted { drv: lib.clone() });
        let status = |name: &str| {
            app.get_graph_nodes()
                .iter()
                .find(|n| n.name == name)
                .unwrap()
                .status
                .to_string()
        };
        assert_eq!(status("lib"), "running");
        assert_eq!(status("app"), "waiting");
    }

    #[test]
    fn formats_sizes_and_times() {
        assert_eq!(size(512), "512 B");
        assert_eq!(size(1536), "1.5 KiB");
        assert_eq!(size(5 * 1024 * 1024), "5.0 MiB");
        assert_eq!(time(0), "1970-01-01 00:00 UTC");
        assert_eq!(time(1_792_454_400), "2026-10-20 00:00 UTC");
        assert_eq!(time(951_782_400 + 3723), "2000-02-29 01:02 UTC");
    }
}
