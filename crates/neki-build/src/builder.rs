//! Running one derivation's builder and registering its outputs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use neki_derivation::{Derivation, DerivationOutput};
use neki_hash::StorePath;
use neki_store::{LocalStore, Store};

use crate::{BuildEvent, BuildOptions, Error, Events, Result};

/// Returns whether the builder ran (false if another process built the
/// outputs while this one waited for their locks).
pub fn build(
    store: &LocalStore,
    drv_path: &StorePath,
    drv: &Derivation,
    all: &BTreeMap<StorePath, Derivation>,
    opts: &BuildOptions,
    stop: &(dyn Fn() -> bool + Sync),
    events: Events,
) -> Result<bool> {
    let outputs = drv.output_paths(store.store_dir())?;
    // In path order, so two processes can't each hold one the other wants.
    let _locks = outputs
        .values()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|path| store.lock_path(path))
        .collect::<Result<Vec<_>, _>>()?;
    let mut missing = false;
    for path in outputs.values() {
        if !store.is_valid_path(path)? {
            store.clear_invalid(path)?;
            missing = true;
        }
    }
    if !missing {
        return Ok(false);
    }

    let name = drv.name()?;
    let log_dir = store.state_dir().join("log");
    fs::create_dir_all(&log_dir).map_err(at(&log_dir))?;
    let log_path = log_dir.join(format!("{}-{name}.log", drv_path.hash_part()));
    let log = Logger {
        file: Mutex::new(File::create(&log_path).map_err(at(&log_path))?),
        drv: drv_path.clone(),
        events,
    };
    let build_dir = store
        .state_dir()
        .join("builds")
        .join(format!("{}-{name}", drv_path.hash_part()));
    neki_platform::fs::remove_tree(&build_dir).map_err(at(&build_dir))?;

    events(BuildEvent::Started {
        drv: drv_path.clone(),
    });
    let result = run_builder(drv, &build_dir, opts, stop, &log)
        .and_then(|()| check_outputs(store, drv, &outputs))
        .and_then(|()| {
            // References can only be to inputs and to the outputs themselves.
            let mut inputs: Vec<StorePath> = drv.input_srcs.iter().cloned().collect();
            for (input, wanted) in &drv.input_drvs {
                let paths = all[input].output_paths(store.store_dir())?;
                inputs.extend(wanted.iter().filter_map(|o| paths.get(o).cloned()));
            }
            let candidates = store.closure(&inputs)?;
            let paths: Vec<StorePath> = outputs.values().cloned().collect();
            store.register_built(&paths, Some(drv_path), &candidates, !drv.is_fixed_output())?;
            Ok(())
        });
    let success = result.is_ok();
    events(BuildEvent::Finished {
        drv: drv_path.clone(),
        success,
    });
    match result {
        Ok(()) => {
            let _ = neki_platform::fs::remove_tree(&build_dir);
            Ok(true)
        }
        Err(err) => {
            for path in outputs.values() {
                let _ = store.clear_invalid(path);
            }
            if !opts.keep_failed {
                let _ = neki_platform::fs::remove_tree(&build_dir);
            }
            match err {
                Error::Cancelled => Err(Error::Cancelled),
                err => {
                    log.line(&format!("error: {err}"));
                    Err(Error::BuildFailed {
                        drv: drv_path.to_string(),
                        reason: err.to_string(),
                        log: log_path,
                    })
                }
            }
        }
    }
}

/// Writes builder output to the log file and the event callback.
struct Logger<'a> {
    file: Mutex<File>,
    drv: StorePath,
    events: Events<'a>,
}

impl Logger<'_> {
    fn line(&self, line: &str) {
        if let Ok(mut file) = self.file.lock() {
            let _ = writeln!(file, "{line}");
        }
        (self.events)(BuildEvent::Log {
            drv: self.drv.clone(),
            line: line.to_string(),
        });
    }

    /// Log every line read from `reader` until it ends.
    fn lines(&self, reader: impl Read) {
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        while reader.read_until(b'\n', &mut buf).is_ok_and(|n| n > 0) {
            let line = String::from_utf8_lossy(&buf);
            self.line(line.trim_end_matches(['\n', '\r']));
            buf.clear();
        }
    }
}

