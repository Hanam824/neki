//! Runs the real `neki-shim` binary against a sidecar.

use std::process::Command;

#[test]
fn runs_the_target_with_arguments_and_path() {
    let dir = tempfile::tempdir().unwrap();
    let shim = dir
        .path()
        .join(if cfg!(windows) { "greet.exe" } else { "greet" });
    std::fs::copy(env!("CARGO_BIN_EXE_neki-shim"), &shim).unwrap();
    let (target, args): (&str, &[&str]) = if cfg!(windows) {
        (
            "C:\\Windows\\System32\\cmd.exe",
            &["/d", "/c", "echo hello & echo %PATH% & exit 7"],
        )
    } else {
        (
            "/bin/sh",
            &["-c", "echo \"$0\"; echo \"$PATH\"; exit 7", "hello"],
        )
    };
    let sidecar = neki_shim::Shim {
        target: target.into(),
        path: vec![dir.path().join("extra")],
        env: vec![("SHIM_TEST".into(), "1".into())],
    };
    std::fs::write(neki_shim::shim_sidecar(&shim), sidecar.to_text()).unwrap();
    let output = Command::new(&shim)
        .args(args)
        .arg("hello")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(7), "{stdout}");
    assert!(stdout.contains("hello"), "{stdout}");
    assert!(stdout.contains("extra"), "{stdout}");

    std::fs::remove_file(neki_shim::shim_sidecar(&shim)).unwrap();
    let output = Command::new(&shim).output().unwrap();
    assert_eq!(output.status.code(), Some(127));
}
