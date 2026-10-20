//! Connects the UI's `AppState` callbacks to `neki-core`. UI state lives on
//! the UI thread; backend operations run on a worker thread and report back
//! through `upgrade_in_event_loop`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use neki_core::{Config, Event, EventSink, MANIFEST_FILE, Manifest, Neki, Project};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::registry::Registry;
use crate::views;
use crate::{AppState, AppWindow, KeyValue, LogEntry, ProjectDetail, ProjectItem};

/// Oldest activity entries are dropped beyond this.
const MAX_LOG: usize = 500;

/// A registered project folder and the result of loading it.
struct Entry {
    root: PathBuf,
    project: Result<Project, String>,
}

struct State {
    neki: Arc<Neki>,
    registry: Registry,
    entries: Vec<Entry>,
    /// Re-checks which projects changed since their last build.
    _watcher: slint::Timer,
}

type Shared = Rc<RefCell<State>>;

pub fn install(ui: &AppWindow) {
    ui.global::<AppState>()
        .set_log(ModelRc::new(VecModel::<LogEntry>::default()));
    views::install_models(ui);

    let registry = match Registry::default_file().map(Registry::load) {
        Some(Ok(registry)) => registry,
        Some(Err(err)) => {
            push_log(
                ui,
                Level::Error,
                format!("{err}; the project list won't be saved"),
            );
            Registry::in_memory()
        }
        None => Registry::in_memory(),
    };
    let neki = Arc::new(Neki::new(Config::from_env()));
    show_settings(ui, &neki, &registry);
    let state: Shared = Rc::new(RefCell::new(State {
        neki,
        registry,
        entries: Vec::new(),
        _watcher: slint::Timer::default(),
    }));
    refresh(&mut state.borrow_mut(), ui);
    connect(ui, &state);
    watch(ui, &state);
}

