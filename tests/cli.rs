use std::process::Command;

#[test]
fn help_states_the_identity() {
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .arg("--help")
        .output()
        .expect("binary runs");
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("utf8");
    assert!(text.contains("declared graphs"));
}

#[test]
fn validate_reports_invalid_contracts() {
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["validate", "nonexistent.json"])
        .output()
        .expect("binary runs");
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8(out.stderr).expect("utf8");
    assert!(err.contains("invalid nonexistent.json"));
}
