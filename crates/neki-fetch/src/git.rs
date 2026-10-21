//! Git sources, through the `git` command, and GitHub's tarball API.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{Error, Result, at, http_get};

/// Settings that make a checkout byte-identical on every OS: no line-ending
/// conversion, paths longer than Windows' old 260-character limit, and no
/// system configuration.
fn git() -> Command {
    let mut cmd = Command::new("git");
    cmd.args(["-c", "core.autocrlf=false", "-c", "core.eol=lf"])
        .args(["-c", "core.longpaths=true"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    cmd
}

/// [`git`] without the user's configuration either, for the commands that
/// write the tree: filters (such as Git LFS), hooks and attributes from it
/// would make checkouts differ between machines. Fetching keeps it, for
/// credentials and proxies; the commit hash pins what is fetched. (Git for
/// Windows reads `/dev/null` as `NUL`.)
fn isolated_git() -> Command {
    let mut cmd = git();
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
    cmd
}

/// Reject a URL or ref that git would read as an option.
fn check_arg<'a>(url: &str, what: &str, value: &'a str) -> Result<&'a str> {
    if value.starts_with('-') {
        return Err(Error::Failed {
            url: url.to_string(),
            reason: format!("{what} {value:?} starts with `-`"),
        });
    }
    Ok(value)
}

