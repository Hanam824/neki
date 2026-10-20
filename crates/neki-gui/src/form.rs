//! The Form tab: the common fields of a `neki.toml`, read from the editor
//! text and written back into it. Only fields that changed are rewritten,
//! so comments and layout everywhere else stay as they were.

use neki_core::Manifest;
use slint::{ComponentHandle, Model, SharedString};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

use crate::app::model;
use crate::{AppState, AppWindow, InputRow, PackageRow};

/// The fields the form edits.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Form {
    pub name: String,
    pub description: String,
    /// Input name and URL.
    pub inputs: Vec<(String, String)>,
    pub dev_packages: Vec<String>,
    pub dev_env: Vec<(String, String)>,
    pub packages: Vec<PackageForm>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PackageForm {
    pub name: String,
    pub src: String,
    pub build_inputs: Vec<String>,
    /// Empty if the package is built with `build` commands.
    pub file: String,
}

fn strings(item: Option<&Item>) -> Vec<String> {
    item.and_then(Item::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn string(item: Option<&Item>) -> String {
    item.and_then(Item::as_str).unwrap_or_default().to_string()
}

/// The form's fields in `text`.
pub fn read(text: &str) -> Result<Form, String> {
    let doc: DocumentMut = text.parse().map_err(|e| format!("{e}"))?;
    let project = doc.get("project");
    let mut form = Form {
        name: string(project.and_then(|p| p.get("name"))),
        description: string(project.and_then(|p| p.get("description"))),
        ..Form::default()
    };
    if let Some(inputs) = doc.get("inputs").and_then(Item::as_table_like) {
        for (name, input) in inputs.iter() {
            let url = input.get("url").and_then(Item::as_str).unwrap_or_default();
            form.inputs.push((name.to_string(), url.to_string()));
        }
    }
    if let Some(shell) = doc.get("dev-shell") {
        form.dev_packages = strings(shell.get("packages"));
        if let Some(env) = shell.get("env").and_then(Item::as_table_like) {
            for (key, value) in env.iter() {
                form.dev_env.push((
                    key.to_string(),
                    value.as_str().unwrap_or_default().to_string(),
                ));
            }
        }
    }
    if let Some(packages) = doc.get("packages").and_then(Item::as_table_like) {
        for (name, package) in packages.iter() {
            form.packages.push(PackageForm {
                name: name.to_string(),
                src: package
                    .get("src")
                    .and_then(Item::as_str)
                    .unwrap_or(".")
                    .to_string(),
                build_inputs: strings(package.get("build-inputs")),
                file: string(package.get("file")),
            });
        }
    }
    Ok(form)
}

/// Set `key` in `table` to `value` unless it already has that value, so
/// unchanged entries keep their formatting.
fn set(table: &mut dyn toml_edit::TableLike, key: &str, value: Value) {
    let same = table
        .get(key)
        .and_then(Item::as_value)
        .is_some_and(|old| old.to_string().trim() == value.to_string().trim());
    if !same {
        table.insert(key, Item::Value(value));
    }
}

fn array(items: &[String]) -> Value {
    Value::Array(items.iter().map(String::as_str).collect::<Array>())
}

/// A table (`[name]`) in `parent`, created if missing.
fn table<'a>(parent: &'a mut Table, name: &str) -> &'a mut dyn toml_edit::TableLike {
    if parent.get(name).and_then(Item::as_table_like).is_none() {
        parent.insert(name, Item::Table(Table::new()));
    }
    parent
        .get_mut(name)
        .and_then(Item::as_table_like_mut)
        .expect("just made sure it's a table")
}