fn run_builder(
    drv: &Derivation,
    build_dir: &Path,
    opts: &BuildOptions,
    stop: &(dyn Fn() -> bool + Sync),
    log: &Logger,
) -> Result<()> {
    for dir in ["tmp", "home"] {
        let dir = build_dir.join(dir);
        fs::create_dir_all(&dir).map_err(at(&dir))?;
    }
    let env = build_env(drv, build_dir, opts)?;
    let out = || {
        env.get("out")
            .map(PathBuf::from)
            .ok_or_else(|| invalid("no `out` output"))
    };
    let strip_root = env.get("stripRoot").is_some_and(|v| v == "1");
    match drv.builder.as_str() {
        "builtin:fetchurl" => {
            if !drv.is_fixed_output() {
                return Err(invalid("builtin:fetchurl needs a fixed output hash"));
            }
            let url = env.get("url").ok_or_else(|| invalid("no `url`"))?;
            log.line(&format!("downloading {url}"));
            if env.get("unpack").is_some_and(|v| v == "1") {
                let archive = build_dir.join("download");
                neki_fetch::download(url, &archive, &|_, _| {})?;
                neki_fetch::unpack(&archive, &out()?, strip_root)?;
            } else {
                neki_fetch::download(url, &out()?, &|_, _| {})?;
            }
            Ok(())
        }
        "builtin:unpack" => crate::builtins::unpack(&env, &out()?, build_dir),
        "builtin:assemble" => crate::builtins::assemble(&env, &out()?),
        "builtin:run" => {
            let dir = match env.get("src") {
                Some(src) => {
                    let dir = build_dir.join("source");
                    neki_store::copy_tree(Path::new(src), &dir)?;
                    dir
                }
                None => build_dir.to_path_buf(),
            };
            let commands: Vec<Vec<String>> = serde_json::from_str(
                env.get("commands").map_or("[]", String::as_str),
            )
            .map_err(|err| {
                invalid(&format!(
                    "`commands` is not a JSON list of argument lists: {err}"
                ))
            })?;
            for argv in commands {
                let argv = argv
                    .iter()
                    .map(|arg| expand(arg, &env))
                    .collect::<Result<Vec<_>>>()?;
                let Some((program, args)) = argv.split_first() else {
                    continue;
                };
                log.line(&format!("$ {}", argv.join(" ")));
                let mut cmd = Command::new(resolve_program(program, &dir));
                cmd.args(args);
                spawn(cmd, &env, &dir, stop, log)?;
            }
            Ok(())
        }
        builtin if builtin.starts_with("builtin:") => {
            Err(invalid(&format!("unknown builder {builtin}")))
        }
        program => {
            let mut cmd = Command::new(program);
            cmd.args(&drv.args);
            spawn(cmd, &env, build_dir, stop, log)
        }
    }
}

/// The builder's complete environment: the derivation's variables, private
/// temp and home directories, and `PATH` from `buildInputs`.
fn build_env(
    drv: &Derivation,
    build_dir: &Path,
    opts: &BuildOptions,
) -> Result<BTreeMap<String, String>> {
    let mut env: BTreeMap<String, String> = neki_platform::process::essential_env()
        .into_iter()
        .collect();
    env.extend(drv.env.clone());
    let tmp = build_dir.join("tmp").display().to_string();
    for name in ["TMPDIR", "TEMP", "TMP", "TEMPDIR"] {
        env.insert(name.into(), tmp.clone());
    }
    env.insert("HOME".into(), build_dir.join("home").display().to_string());
    env.insert("NEKI_BUILD_TOP".into(), build_dir.display().to_string());
    env.insert("NEKI_BUILD_CORES".into(), opts.max_jobs.max(1).to_string());
    let bins: Vec<PathBuf> = drv
        .env
        .get("buildInputs")
        .map(|inputs| {
            inputs
                .split_whitespace()
                .flat_map(|p| neki_store::bin_dirs(Path::new(p)))
                .collect()
        })
        .unwrap_or_default();
    let path = std::env::join_paths(bins)
        .map_err(|err| invalid(&format!("buildInputs: {err}")))?
        .to_string_lossy()
        .into_owned();
    env.insert("PATH".into(), path);
    Ok(env)
}

