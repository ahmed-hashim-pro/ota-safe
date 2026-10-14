//! The `ota-sim` binary, as the README uses it.

use std::process::Command;

fn run(args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_ota-sim"))
        .args(args)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    (out.status.code().unwrap_or(-1), text)
}

#[test]
fn matrix_passes_and_shows_every_scheme() {
    let (code, out) = run(&["matrix"]);
    assert_eq!(code, 0, "{out}");
    for s in [
        "naive in-place",
        "naive A/B",
        "ota-safe",
        "98.6% bricked",
        "3.1% bricked",
        "every cut ok",
    ] {
        assert!(out.contains(s), "missing {s:?} in\n{out}");
    }
}

#[test]
fn mutants_are_all_caught() {
    let (code, out) = run(&["mutants"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out.matches(" caught ").count(), 6, "{out}");
    assert!(!out.contains("MISSED"));
}

#[test]
fn life_replays_a_cut() {
    let (code, out) = run(&["life", "--scheme", "naive-ab", "--cut", "44:erase-0"]);
    assert_eq!(code, 1, "a bricked life exits 1:\n{out}");
    assert!(
        out.contains("POWER CUT at op 44 (erase sector 0x00000)"),
        "{out}"
    );
    assert!(out.contains("BRICKED"), "{out}");
    let (code, out) = run(&["life", "--cut", "44:erase-0"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.ends_with("ok\n"), "{out}");
}

#[test]
fn dump_writes_the_flash_and_states_the_decision() {
    let dir = std::env::temp_dir().join(format!("ota-sim-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cut.bin");
    let (code, out) = run(&["dump", "--cut", "47:prefix-2", "-o", file.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(std::fs::metadata(&file).unwrap().len(), 40960);
    assert!(out.contains("would boot"), "{out}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn bad_arguments_exit_2() {
    assert_eq!(run(&[]).0, 2);
    assert_eq!(run(&["frobnicate"]).0, 2);
    assert_eq!(run(&["matrix", "--depth", "3"]).0, 2);
    assert_eq!(run(&["life", "--cut", "7:sideways"]).0, 2);
    assert_eq!(run(&["life", "--scheme", "nope"]).0, 2);
}