fn connect(ui: &AppWindow, state: &Shared) {
    let app = ui.global::<AppState>();

    // Builds a callback that receives the live window and the shared state.
    macro_rules! handler {
        (|$ui:ident, $state:ident $(, $arg:ident : $ty:ty)*| $body:block) => {{
            let weak = ui.as_weak();
            let state = Rc::clone(state);
            move |$($arg: $ty),*| {
                let Some($ui) = weak.upgrade() else { return };
                let $state = &state;
                $body
            }
        }};
    }

    app.on_refresh_projects(handler!(|ui, state| {
        refresh(&mut state.borrow_mut(), &ui);
        push_log(&ui, Level::Info, "Reloaded projects");
    }));
    app.on_select_project(handler!(|ui, state, index: i32| {
        select(&state.borrow(), &ui, index);
    }));
    app.on_add_project(handler!(|ui, state, path: SharedString| {
        add_project(&mut state.borrow_mut(), &ui, &path, false);
    }));
    app.on_create_project(handler!(|ui, state, path: SharedString| {
        add_project(&mut state.borrow_mut(), &ui, &path, true);
    }));
    app.on_remove_project(handler!(|ui, state, index: i32| {
        let mut state = state.borrow_mut();
        let Some(index) = index_in(index, state.entries.len()) else {
            return;
        };
        state.registry.remove(index);
        if let Err(err) = state.registry.save() {
            push_log(&ui, Level::Error, err);
        }
        ui.global::<AppState>().set_selected_project(-1);
        refresh(&mut state, &ui);
    }));
    app.on_reload_manifest(handler!(|ui, state| {
        select(
            &state.borrow(),
            &ui,
            ui.global::<AppState>().get_selected_project(),
        );
    }));
    app.on_save_manifest(handler!(|ui, state, text: SharedString| {
        save_manifest(&mut state.borrow_mut(), &ui, &text);
    }));
    app.on_build_project(handler!(|ui, state, package: SharedString| {
        let state = state.borrow();
        let Some(project) = selected_project(&state, &ui) else {
            return;
        };
        let store_dir = state.neki.config().store_dir.clone();
        let label = format!("Building {}#{package}", project.manifest.project.name);
        spawn_task(
            &ui,
            &state.neki,
            label,
            move |neki, events| neki.build(&project, Some(package.as_str()), events),
            move |ui, paths| {
                for path in paths {
                    push_log(
                        ui,
                        Level::Ok,
                        store_dir.join(path.to_string()).display().to_string(),
                    );
                }
            },
        );
    }));
    app.on_lock_project(handler!(|ui, state| {
        let state = state.borrow();
        let Some(project) = selected_project(&state, &ui) else {
            return;
        };
        let label = format!("Locking {}", project.manifest.project.name);
        spawn_task(
            &ui,
            &state.neki,
            label,
            move |neki, events| neki.lock(&project, &[], events),
            |ui, _| {
                push_log(ui, Level::Ok, "neki.lock updated");
                ui.global::<AppState>().invoke_refresh_projects();
            },
        );
    }));
    app.on_pick_folder(handler!(|ui, _state| {
        let picked = rfd::FileDialog::new()
            .set_title("Choose a project folder")
            .pick_folder();
        if let Some(folder) = picked {
            ui.global::<AppState>()
                .set_new_project_path(folder.display().to_string().into());
        }
    }));
    app.on_open_dev_shell(handler!(|ui, state| {
        let state = state.borrow();
        let Some(project) = selected_project(&state, &ui) else {
            return;
        };
        let label = format!("Opening the dev shell of {}", project.manifest.project.name);
        spawn_task(
            &ui,
            &state.neki,
            label,
            move |neki, events| neki.open_dev_shell(&project, events),
            |ui, ()| push_log(ui, Level::Ok, "Opened a terminal with the dev shell"),
        );
    }));
    app.on_select_build(handler!(|ui, _state, index: i32| {
        views::select_build(&ui, index);
    }));
    app.on_cancel_build(handler!(|ui, state| {
        state.borrow().neki.cancel();
        push_log(&ui, Level::Info, "Cancelling the build…");
    }));
    app.on_store_action(handler!(|ui, state, action: SharedString| {
        views::store_action(&ui, &state.borrow().neki, &action);
    }));
    app.on_load_store(handler!(|ui, state| {
        views::load_store(&ui, &state.borrow().neki);
    }));
    app.on_filter_store(handler!(|ui, _state, text: SharedString| {
        views::filter_store(&ui, &text);
    }));
    app.on_select_path(handler!(|ui, state, index: i32| {
        views::select_path(&ui, &state.borrow().neki, index);
    }));
    app.on_why_depends(handler!(|ui, state, target: SharedString| {
        views::why_depends(&ui, &state.borrow().neki, &target);
    }));
    app.on_profile_action(handler!(
        |ui, state, action: SharedString, profile: SharedString, packages: SharedString| {
            views::profile_action(
                &ui,
                &state.borrow().neki,
                &action,
                profile.to_string(),
                &packages,
            );
        }
    ));
    app.on_diff_generations(handler!(|ui, _state, a: i32, b: i32| {
        views::diff_generations(&ui, a, b);
    }));
    app.on_clear_log(handler!(|ui, _state| {
        ui.global::<AppState>()
            .set_log(ModelRc::new(VecModel::<LogEntry>::default()));
    }));
}

/// Every few seconds, update each project's build state in the list, so
/// edits made outside the app show up as "changed since the last build".
fn watch(ui: &AppWindow, state: &Shared) {
    let weak = ui.as_weak();
    let shared = Rc::downgrade(state);
    state.borrow()._watcher.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_secs(3),
        move || {
            let (Some(ui), Some(state)) = (weak.upgrade(), shared.upgrade()) else {
                return;
            };
            let Ok(state) = state.try_borrow() else {
                return;
            };
            let projects = ui.global::<AppState>().get_projects();
            for (i, entry) in state.entries.iter().enumerate() {
                if let Some(mut item) = projects.row_data(i) {
                    let build_state = build_state(&entry.root);
                    if item.build_state != build_state {
                        item.build_state = build_state.into();
                        projects.set_row_data(i, item);
                    }
                }
            }
        },
    );
}

/// Whether the project at `root` was built (it has a `result` link) and
/// whether any of its files changed after that.
fn build_state(root: &Path) -> &'static str {
    let Ok(link) = std::fs::symlink_metadata(root.join("result")) else {
        return "not built";
    };
    let Ok(built) = link.modified() else {
        return "built";
    };
    if newest_change(root, root, &mut 0).is_some_and(|t| t > built) {
        "changed since the last build"
    } else {
        "built"
    }
}