/// Expand `${NAME}` from `env` and `$$` to `$` in `arg`.
fn expand(arg: &str, env: &BTreeMap<String, String>) -> Result<String> {
    let mut out = String::new();
    let mut rest = arg;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        if let Some(after) = rest.strip_prefix("$$") {
            out.push('$');
            rest = after;
        } else if let Some(after) = rest.strip_prefix("${") {
            let end = after
                .find('}')
                .ok_or_else(|| invalid(&format!("unclosed ${{ in {arg:?}")))?;
            let name = &after[..end];
            let value = env
                .get(name)
                .ok_or_else(|| invalid(&format!("{arg:?} uses ${{{name}}}, which is not set")))?;
            out.push_str(value);
            rest = &after[end + 1..];
        } else {
            out.push('$');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// A relative program path with a directory part is relative to `dir` (the
/// command's working directory) on every OS; bare names are searched in
/// `PATH`.
fn resolve_program(program: &str, dir: &Path) -> PathBuf {
    let path = Path::new(program);
    if path.is_relative() && path.components().count() > 1 {
        dir.join(path)
    } else {
        path.to_path_buf()
    }
}

/// Run `cmd` with exactly `env` in `dir`, logging its output, until it
/// exits or `stop` says to give up.
fn spawn(
    mut cmd: Command,
    env: &BTreeMap<String, String>,
    dir: &Path,
    stop: &(dyn Fn() -> bool + Sync),
    log: &Logger,
) -> Result<()> {
    cmd.env_clear()
        .envs(env)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut tree = neki_platform::process::spawn_tree(cmd)
        .map_err(|err| invalid(&format!("cannot run {program}: {err}")))?;
    let (stdout, stderr) = (tree.take_stdout(), tree.take_stderr());
    std::thread::scope(|scope| {
        if let Some(stdout) = stdout {
            scope.spawn(|| log.lines(stdout));
        }
        if let Some(stderr) = stderr {
            scope.spawn(|| log.lines(stderr));
        }
        let status = loop {
            if stop() {
                tree.kill();
                return Err(Error::Cancelled);
            }
            match tree.try_wait()? {
                Some(status) => break status,
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        // Kill what it left running, which also closes the output pipes.
        tree.wait()?;
        if status.success() {
            Ok(())
        } else {
            Err(invalid(&format!("{program} exited with {status}")))
        }
    })
}

/// Check that the builder created every output, and that fixed outputs
/// have their declared hash.
fn check_outputs(
    store: &LocalStore,
    drv: &Derivation,
    outputs: &BTreeMap<String, StorePath>,
) -> Result<()> {
    for (name, path) in outputs {
        let location = store.store_dir().to_path(path);
        if fs::symlink_metadata(&location).is_err() {
            return Err(invalid(&format!(
                "the builder did not create output `{name}`"
            )));
        }
        if let Some(DerivationOutput::Fixed { hash, recursive }) = drv.outputs.get(name) {
            let got = if *recursive {
                let mut hasher = neki_hash::Hasher::new(hash.algo());
                neki_nar::dump(&location, &mut std::io::BufWriter::new(&mut hasher))?;
                hasher.finish()
            } else {
                neki_store::file_hash(&location, hash.algo())?
            };
            if got != *hash {
                return Err(neki_store::Error::HashMismatch {
                    what: format!("output `{name}`"),
                    expected: hash.to_sri(),
                    got: got.to_sri(),
                }
                .into());
            }
        }
    }
    Ok(())
}

fn invalid(reason: &str) -> Error {
    Error::Builder(reason.to_string())
}

fn at(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::File {
        path: path.to_path_buf(),
        source,
    }
}