fn run(url: &str, mut cmd: Command) -> Result<String> {
    let output = cmd.output().map_err(|err| Error::Failed {
        url: url.to_string(),
        reason: if err.kind() == std::io::ErrorKind::NotFound {
            "git inputs need the `git` program on PATH".into()
        } else {
            err.to_string()
        },
    })?;
    if !output.status.success() {
        return Err(Error::Failed {
            url: url.to_string(),
            reason: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The files of the directory `dir` that belong in a copy of it, relative
/// to it, if it's in a git work tree: those git tracks, and untracked ones
/// that aren't ignored. `None` outside a work tree, or if the work tree
/// ignores `dir` itself (a home directory kept in git, say). Only the
/// repository's own ignore rules count, not the user's global ones, so the
/// copy is the same on every machine.
pub fn git_source_files(dir: &Path) -> Result<Option<BTreeSet<PathBuf>>> {
    let dir = std::path::absolute(dir).map_err(at(dir))?;
    if !dir.ancestors().any(|d| d.join(".git").exists()) {
        return Ok(None);
    }
    let failed = |reason: String| Error::SourceFiles {
        dir: dir.clone(),
        reason,
    };
    let git_in_dir = |args: &[&str]| {
        let mut cmd = isolated_git();
        cmd.arg("-C").arg(&dir).args(args);
        cmd.output().map_err(|err| {
            failed(if err.kind() == std::io::ErrorKind::NotFound {
                "it's in a git repository, and listing its files needs the `git` program on PATH"
                    .into()
            } else {
                err.to_string()
            })
        })
    };
    // Exit code 0: ignored, 1: not ignored.
    if git_in_dir(&["check-ignore", "--quiet", "--", "."])?
        .status
        .success()
    {
        return Ok(None);
    }
    let out = git_in_dir(&[
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
    ])?;
    if !out.status.success() {
        return Err(failed(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(Some(
        String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter(|f| !f.is_empty())
            .map(|f| f.split('/').collect())
            .collect(),
    ))
}

/// The commit that `reference` (a branch or tag; `HEAD` if `None`) points
/// to in the repository at `url`. A full commit hash is returned as is.
pub fn git_resolve(url: &str, reference: Option<&str>) -> Result<String> {
    let reference = check_arg(url, "ref", reference.unwrap_or("HEAD"))?;
    if is_commit(reference) {
        return Ok(reference.to_string());
    }
    let mut cmd = git();
    cmd.args(["ls-remote", "--", check_arg(url, "URL", url)?, reference]);
    let out = run(url, cmd)?;
    // Prefer an exact match, then the peeled commit of an annotated tag.
    let lines: Vec<(&str, &str)> = out
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .collect();
    let peeled = lines.iter().find(|(_, name)| name.ends_with("^{}"));
    peeled
        .or_else(|| lines.first())
        .map(|(rev, _)| rev.to_string())
        .ok_or_else(|| Error::Failed {
            url: url.to_string(),
            reason: format!("no ref named {reference}"),
        })
}

/// A full commit hash: SHA-1, or SHA-256 for repositories that use it.
fn is_commit(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Check out commit `rev` (a full commit hash) of `url` into the new
/// directory `dest`, without the `.git` directory.
pub fn git_checkout(url: &str, rev: &str, dest: &Path) -> Result<()> {
    check_arg(url, "URL", url)?;
    if !is_commit(rev) {
        return Err(Error::Failed {
            url: url.to_string(),
            reason: format!("{rev:?} is not a full commit hash"),
        });
    }
    std::fs::create_dir(dest).map_err(at(dest))?;
    let in_dest = |mut cmd: Command, args: &[&str]| {
        cmd.arg("-C").arg(dest).args(args);
        cmd
    };
    // No templates: they could install hooks. The repository must use the
    // same hash function as the remote.
    let mut init = vec!["init", "--quiet", "--template="];
    if rev.len() == 64 {
        init.push("--object-format=sha256");
    }
    run(url, in_dest(isolated_git(), &init))?;
    // Most servers allow fetching a single commit; otherwise fetch everything.
    if run(
        url,
        in_dest(git(), &["fetch", "--quiet", "--depth", "1", "--", url, rev]),
    )
    .is_err()
    {
        run(
            url,
            in_dest(
                git(),
                &[
                    "fetch",
                    "--quiet",
                    "--",
                    url,
                    "+refs/heads/*:refs/remotes/origin/*",
                    "+refs/tags/*:refs/tags/*",
                ],
            ),
        )?;
    }
    run(
        url,
        in_dest(
            isolated_git(),
            &[
                "-c",
                "advice.detachedHead=false",
                "checkout",
                "--quiet",
                rev,
            ],
        ),
    )?;
    // Git only sets executable bits where the OS has them; mark the same
    // files on every OS so the tree hashes the same. Where the OS has no
    // symlinks, git writes them as plain files, which would hash
    // differently, so that fails instead, like unpacking a NAR does.
    let index = run(url, in_dest(isolated_git(), &["ls-files", "--stage", "-z"]))?;
    for entry in index.split('\0') {
        let Some((mode, rest)) = entry.split_once(' ') else {
            continue;
        };
        let Some((_, file)) = rest.split_once('\t') else {
            continue;
        };
        let file = dest.join(file);
        match mode {
            "100755" => neki_platform::fs::set_executable(&file).map_err(at(&file))?,
            "120000"
                if !std::fs::symlink_metadata(&file)
                    .map_err(at(&file))?
                    .file_type()
                    .is_symlink() =>
            {
                return Err(Error::Failed {
                    url: url.to_string(),
                    reason: format!(
                        "{} is a symbolic link, and symbolic links in store contents \
                         aren't supported on this OS yet",
                        file.display()
                    ),
                });
            }
            _ => {}
        }
    }
    let git_dir = dest.join(".git");
    neki_platform::fs::remove_tree(&git_dir).map_err(at(&git_dir))
}

/// The commit `reference` (branch, tag or commit; the default branch if
/// `None`) points to in GitHub repository `owner/repo`. Set `GITHUB_TOKEN`
/// to raise the API rate limit.
pub fn github_resolve(owner: &str, repo: &str, reference: Option<&str>) -> Result<String> {
    if let Some(rev) = reference.filter(|r| is_commit(r)) {
        return Ok(rev.to_string());
    }
    let url = format!(
        "https://api.github.com/repos/{owner}/{repo}/commits/{}",
        reference.unwrap_or("HEAD")
    );
    let token = std::env::var("GITHUB_TOKEN")
        .ok()
        .map(|t| format!("Bearer {t}"));
    let mut headers = vec![("Accept", "application/vnd.github.sha")];
    if let Some(token) = &token {
        headers.push(("Authorization", token));
    }
    let mut body = String::new();
    http_get(&url, &headers)?
        .into_body()
        .into_reader()
        .take(1024)
        .read_to_string(&mut body)
        .map_err(|err| Error::Failed {
            url: url.clone(),
            reason: err.to_string(),
        })?;
    let rev = body.trim();
    if !is_commit(rev) {
        return Err(Error::Failed {
            url,
            reason: format!("unexpected response {rev:?}"),
        });
    }
    Ok(rev.to_string())
}

/// URL of the tarball of commit `rev` of GitHub repository `owner/repo`.
pub fn github_tarball_url(owner: &str, repo: &str, rev: &str) -> String {
    format!("https://github.com/{owner}/{repo}/archive/{rev}.tar.gz")
}