/// The newest modification time of the project's files, skipping build
/// results, version control data and dependency folders; at most a few
/// thousand files are looked at.
fn newest_change(root: &Path, dir: &Path, seen: &mut usize) -> Option<std::time::SystemTime> {
    const SKIP: &[&str] = &[".git", "target", "node_modules", "result"];
    let mut newest = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        *seen += 1;
        if *seen > 5000 {
            break;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if SKIP.contains(&name.as_ref()) || (dir == root && name.starts_with("result-")) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let time = if meta.is_dir() {
            newest_change(root, &entry.path(), seen)
        } else {
            meta.modified().ok()
        };
        newest = newest.max(time);
    }
    newest
}

/// Reload every registered project and redraw the list and the selection.
fn refresh(state: &mut State, ui: &AppWindow) {
    state.entries = state
        .registry
        .projects()
        .iter()
        .map(|root| Entry {
            root: root.clone(),
            project: Project::load(root).map_err(|err| err.to_string()),
        })
        .collect();
    let app = ui.global::<AppState>();
    app.set_projects(model(state.entries.iter().map(project_item).collect()));
    let selected = app.get_selected_project();
    if index_in(selected, state.entries.len()).is_some() {
        select(state, ui, selected);
    } else {
        app.set_selected_project(-1);
    }
}

/// Show entry `index` in the detail view, re-reading its `neki.toml`.
fn select(state: &State, ui: &AppWindow, index: i32) {
    let app = ui.global::<AppState>();
    let Some(entry) = index_in(index, state.entries.len()).map(|i| &state.entries[i]) else {
        app.set_selected_project(-1);
        return;
    };
    app.set_selected_project(index);
    app.set_detail(detail(entry));
    let text = std::fs::read_to_string(entry.root.join(MANIFEST_FILE)).unwrap_or_default();
    app.set_manifest_text(text.into());
    app.set_manifest_dirty(false);
    app.set_manifest_error(
        entry
            .project
            .as_ref()
            .err()
            .map(String::as_str)
            .unwrap_or_default()
            .into(),
    );
}

fn add_project(state: &mut State, ui: &AppWindow, path: &str, create: bool) {
    let path = path.trim();
    if path.is_empty() {
        return;
    }
    let mut root = match std::path::absolute(path) {
        Ok(root) => root,
        Err(err) => return push_log(ui, Level::Error, format!("{path}: {err}")),
    };
    if root.file_name().is_some_and(|name| name == MANIFEST_FILE) {
        root.pop();
    }
    if create {
        match state.neki.init_project(&root) {
            Ok(_) => push_log(
                ui,
                Level::Ok,
                format!("Created {}", root.join(MANIFEST_FILE).display()),
            ),
            Err(err) => return push_log(ui, Level::Error, err.to_string()),
        }
    } else if !root.join(MANIFEST_FILE).is_file() {
        let message = format!(
            "No {MANIFEST_FILE} in {}. Use \"Create new\" to start a project there.",
            root.display()
        );
        return push_log(ui, Level::Error, message);
    }

    let index = state.registry.add(root);
    if let Err(err) = state.registry.save() {
        push_log(ui, Level::Error, err);
    }
    let app = ui.global::<AppState>();
    app.set_new_project_path(SharedString::new());
    app.set_selected_project(i32::try_from(index).unwrap_or(-1));
    refresh(state, ui);
}

/// Validate `text` as a manifest and write it to the selected project.
fn save_manifest(state: &mut State, ui: &AppWindow, text: &str) {
    let app = ui.global::<AppState>();
    let Some(index) = index_in(app.get_selected_project(), state.entries.len()) else {
        return;
    };
    let path = state.entries[index].root.join(MANIFEST_FILE);
    if let Err(err) = Manifest::parse(text) {
        app.set_manifest_error(format!("Not saved: {err}").into());
        return push_log(
            ui,
            Level::Error,
            format!("{} not saved: invalid {MANIFEST_FILE}", path.display()),
        );
    }
    if let Err(err) = std::fs::write(&path, text) {
        return push_log(
            ui,
            Level::Error,
            format!("cannot write {}: {err}", path.display()),
        );
    }
    push_log(ui, Level::Ok, format!("Saved {}", path.display()));
    refresh(state, ui);
}

