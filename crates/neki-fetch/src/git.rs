//! Git sources, through the `git` command, and GitHub's tarball API.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::{Error, Result, at, http_get};

/// Settings that make a checkout byte-identical on every OS: no line-ending
/// conversion, and no system or user configuration.
fn git() -> Command {
    let mut cmd = Command::new("git");
    cmd.args(["-c", "core.autocrlf=false", "-c", "core.eol=lf"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    cmd
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

/// The commit that `reference` (a branch or tag; `HEAD` if `None`) points
/// to in the repository at `url`. A full commit hash is returned as is.
pub fn git_resolve(url: &str, reference: Option<&str>) -> Result<String> {
    let reference = reference.unwrap_or("HEAD");
    if is_commit(reference) {
        return Ok(reference.to_string());
    }
    let mut cmd = git();
    cmd.args(["ls-remote", url, reference]);
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

fn is_commit(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Check out commit `rev` of `url` into the new directory `dest`, without
/// the `.git` directory.
pub fn git_checkout(url: &str, rev: &str, dest: &Path) -> Result<()> {
    std::fs::create_dir(dest).map_err(at(dest))?;
    let in_dest = |args: &[&str]| {
        let mut cmd = git();
        cmd.arg("-C").arg(dest).args(args);
        cmd
    };
    run(url, in_dest(&["init", "--quiet"]))?;
    // Most servers allow fetching a single commit; otherwise fetch everything.
    if run(
        url,
        in_dest(&["fetch", "--quiet", "--depth", "1", url, rev]),
    )
    .is_err()
    {
        run(
            url,
            in_dest(&[
                "fetch",
                "--quiet",
                url,
                "+refs/heads/*:refs/remotes/origin/*",
                "+refs/tags/*:refs/tags/*",
            ]),
        )?;
    }
    run(
        url,
        in_dest(&[
            "-c",
            "advice.detachedHead=false",
            "checkout",
            "--quiet",
            rev,
        ]),
    )?;
    // Git only sets executable bits where the OS has them; mark the same
    // files on every OS so the tree hashes the same.
    let index = run(url, in_dest(&["ls-files", "--stage", "-z"]))?;
    for entry in index.split('\0') {
        if let Some(("100755", rest)) = entry.split_once(' ')
            && let Some((_, file)) = rest.split_once('\t')
        {
            let file = dest.join(file);
            neki_platform::fs::set_executable(&file).map_err(at(&file))?;
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
