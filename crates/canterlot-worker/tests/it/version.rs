use std::process::Command;

#[test]
fn binary_reports_the_workspace_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_canterlot-worker"))
        .arg("--version")
        .output()
        .expect("worker binary starts");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("canterlot-worker {}", env!("CARGO_PKG_VERSION"))
    );
}