/// The selected project, if it loaded successfully.
fn selected_project(state: &State, ui: &AppWindow) -> Option<Project> {
    let index = index_in(
        ui.global::<AppState>().get_selected_project(),
        state.entries.len(),
    )?;
    match &state.entries[index].project {
        Ok(project) => Some(project.clone()),
        Err(err) => {
            push_log(
                ui,
                Level::Error,
                format!("Fix {MANIFEST_FILE} first: {err}"),
            );
            None
        }
    }
}

/// Run `work` on a worker thread, then `done` with its result on the UI
/// thread. One task runs at a time; `busy` disables the action buttons.
pub fn spawn_task<T, W, D>(
    ui: &AppWindow,
    neki: &Arc<Neki>,
    label: impl Into<String>,
    work: W,
    done: D,
) where
    T: Send + 'static,
    W: FnOnce(&Neki, &dyn EventSink) -> neki_core::Result<T> + Send + 'static,
    D: FnOnce(&AppWindow, T) + Send + 'static,
{
    let app = ui.global::<AppState>();
    if app.get_busy() {
        return push_log(ui, Level::Info, "Another task is still running");
    }
    let label = label.into();
    app.set_busy(true);
    app.set_busy_label(label.as_str().into());
    push_log(ui, Level::Info, label.as_str());

    let weak = ui.as_weak();
    let neki = Arc::clone(neki);
    std::thread::spawn(move || {
        let events = UiSink { ui: weak.clone() };
        let result = work(&neki, &events);
        // Fails only if the window is already gone.
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let app = ui.global::<AppState>();
            app.set_busy(false);
            app.set_busy_label(SharedString::new());
            match result {
                Ok(value) => done(&ui, value),
                Err(err) => push_log(&ui, Level::Error, format!("{label}: {err}")),
            }
        });
    });
}

/// Forwards backend events from worker threads to the activity log.
struct UiSink {
    ui: slint::Weak<AppWindow>,
}