/// `text` with the form's fields written into it, checked as a manifest.
pub fn apply(text: &str, form: &Form) -> Result<String, String> {
    let mut doc: DocumentMut = text.parse().map_err(|e| format!("{e}"))?;
    let root = doc.as_table_mut();

    let project = table(root, "project");
    set(project, "name", form.name.as_str().into());
    if form.description.is_empty() {
        project.remove("description");
    } else {
        set(project, "description", form.description.as_str().into());
    }

    let inputs = table(root, "inputs");
    let wanted: Vec<&str> = form.inputs.iter().map(|(n, _)| n.as_str()).collect();
    let gone: Vec<String> = inputs
        .iter()
        .map(|(k, _)| k.to_string())
        .filter(|k| !wanted.contains(&k.as_str()))
        .collect();
    for name in gone {
        inputs.remove(&name);
    }
    for (name, url) in &form.inputs {
        if name.is_empty() {
            return Err("an input has no name".into());
        }
        match inputs.get_mut(name).and_then(Item::as_table_like_mut) {
            Some(input) => set(input, "url", url.as_str().into()),
            None => {
                let mut input = InlineTable::new();
                input.insert("url", url.as_str().into());
                inputs.insert(name, Item::Value(Value::InlineTable(input)));
            }
        }
    }

    let has_shell = root.contains_key("dev-shell");
    if has_shell || !form.dev_packages.is_empty() || !form.dev_env.is_empty() {
        let shell = table(root, "dev-shell");
        set(shell, "packages", array(&form.dev_packages));
        let mut env = InlineTable::new();
        for (key, value) in &form.dev_env {
            env.insert(key, value.as_str().into());
        }
        if form.dev_env.is_empty() {
            shell.remove("env");
        } else {
            set(shell, "env", Value::InlineTable(env));
        }
    }

    for package in &form.packages {
        let packages = table(root, "packages");
        let Some(decl) = packages
            .get_mut(&package.name)
            .and_then(Item::as_table_like_mut)
        else {
            continue;
        };
        if package.src != "." || decl.contains_key("src") {
            set(decl, "src", package.src.as_str().into());
        }
        if !package.build_inputs.is_empty() || decl.contains_key("build-inputs") {
            set(decl, "build-inputs", array(&package.build_inputs));
        }
        if package.file.is_empty() {
            decl.remove("file");
        } else {
            set(decl, "file", package.file.as_str().into());
        }
    }

    let text = doc.to_string();
    Manifest::parse(&text).map_err(|e| e.to_string())?;
    Ok(text)
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(String::from).collect()
}

/// Fill the Form tab from the editor text.
pub fn load(ui: &AppWindow) {
    let app = ui.global::<AppState>();
    let form = match read(&app.get_manifest_text()) {
        Ok(form) => form,
        Err(err) => {
            app.set_form_error(format!("Fix neki.toml first: {err}").into());
            return;
        }
    };
    app.set_form_error(SharedString::new());
    app.set_form_name(form.name.into());
    app.set_form_description(form.description.into());
    app.set_form_inputs(model(
        form.inputs
            .into_iter()
            .map(|(name, url)| InputRow {
                name: name.into(),
                url: url.into(),
            })
            .collect(),
    ));
    app.set_form_dev_packages(form.dev_packages.join(" ").into());
    let env: Vec<String> = form
        .dev_env
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    app.set_form_dev_env(env.join("\n").into());
    app.set_form_packages(model(
        form.packages
            .into_iter()
            .map(|p| PackageRow {
                name: p.name.into(),
                src: p.src.into(),
                build_inputs: p.build_inputs.join(" ").into(),
                file: p.file.into(),
            })
            .collect(),
    ));
}

/// Change input row `index`, or add one if it's -1.
pub fn change_input(ui: &AppWindow, index: i32, name: &str, url: &str) {
    let inputs = ui.global::<AppState>().get_form_inputs();
    let row = InputRow {
        name: name.into(),
        url: url.into(),
    };
    match usize::try_from(index)
        .ok()
        .filter(|&i| i < inputs.row_count())
    {
        Some(i) => inputs.set_row_data(i, row),
        None => {
            let mut rows: Vec<InputRow> = inputs.iter().collect();
            rows.push(row);
            ui.global::<AppState>().set_form_inputs(model(rows));
        }
    }
}

pub fn remove_input(ui: &AppWindow, index: i32) {
    let app = ui.global::<AppState>();
    let mut rows: Vec<InputRow> = app.get_form_inputs().iter().collect();
    if let Some(i) = usize::try_from(index).ok().filter(|&i| i < rows.len()) {
        rows.remove(i);
        app.set_form_inputs(model(rows));
    }
}

pub fn change_package(ui: &AppWindow, index: i32, src: &str, build_inputs: &str, file: &str) {
    let packages = ui.global::<AppState>().get_form_packages();
    let Some(i) = usize::try_from(index).ok() else {
        return;
    };
    if let Some(row) = packages.row_data(i) {
        packages.set_row_data(
            i,
            PackageRow {
                src: src.into(),
                build_inputs: build_inputs.into(),
                file: file.into(),
                ..row
            },
        );
    }
}

