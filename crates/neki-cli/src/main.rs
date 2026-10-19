//! The `neki` command-line interface: argument parsing and output only;
//! every operation goes through `neki-core`.

use std::path::{Path, PathBuf};
use std::process::{ExitCode, ExitStatus};

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};
use neki_core::{Config, Event, EventSink, GcOptions, Neki, Project, StorePath};

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
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a new project (neki.toml) in the current or `-C` directory.
    Init,
    /// Show the configuration and the current project.
    Info {
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Build a package of the current project.
    Build {
        /// Package to build (default: `default`).
        package: Option<String>,
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
    Develop,
    /// Start a shell with the given packages available.
    Shell {
        #[arg(required = true)]
        packages: Vec<String>,
    },
    /// Create or update neki.lock.
    Lock {
        /// Re-resolve this input (repeatable).
        #[arg(long = "update", value_name = "INPUT")]
        update: Vec<String>,
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
    /// Install packages into the profile.
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
    /// Switch back to the previous generation.
    Rollback {
        #[command(flatten)]
        profile: ProfileArg,
    },
    /// List the profile's generations.
    History {
        #[command(flatten)]
        profile: ProfileArg,
    },
}

#[derive(Debug, Subcommand)]
enum StoreCommand {
    /// Copy a file or directory into the store.
    Add { path: PathBuf },
    /// Show metadata of a store path.
    PathInfo { path: String },
    /// Check that store contents match their recorded hashes.
    Verify,
    /// Delete store paths that are not reachable from any GC root.
    Gc {
        /// Only show what would be deleted.
        #[arg(long)]
        dry_run: bool,
    },
    /// Replace identical files in the store with hard links.
    Optimise,
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
    /// Upload store paths and their closures to a binary cache.
    Push {
        url: String,
        #[arg(required = true)]
        paths: Vec<String>,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let neki = Neki::new(Config::from_env());
    let dir = match cli.dir {
        Some(dir) => dir,
        None => std::env::current_dir()?,
    };
    let events = StderrSink;
    match cli.command {
        Command::Init => {
            let project = neki.init_project(&dir)?;
            println!(
                "created {}",
                project.root.join(neki_core::MANIFEST_FILE).display()
            );
        }
        Command::Info { json } => info(&neki, &dir, json)?,
        Command::Build { package } => {
            let project = Project::discover(&dir)?;
            for path in neki.build(&project, package.as_deref(), &events)? {
                print_path(&neki, &path);
            }
        }
        Command::Run { package, args } => {
            let project = Project::discover(&dir)?;
            return Ok(exit_code(neki.run(
                &project,
                package.as_deref(),
                &args,
                &events,
            )?));
        }
        Command::Develop => {
            let project = Project::discover(&dir)?;
            let env = neki.develop(&project, &events)?;
            return Ok(exit_code(neki.spawn_shell(&env)?));
        }
        Command::Shell { packages } => {
            let env = neki.shell(&packages, &events)?;
            return Ok(exit_code(neki.spawn_shell(&env)?));
        }
        Command::Lock { update } => {
            let project = Project::discover(&dir)?;
            neki.lock(&project, &update, &events)?;
        }
        Command::Profile(cmd) => profile(&neki, cmd, &events)?,
        Command::Store(cmd) => store(&neki, cmd)?,
        Command::Hash(cmd) => {
            let hash = match cmd {
                HashCommand::Path { path } => neki.hash_path(&path)?,
                HashCommand::File { path } => neki.hash_file(&path)?,
            };
            println!("{}", hash.to_sri());
        }
        Command::Cache(CacheCommand::Push { url, paths }) => {
            neki.cache_push(&url, &paths, &events)?
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn info(neki: &Neki, dir: &Path, json: bool) -> Result<()> {
    let config = neki.config();
    let project = neki.find_project(dir)?;
    if json {
        let project = project.map(|p| {
            serde_json::json!({ "root": p.root, "locked": p.lock.is_some(), "manifest": p.manifest })
        });
        let out = serde_json::json!({ "config": config, "project": project });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    println!("store dir:    {}", config.store_dir.display());
    println!("state dir:    {}", config.state_dir.display());
    println!("system:       {}", config.system);
    println!("max jobs:     {}", config.max_jobs);
    println!("substituters: {}", list(config.substituters.iter()));
    let Some(project) = project else {
        println!("project:      (none)");
        return Ok(());
    };
    let manifest = &project.manifest;
    println!(
        "project:      {} ({})",
        manifest.project.name,
        project.root.display()
    );
    println!("inputs:       {}", list(manifest.inputs.keys()));
    println!("packages:     {}", list(manifest.packages.keys()));
    let shell_packages = manifest
        .dev_shell
        .as_ref()
        .map(|shell| shell.packages.as_slice())
        .unwrap_or_default();
    println!("dev shell:    {}", list(shell_packages.iter()));
    println!(
        "locked:       {}",
        if project.lock.is_some() { "yes" } else { "no" }
    );
    Ok(())
}

fn profile(neki: &Neki, cmd: ProfileCommand, events: &dyn EventSink) -> Result<()> {
    match cmd {
        ProfileCommand::Install { packages, profile } => {
            let generation = neki.profile_install(&profile.profile, &packages, events)?;
            println!("created generation {}", generation.number);
        }
        ProfileCommand::Remove { packages, profile } => {
            let generation = neki.profile_remove(&profile.profile, &packages, events)?;
            println!("created generation {}", generation.number);
        }
        ProfileCommand::Rollback { profile } => {
            let generation = neki.profile_rollback(&profile.profile)?;
            println!("switched to generation {}", generation.number);
        }
        ProfileCommand::History { profile } => {
            for generation in neki.profile_generations(&profile.profile)? {
                println!(
                    "{:>4}  {}  {}",
                    generation.number, generation.created, generation.path
                );
            }
        }
    }
    Ok(())
}

fn store(neki: &Neki, cmd: StoreCommand) -> Result<()> {
    match cmd {
        StoreCommand::Add { path } => print_path(neki, &neki.store_add(&path)?),
        StoreCommand::PathInfo { path } => {
            let info = neki.path_info(&path)?;
            println!("path:       {}", info.path);
            println!("nar hash:   {}", info.nar_hash.to_sri());
            println!("nar size:   {}", info.nar_size);
            println!(
                "references: {}",
                list(
                    info.references
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .iter()
                )
            );
        }
        StoreCommand::Verify => {
            let modified = neki.verify()?;
            for path in &modified {
                println!("modified: {path}");
            }
            if !modified.is_empty() {
                bail!("{} store paths were modified", modified.len());
            }
        }
        StoreCommand::Gc { dry_run } => {
            let result = neki.gc(&GcOptions {
                dry_run,
                ..GcOptions::default()
            })?;
            let verb = if dry_run {
                "would be deleted"
            } else {
                "deleted"
            };
            println!(
                "{} paths {verb}, {} bytes",
                result.deleted.len(),
                result.bytes_freed
            );
        }
        StoreCommand::Optimise => println!("{} bytes saved", neki.optimise()?),
    }
    Ok(())
}

fn print_path(neki: &Neki, path: &StorePath) {
    println!(
        "{}",
        neki.config().store_dir.join(path.to_string()).display()
    );
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
struct StderrSink;

impl EventSink for StderrSink {
    fn event(&self, event: Event) {
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
            Event::Message(message) => eprintln!("{message}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        super::Cli::command().debug_assert();
    }
}