impl EventSink for UiSink {
    fn event(&self, event: Event) {
        let tracked = event.clone();
        let _ = self
            .ui
            .upgrade_in_event_loop(move |ui| views::track_build(&ui, tracked));
        let (level, text) = match event {
            Event::BuildStarted { drv } => (Level::Info, format!("Building {drv}")),
            // Shown on the Builds page, not in the activity log.
            Event::BuildLog { .. } => return,
            Event::BuildFinished { drv, success: true } => (Level::Ok, format!("Built {drv}")),
            Event::BuildFinished {
                drv,
                success: false,
            } => (Level::Error, format!("Failed {drv}")),
            Event::Download { path, done: 0, .. } => (Level::Info, format!("Downloading {path}")),
            Event::Download { .. } => return,
            Event::Substituted { path } => (Level::Ok, format!("Downloaded {path}")),
            Event::Message(message) => (Level::Info, message),
        };
        let _ = self
            .ui
            .upgrade_in_event_loop(move |ui| push_log(&ui, level, text));
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Level {
    Info,
    Ok,
    Error,
}

/// Add an entry at the top of the activity log.
pub fn push_log(ui: &AppWindow, level: Level, text: impl Into<SharedString>) {
    let level = match level {
        Level::Info => "info",
        Level::Ok => "ok",
        Level::Error => "error",
    };
    let log = ui.global::<AppState>().get_log();
    if let Some(log) = log.as_any().downcast_ref::<VecModel<LogEntry>>() {
        log.insert(
            0,
            LogEntry {
                level: level.into(),
                text: text.into(),
            },
        );
        if log.row_count() > MAX_LOG {
            log.remove(MAX_LOG);
        }
    }
}

fn show_settings(ui: &AppWindow, neki: &Neki, registry: &Registry) {
    let config = neki.config();
    let app = ui.global::<AppState>();
    app.set_store_dir(config.store_dir.display().to_string().into());
    let substituters = if config.substituters.is_empty() {
        "(none)".to_string()
    } else {
        config.substituters.join(" ")
    };
    let project_list = registry.file().map_or_else(
        || "(not saved)".to_string(),
        |file| file.display().to_string(),
    );
    let keys = if config.trusted_public_keys.is_empty() {
        "(none)".to_string()
    } else {
        config.trusted_public_keys.join(" ")
    };
    let registry_entries: Vec<String> = config
        .registry
        .iter()
        .map(|(name, url)| format!("{name} = {url}"))
        .collect();
    let settings_file = neki_core::user_config_dir().map_or_else(
        || "(no config directory)".to_string(),
        |dir| dir.join("config.toml").display().to_string(),
    );
    let items = vec![
        kv("Store directory", config.store_dir.display().to_string()),
        kv("State directory", config.state_dir.display().to_string()),
        kv("System", config.system.as_str()),
        kv("Parallel builds", config.max_jobs.to_string()),
        kv("Substituters", substituters),
        kv("Trusted keys", keys),
        kv("Registry", registry_entries.join("\n")),
        kv("Settings file", settings_file),
        kv("Project list", project_list),
        kv("Version", env!("CARGO_PKG_VERSION")),
    ];
    app.set_settings(model(items));
}

fn project_item(entry: &Entry) -> ProjectItem {
    match &entry.project {
        Ok(project) => ProjectItem {
            name: project.manifest.project.name.as_str().into(),
            status: entry.root.display().to_string().into(),
            ok: true,
            build_state: build_state(&entry.root).into(),
        },
        Err(err) => ProjectItem {
            name: folder_name(&entry.root).into(),
            status: err.as_str().into(),
            ok: false,
            build_state: SharedString::new(),
        },
    }
}

fn detail(entry: &Entry) -> ProjectDetail {
    let root = entry.root.display().to_string().into();
    let project = match &entry.project {
        Ok(project) => project,
        Err(err) => {
            return ProjectDetail {
                name: folder_name(&entry.root).into(),
                description: format!("Cannot load this project: {err}").into(),
                root,
                ..ProjectDetail::default()
            };
        }
    };
    let manifest = &project.manifest;
    let locked = |name: &str| project.lock.as_ref().and_then(|lock| lock.inputs.get(name));

    let inputs = manifest
        .inputs
        .iter()
        .map(|(name, input)| {
            let status = match locked(name) {
                Some(entry) => match &entry.rev {
                    Some(rev) => format!("locked at {rev}"),
                    None => "locked".to_string(),
                },
                None => "not locked".to_string(),
            };
            kv(name.as_str(), format!("{} ({status})", input.url))
        })
        .collect();
    let packages = manifest
        .packages
        .iter()
        .map(|(name, package)| {
            let build: Vec<String> = package.build.iter().map(|argv| argv.join(" ")).collect();
            let recipe = match &package.file {
                Some(file) => format!("defined in {file}"),
                None => list(&build),
            };
            let value = format!(
                "src: {}\ninputs: {}\nbuild: {recipe}",
                package.src,
                list(&package.build_inputs),
            );
            kv(name.as_str(), value)
        })
        .collect();
    let mut dev_shell = Vec::new();
    if let Some(shell) = &manifest.dev_shell {
        dev_shell.push(kv("packages", list(&shell.packages)));
        dev_shell.extend(
            shell
                .env
                .iter()
                .map(|(key, value)| kv(format!("${key}"), value.as_str())),
        );
        if let Some(hook) = &shell.shell_hook {
            dev_shell.push(kv("shell hook", hook.as_str()));
        }
    }
    let locked_count = manifest
        .inputs
        .keys()
        .filter(|name| locked(name).is_some())
        .count();
    let lock_status = match &project.lock {
        Some(_) => format!(
            "neki.lock pins {locked_count} of {} inputs.",
            manifest.inputs.len()
        ),
        None => "No neki.lock yet. Use \"Update lock\" to create it.".to_string(),
    };

    ProjectDetail {
        name: manifest.project.name.as_str().into(),
        description: manifest
            .project
            .description
            .as_deref()
            .unwrap_or_default()
            .into(),
        root,
        lock_status: lock_status.into(),
        package_names: model(
            manifest
                .packages
                .keys()
                .map(|name| name.as_str().into())
                .collect(),
        ),
        inputs: model(inputs),
        packages: model(packages),
        dev_shell: model(dev_shell),
    }
}

fn index_in(index: i32, len: usize) -> Option<usize> {
    usize::try_from(index).ok().filter(|&index| index < len)
}

fn folder_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Comma-separated items, or `(none)`.
fn list(items: &[String]) -> String {
    if items.is_empty() {
        "(none)".to_string()
    } else {
        items.join(", ")
    }
}

pub fn kv(key: impl Into<SharedString>, value: impl Into<SharedString>) -> KeyValue {
    KeyValue {
        key: key.into(),
        value: value.into(),
    }
}

pub fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}