/// Write the Form tab's fields into the editor text, which then needs saving.
pub fn apply_to_text(ui: &AppWindow) {
    let app = ui.global::<AppState>();
    let mut dev_env = Vec::new();
    for line in app.get_form_dev_env().lines().map(str::trim) {
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            app.set_form_error(format!("Not KEY=value: {line}").into());
            return;
        };
        dev_env.push((key.trim().to_string(), value.trim().to_string()));
    }
    let form = Form {
        name: app.get_form_name().trim().to_string(),
        description: app.get_form_description().trim().to_string(),
        inputs: app
            .get_form_inputs()
            .iter()
            .map(|r| (r.name.trim().to_string(), r.url.trim().to_string()))
            .collect(),
        dev_packages: words(&app.get_form_dev_packages()),
        dev_env,
        packages: app
            .get_form_packages()
            .iter()
            .map(|r| PackageForm {
                name: r.name.to_string(),
                src: r.src.trim().to_string(),
                build_inputs: words(&r.build_inputs),
                file: r.file.trim().to_string(),
            })
            .collect(),
    };
    let old = app.get_manifest_text();
    match apply(&old, &form) {
        Ok(text) => {
            app.set_form_error(SharedString::new());
            if text != old.as_str() {
                app.set_manifest_text(text.into());
                app.set_manifest_dirty(true);
            }
        }
        Err(err) => app.set_form_error(format!("Not applied: {err}").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = r#"[project]
name = "app"

# Package sources.
[inputs]
pkgs = { url = "path:../pkgs" }   # the local set
old = { url = "github:a/b" }

[dev-shell]
packages = ["pkgs.jq"]

[packages.default]
src = "."
build = [["make"]]
"#;

    #[test]
    fn reads_the_fields() {
        let form = read(TEXT).unwrap();
        assert_eq!(form.name, "app");
        assert_eq!(form.inputs[0], ("pkgs".into(), "path:../pkgs".into()));
        assert_eq!(form.dev_packages, ["pkgs.jq"]);
        assert_eq!(form.packages[0].name, "default");
    }

    #[test]
    fn edits_keep_comments_and_layout() {
        let mut form = read(TEXT).unwrap();
        form.description = "An app".into();
        form.inputs.retain(|(n, _)| n != "old");
        form.inputs.push(("tools".into(), "github:c/d".into()));
        form.dev_packages.push("pkgs.ripgrep".into());
        form.dev_env.push(("MODE".into(), "dev".into()));
        form.packages[0].build_inputs.push("pkgs.python".into());
        let text = apply(TEXT, &form).unwrap();
        assert!(text.contains("# Package sources."), "{text}");
        assert!(
            text.contains(r#"pkgs = { url = "path:../pkgs" }   # the local set"#),
            "{text}"
        );
        assert!(!text.contains("old ="));
        assert!(text.contains(r#"tools = { url = "github:c/d" }"#), "{text}");
        assert!(text.contains(r#"description = "An app""#));
        assert!(text.contains(r#"build = [["make"]]"#));
        assert_eq!(read(&text).unwrap(), form);
        // Nothing changed: the text stays exactly the same.
        assert_eq!(apply(TEXT, &read(TEXT).unwrap()).unwrap(), TEXT);
    }

    #[test]
    fn invalid_edits_are_rejected() {
        let mut form = read(TEXT).unwrap();
        form.inputs.push((String::new(), "x".into()));
        assert!(apply(TEXT, &form).is_err());
        let mut form = read(TEXT).unwrap();
        form.packages[0].file = "package.neki".into();
        // `file` is fine; a manifest the parser rejects isn't.
        assert!(apply(TEXT, &form).is_ok());
        assert!(read("[project\n").is_err());
    }

    /// The Form tab in the real window: edits go into the editor text.
    #[test]
    fn form_tab_edits_the_text() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let app = ui.global::<AppState>();
        app.set_manifest_text(TEXT.into());
        load(&ui);
        assert_eq!(app.get_form_inputs().row_count(), 2);
        assert_eq!(app.get_form_dev_packages(), "pkgs.jq");

        change_input(&ui, -1, "tools", "github:c/d");
        remove_input(&ui, 1);
        change_package(&ui, 0, ".", "pkgs.python", "");
        app.set_form_dev_env("MODE = dev\n".into());
        apply_to_text(&ui);
        let text = app.get_manifest_text();
        assert!(app.get_manifest_dirty());
        assert_eq!(app.get_form_error(), "");
        assert!(text.contains("# the local set"), "{text}");
        assert!(text.contains(r#"tools = { url = "github:c/d" }"#), "{text}");
        assert!(!text.contains("old ="), "{text}");
        assert!(text.contains(r#"build-inputs = ["pkgs.python"]"#), "{text}");
        assert!(text.contains(r#"env = { MODE = "dev" }"#), "{text}");

        app.set_form_dev_env("no equals sign".into());
        apply_to_text(&ui);
        assert!(app.get_form_error().contains("KEY=value"));
    }
}
