//! The `neki` command-line interface: argument parsing and output only;
//! every operation goes through `neki-core`. With `--json`, every command
//! prints one JSON value on stdout; with `--log-format json`, progress on
//! stderr is one JSON object per line.

use std::path::{Path, PathBuf};
use std::process::{ExitCode, ExitStatus};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use neki_core::{
    Config, Event, EventSink, GcOptions, Generation, Neki, PathInfo, Project, Shell, StorePath,
};
use serde_json::{Value, json};

#[derive(Debug, Parser)]
#[command(
    name = "neki",
    version,
    about = "Declarative, reproducible package management"
)]
struct Cli {
    /// Run as if started in this directory.
    #[arg(short = 'C', long, global = true, value_name = "DIR")]
    dir: Option<PathBuf>,
    /// Print the result as JSON.
    #[arg(long, global = true)]
    json: bool,
    /// How to print progress on stderr.
    #[arg(long, global = true, value_enum, default_value_t = LogFormat::Text)]
    log_format: LogFormat,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum LogFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ShellKind {
    Posix,
    Powershell,
}

impl From<ShellKind> for Shell {
    fn from(kind: ShellKind) -> Shell {
        match kind {
            ShellKind::Posix => Shell::Posix,
            ShellKind::Powershell => Shell::PowerShell,
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a new project (neki.toml) in the current or `-C` directory.
    Init,
    /// Show the configuration and the current project.
    Info,
    /// Build a package of the current project (or of a .neki file).
    Build {
        /// Package to build (default: `default`).
        package: Option<String>,
        /// Build from this .neki file instead of the project.
        #[arg(short, long, conflicts_with_all = ["package", "drv"])]
        file: Option<PathBuf>,
        /// With --file: the attribute to build (default: the whole file).
        #[arg(short = 'A', long, requires = "file")]
        attr: Option<String>,
        /// Build these store derivations (.drv paths) directly.
        #[arg(long, num_args = 1.., conflicts_with = "package")]
        drv: Vec<String>,
    },
    /// Build a package and run it.
    Run {
        /// Package to run (default: `default`).
        package: Option<String>,
        /// Arguments passed to the program.
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Enter the project's development shell.
    Develop {
        /// Print a script that sets up the environment instead, e.g.
        /// `eval "$(neki develop --print-env posix)"`.
        #[arg(long, value_enum, value_name = "SHELL")]
        print_env: Option<ShellKind>,
    },
    /// Start a shell with the given packages (`<input>.<package>`) available.
    Shell {
        #[arg(required = true)]
        packages: Vec<String>,
        /// Print a script that sets up the environment instead. (The
        /// environment is only kept from the garbage collector while a
        /// shell started by `neki shell` runs.)
        #[arg(long, value_enum, value_name = "SHELL")]
        print_env: Option<ShellKind>,
    },
    /// Create or update neki.lock.
    Lock {
        /// Re-resolve this input (repeatable).
        #[arg(long = "update", value_name = "INPUT")]
        update: Vec<String>,
    },
    /// Evaluate an expression and print the result.
    Eval {
        /// The expression, e.g. `(import ./pkgs).hello.name`.
        expr: String,
    },
    /// Manage packages installed in a profile.
    #[command(subcommand)]
    Profile(ProfileCommand),
    /// Inspect and maintain the store.
    #[command(subcommand)]
    Store(StoreCommand),
    /// Compute content hashes.
    #[command(subcommand)]
    Hash(HashCommand),
    /// Work with binary caches.
    #[command(subcommand)]
    Cache(CacheCommand),
}

#[derive(Debug, Args)]
struct ProfileArg {
    /// Profile to operate on.
    #[arg(long, default_value = "default")]
    profile: String,
}

#[derive(Debug, Subcommand)]
enum ProfileCommand {
    /// Install packages (`<registry input>.<package>`) into the profile.
    Install {
        #[arg(required = true)]
        packages: Vec<String>,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Remove packages from the profile.
    Remove {
        #[arg(required = true)]
        packages: Vec<String>,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Rebuild the profile's packages from the newest inputs.
    Upgrade {
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Switch back to the previous generation.
    Rollback {
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// Switch to a generation by number.
    Switch {
        generation: u32,
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// List the profile's generations.
    History {
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// List the profiles.
    List,
}

#[derive(Debug, Subcommand)]
enum StoreCommand {
    /// Copy a file or directory into the store.
    Add { path: PathBuf },
    /// Show metadata of a store path (`<hash>-<name>` or its full location).
    PathInfo { path: String },
    /// List every valid store path.
    Ls,
    /// List a path and everything it refers to.
    Closure { path: String },
    /// Show why `from` depends on `to`.
    WhyDepends { from: String, to: String },
    /// List the GC roots.
    Roots,
    /// Check that store contents match their recorded hashes.
    Verify,
    /// Delete store paths that are not reachable from any GC root.
    Gc {
        /// Only show what would be deleted.
        #[arg(long)]
        dry_run: bool,
        /// First delete profile generations older than this, e.g. `30d`.
        #[arg(long, value_name = "AGE", value_parser = parse_age)]
        delete_older_than: Option<Duration>,
    },
    /// Replace identical files in the store with hard links.
    Optimise,
    /// Serve the store as a binary cache over HTTP.
    Serve {
        #[arg(long, default_value = "127.0.0.1:8080")]
        listen: String,
        /// Sign paths with this secret key.
        #[arg(long)]
        key_file: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum HashCommand {
    /// Hash a file or directory (NAR serialization).
    Path { path: PathBuf },
    /// Hash the contents of a single file.
    File { path: PathBuf },
}

#[derive(Debug, Subcommand)]
enum CacheCommand {
    /// Upload store paths and their closures to a file:// binary cache.
    Push {
        url: String,
        #[arg(required = true)]
        paths: Vec<String>,
        /// Sign with this secret key (default: `secret-key-file` setting).
        #[arg(long)]
        key_file: Option<PathBuf>,
    },
    /// Download store paths and their closures from a binary cache.
    Fetch {
        url: String,
        #[arg(required = true)]
        paths: Vec<String>,
    },
    /// Create a signing key; prints the public key to add to
    /// `trusted-public-keys` where the cache is used.
    Keygen {
        /// Key name, e.g. `cache.example.org-1`.
        name: String,
        /// Where to write the secret key.
        #[arg(long)]
        secret_file: PathBuf,
    },
}

/// `30d`, `12h`, `45m`, `10s` or a number of days.
fn parse_age(s: &str) -> Result<Duration, String> {
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (number, unit) = s.split_at(split);
    let n: u64 = number
        .parse()
        .map_err(|_| format!("{s:?} is not an age like 30d"))?;
    let seconds = match unit {
        "" | "d" => n * 86400,
        "h" => n * 3600,
        "m" => n * 60,
        "s" => n,
        _ => return Err(format!("{s:?}: use d, h, m or s")),
    };
    Ok(Duration::from_secs(seconds))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    match run(cli) {
        Ok(code) => code,
        Err(err) => {
            if json {
                println!("{}", json!({ "error": err.to_string() }));
            } else {
                // Plain `{err}`: Neki's error messages already include their
                // cause, so printing the source chain (`{err:#}`) would repeat it.
                eprintln!("error: {err}");
            }
            ExitCode::FAILURE
        }
    }
}

/// What a command produced: printed as text, or as JSON with `--json`.
struct Output {
    text: Vec<String>,
    json: Value,
}

impl Output {
    fn new(text: impl Into<String>, json: Value) -> Self {
        let text = text.into();
        Self {
            text: if text.is_empty() {
                Vec::new()
            } else {
                vec![text]
            },
            json,
        }
    }

    fn lines(text: Vec<String>, json: Value) -> Self {
        Self { text, json }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let neki = Neki::new(Config::from_env());
    let dir = match cli.dir {
        Some(dir) => dir,
        None => std::env::current_dir()?,
    };
    let events = Sink {
        json: cli.log_format == LogFormat::Json,
    };
    let full = |p: &StorePath| {
        neki.config()
            .store_dir
            .join(p.to_string())
            .display()
            .to_string()
    };
    let paths_output = |paths: &[StorePath]| {
        let paths: Vec<String> = paths.iter().map(full).collect();
        Output::lines(paths.clone(), json!(paths))
    };
    let output = match cli.command {
        Command::Init => {
            let project = neki.init_project(&dir)?;
            let file = project.root.join(neki_core::MANIFEST_FILE);
            Output::new(
                format!("created {}", file.display()),
                json!({ "created": file }),
            )
        }
        Command::Info => info(&neki, &dir)?,
        Command::Build {
            package,
            file,
            attr,
            drv,
        } => {
            let out = if let Some(file) = file {
                neki.build_file(&dir.join(file), attr.as_deref(), &events)?
            } else if !drv.is_empty() {
                neki.build_drvs(&drv, &events)?
            } else {
                let project = Project::discover(&dir)?;
                neki.build(&project, package.as_deref(), &events)?
            };
            paths_output(&out)
        }
        Command::Run { package, args } => {
            let project = Project::discover(&dir)?;
            let status = neki.run(&project, package.as_deref(), &args, &events)?;
            return Ok(exit_code(status));
        }
        Command::Develop { print_env } => {
            let project = Project::discover(&dir)?;
            let env = neki.develop(&project, &events)?;
            match print_env {
                Some(shell) => Output::new(
                    neki_core::render_activation(&env, shell.into()),
                    env_json(&env),
                ),
                None => return Ok(exit_code(neki.spawn_shell(&env)?)),
            }
        }
        Command::Shell {
            packages,
            print_env,
        } => {
            let project = neki.find_project(&dir)?;
            // Kept from the garbage collector while the shell runs.
            let shell = neki.shell(project.as_ref(), &packages, &events)?;
            match print_env {
                Some(kind) => Output::new(
                    neki_core::render_activation(&shell.env, kind.into()),
                    env_json(&shell.env),
                ),
                None => return Ok(exit_code(neki.spawn_shell(&shell.env)?)),
            }
        }
        Command::Lock { update } => {
            let project = Project::discover(&dir)?;
            let lock = neki.lock(&project, &update, &events)?;
            let lines = lock
                .inputs
                .iter()
                .map(|(name, input)| {
                    let rev = input.rev.as_deref().unwrap_or("-");
                    format!("{name}: {rev} {}", input.nar_hash)
                })
                .collect();
            Output::lines(lines, serde_json::to_value(&lock)?)
        }
        Command::Eval { expr } => {
            let value = neki.eval(&expr, &dir, cli.json)?;
            let json = if cli.json {
                serde_json::from_str(&value)?
            } else {
                Value::Null
            };
            Output::new(value, json)
        }
        Command::Profile(cmd) => profile(&neki, cmd, &events)?,
        Command::Store(cmd) => store(&neki, cmd, cli.json)?,
        Command::Hash(cmd) => {
            let hash = match cmd {
                HashCommand::Path { path } => neki.hash_path(&path)?,
                HashCommand::File { path } => neki.hash_file(&path)?,
            };
            Output::new(hash.to_sri(), json!(hash.to_sri()))
        }
        Command::Cache(cmd) => cache(&neki, cmd)?,
    };
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&output.json)?);
    } else {
        for line in output.text {
            println!("{}", line.trim_end_matches('\n'));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn env_json(env: &neki_core::ShellEnv) -> Value {
    json!({ "vars": env.vars, "path": env.path_prepend, "hook": env.hook })
}

fn info(neki: &Neki, dir: &Path) -> Result<Output> {
    let config = neki.config();
    let project = neki.find_project(dir)?;
    let json = json!({
        "config": config,
        "project": project.as_ref().map(|p| json!({
            "root": p.root,
            "locked": p.lock.is_some(),
            "manifest": p.manifest,
        })),
    });
    let mut lines = vec![
        format!("store dir:    {}", config.store_dir.display()),
        format!("state dir:    {}", config.state_dir.display()),
        format!("system:       {}", config.system),
        format!("max jobs:     {}", config.max_jobs),
        format!("substituters: {}", list(config.substituters.iter())),
    ];
    match project {
        None => lines.push("project:      (none)".into()),
        Some(project) => {
            let manifest = &project.manifest;
            lines.push(format!(
                "project:      {} ({})",
                manifest.project.name,
                project.root.display()
            ));
            lines.push(format!("inputs:       {}", list(manifest.inputs.keys())));
            lines.push(format!("packages:     {}", list(manifest.packages.keys())));
            let shell_packages = manifest
                .dev_shell
                .as_ref()
                .map(|shell| shell.packages.as_slice())
                .unwrap_or_default();
            lines.push(format!("dev shell:    {}", list(shell_packages.iter())));
            lines.push(format!(
                "locked:       {}",
                if project.lock.is_some() { "yes" } else { "no" }
            ));
        }
    }
    Ok(Output::lines(lines, json))
}

fn generation_json(neki: &Neki, g: &Generation) -> Value {
    json!({
        "number": g.number,
        "created": g.created,
        "path": neki.config().store_dir.join(g.path.to_string()),
        "packages": g.packages,
    })
}

fn profile(neki: &Neki, cmd: ProfileCommand, events: &dyn EventSink) -> Result<Output> {
    let generation = |verb: &str, g: Generation| {
        Output::new(
            format!("{verb} generation {}", g.number),
            generation_json(neki, &g),
        )
    };
    Ok(match cmd {
        ProfileCommand::Install { packages, profile } => generation(
            "created",
            neki.profile_install(&profile.profile, &packages, events)?,
        ),
        ProfileCommand::Remove { packages, profile } => generation(
            "created",
            neki.profile_remove(&profile.profile, &packages, events)?,
        ),
        ProfileCommand::Upgrade { profile } => {
            generation("created", neki.profile_upgrade(&profile.profile, events)?)
        }
        ProfileCommand::Rollback { profile } => {
            generation("switched to", neki.profile_rollback(&profile.profile)?)
        }
        ProfileCommand::Switch {
            generation: number,
            profile,
        } => generation(
            "switched to",
            neki.profile_switch(&profile.profile, number)?,
        ),
        ProfileCommand::History { profile } => {
            let current = neki.profile(&profile.profile).current()?.map(|g| g.number);
            let generations = neki.profile_generations(&profile.profile)?;
            let lines = generations
                .iter()
                .map(|g| {
                    let marker = if Some(g.number) == current { "*" } else { " " };
                    format!(
                        "{marker}{:>4}  {}  {}",
                        g.number,
                        g.created,
                        g.packages.join(" ")
                    )
                })
                .collect();
            let json_generations: Vec<Value> = generations
                .iter()
                .map(|g| generation_json(neki, g))
                .collect();
            Output::lines(
                lines,
                json!({ "current": current, "generations": json_generations }),
            )
        }
        ProfileCommand::List => {
            let profiles = neki.profiles()?;
            let names: Vec<String> = profiles.iter().map(|p| p.name.clone()).collect();
            let mut lines = names.clone();
            if let Some(first) = profiles.first() {
                lines.push(format!(
                    "(add {} to PATH)",
                    first.current_link().join("bin").display()
                ));
            }
            Output::lines(lines, json!(names))
        }
    })
}

fn path_info_json(neki: &Neki, info: &PathInfo) -> Value {
    let full = |p: &StorePath| neki.config().store_dir.join(p.to_string());
    json!({
        "path": full(&info.path),
        "narHash": info.nar_hash.to_sri(),
        "narSize": info.nar_size,
        "references": info.references.iter().map(full).collect::<Vec<_>>(),
        "deriver": info.deriver.as_ref().map(full),
        "registrationTime": info.registration_time,
        "signatures": info.signatures,
    })
}

fn store(neki: &Neki, cmd: StoreCommand, json_output: bool) -> Result<Output> {
    let full = |p: &StorePath| {
        neki.config()
            .store_dir
            .join(p.to_string())
            .display()
            .to_string()
    };
    Ok(match cmd {
        StoreCommand::Add { path } => {
            let path = full(&neki.store_add(&path)?);
            Output::new(path.clone(), json!(path))
        }
        StoreCommand::PathInfo { path } => {
            let info = neki.path_info(&path)?;
            let refs: Vec<String> = info.references.iter().map(full).collect();
            let deriver = info.deriver.as_ref().map_or("(none)".into(), full);
            Output::lines(
                vec![
                    format!("path:       {}", full(&info.path)),
                    format!("nar hash:   {}", info.nar_hash.to_sri()),
                    format!("nar size:   {}", info.nar_size),
                    format!("references: {}", list(refs.iter())),
                    format!("deriver:    {deriver}"),
                ],
                path_info_json(neki, &info),
            )
        }
        StoreCommand::Ls => {
            let paths: Vec<String> = neki.store_paths()?.iter().map(full).collect();
            Output::lines(paths.clone(), json!(paths))
        }
        StoreCommand::Closure { path } => {
            let infos = neki.closure(&path)?;
            let total: u64 = infos.iter().map(|i| i.nar_size).sum();
            let mut lines: Vec<String> = infos
                .iter()
                .map(|i| format!("{:>12}  {}", i.nar_size, full(&i.path)))
                .collect();
            lines.push(format!("{total:>12}  total"));
            let paths: Vec<Value> = infos.iter().map(|i| path_info_json(neki, i)).collect();
            Output::lines(lines, json!({ "paths": paths, "totalNarSize": total }))
        }
        StoreCommand::WhyDepends { from, to } => {
            let chains = neki.why_depends(&from, &to)?;
            if chains.is_empty() && !json_output {
                bail!("{from} does not depend on {to}");
            }
            let lines = chains
                .iter()
                .map(|chain| {
                    let names: Vec<&str> = chain.iter().map(StorePath::name).collect();
                    names.join(" → ")
                })
                .collect();
            let json_chains: Vec<Vec<String>> = chains
                .iter()
                .map(|chain| chain.iter().map(full).collect())
                .collect();
            Output::lines(lines, json!(json_chains))
        }
        StoreCommand::Roots => {
            let roots = neki.roots()?;
            let lines = roots
                .iter()
                .map(|r| format!("{}  {}", r.name, full(&r.path)))
                .collect();
            let json_roots: Vec<Value> = roots
                .iter()
                .map(|r| json!({ "name": r.name, "path": full(&r.path), "link": r.link }))
                .collect();
            Output::lines(lines, json!(json_roots))
        }
        StoreCommand::Verify => {
            let modified = neki.verify()?;
            if !modified.is_empty() && !json_output {
                for path in &modified {
                    eprintln!("modified: {}", full(path));
                }
                bail!("{} of the store's paths were modified", modified.len());
            }
            let paths: Vec<String> = modified.iter().map(full).collect();
            let text = if paths.is_empty() {
                "all paths are intact"
            } else {
                ""
            };
            Output::new(text, json!({ "modified": paths }))
        }
        StoreCommand::Gc {
            dry_run,
            delete_older_than,
        } => {
            let result = neki.gc(&GcOptions {
                dry_run,
                delete_older_than,
            })?;
            let verb = if dry_run {
                "would be deleted"
            } else {
                "deleted"
            };
            let paths: Vec<String> = result.deleted.iter().map(full).collect();
            let mut lines = if dry_run { paths.clone() } else { Vec::new() };
            lines.push(format!(
                "{} paths {verb}, {} bytes",
                result.deleted.len(),
                result.bytes_freed
            ));
            Output::lines(
                lines,
                json!({ "deleted": paths, "bytesFreed": result.bytes_freed, "dryRun": dry_run }),
            )
        }
        StoreCommand::Optimise => {
            let saved = neki.optimise()?;
            Output::new(
                format!("{saved} bytes saved"),
                json!({ "bytesSaved": saved }),
            )
        }
        StoreCommand::Serve { listen, key_file } => {
            neki.serve(&listen, key_file.as_deref(), |addr| {
                eprintln!(
                    "serving {} at http://{addr}",
                    neki.config().store_dir.display()
                );
            })?;
            Output::new("", Value::Null)
        }
    })
}

fn cache(neki: &Neki, cmd: CacheCommand) -> Result<Output> {
    let full = |p: &StorePath| {
        neki.config()
            .store_dir
            .join(p.to_string())
            .display()
            .to_string()
    };
    Ok(match cmd {
        CacheCommand::Push {
            url,
            paths,
            key_file,
        } => {
            let uploaded: Vec<String> = neki
                .cache_push(&url, &paths, key_file.as_deref())?
                .iter()
                .map(full)
                .collect();
            let mut lines = uploaded.clone();
            lines.push(format!("{} paths uploaded", uploaded.len()));
            Output::lines(lines, json!({ "uploaded": uploaded }))
        }
        CacheCommand::Fetch { url, paths } => {
            let fetched: Vec<String> = neki.cache_fetch(&url, &paths)?.iter().map(full).collect();
            Output::lines(fetched.clone(), json!({ "fetched": fetched }))
        }
        CacheCommand::Keygen { name, secret_file } => {
            if secret_file.exists() {
                bail!("{} already exists", secret_file.display());
            }
            let key = neki.cache_keygen(&name)?;
            std::fs::write(&secret_file, key.to_text())
                .with_context(|| format!("writing {}", secret_file.display()))?;
            let public = key.public().to_string();
            Output::new(
                public.clone(),
                json!({ "publicKey": public, "secretFile": secret_file }),
            )
        }
    })
}

/// Space-separated items, or `(none)`.
fn list<'a>(items: impl Iterator<Item = &'a String>) -> String {
    let items: Vec<&str> = items.map(String::as_str).collect();
    if items.is_empty() {
        "(none)".to_string()
    } else {
        items.join(" ")
    }
}

fn exit_code(status: ExitStatus) -> ExitCode {
    ExitCode::from(
        status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .unwrap_or(1),
    )
}

/// Prints progress to stderr, keeping stdout for results.
struct Sink {
    json: bool,
}

impl EventSink for Sink {
    fn event(&self, event: Event) {
        if self.json {
            let value = match &event {
                Event::BuildStarted { drv } => {
                    json!({ "event": "build-started", "drv": drv.to_string() })
                }
                Event::BuildLog { drv, line } => {
                    json!({ "event": "build-log", "drv": drv.to_string(), "line": line })
                }
                Event::BuildFinished { drv, success } => {
                    json!({ "event": "build-finished", "drv": drv.to_string(), "success": success })
                }
                Event::Download { path, done, total } => json!({
                    "event": "download",
                    "path": path.to_string(),
                    "done": done,
                    "total": total,
                }),
                Event::Substituted { path } => {
                    json!({ "event": "substituted", "path": path.to_string() })
                }
                Event::Message(message) => json!({ "event": "message", "message": message }),
            };
            eprintln!("{value}");
            return;
        }
        match event {
            Event::BuildStarted { drv } => eprintln!("building {drv}"),
            Event::BuildLog { line, .. } => eprintln!("  {line}"),
            Event::BuildFinished {
                drv,
                success: false,
            } => eprintln!("failed {drv}"),
            Event::BuildFinished { .. } => {}
            Event::Download { path, done: 0, .. } => eprintln!("downloading {path}"),
            Event::Download { .. } => {}
            Event::Substituted { path } => eprintln!("downloaded {path}"),
            Event::Message(message) => eprintln!("{message}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        super::Cli::command().debug_assert();
    }

    #[test]
    fn ages() {
        let age = super::parse_age;
        assert_eq!(age("30d").unwrap(), Duration::from_secs(30 * 86400));
        assert_eq!(age("2").unwrap(), Duration::from_secs(2 * 86400));
        assert_eq!(age("12h").unwrap(), Duration::from_secs(12 * 3600));
        assert!(age("1y").is_err());
        assert!(age("d").is_err());
    }
}
