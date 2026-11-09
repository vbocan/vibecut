//! Test helper for code that reads the real environment (`from_env`
//! constructors). `std::env::set_var` would race the other tests running in
//! parallel, so such a test runs itself again in a child process that has
//! the variables set:
//!
//! ```ignore
//! const TEST: &str = "ml_engine::tests::some_test";
//! if test_env::is_child(TEST) {
//!     // checks against the real environment
//!     return;
//! }
//! // set up files, then:
//! test_env::run_child(TEST, &[("VIBECUT_MODELS_DIR", dir.as_str())]);
//! ```

use std::process::Command;

const CHILD_VAR: &str = "VIBECUT_TEST_CHILD";

/// Whether this process is the child [`run_child`] started for `test`.
pub fn is_child(test: &str) -> bool {
    std::env::var(CHILD_VAR).is_ok_and(|v| v == test)
}

/// Run `test` (its path in the crate, e.g. `fonts::tests::name`) alone in a
/// child process with `vars` set, and fail unless it ran and passed.
pub fn run_child(test: &str, vars: &[(&str, &str)]) {
    let out = Command::new(std::env::current_exe().expect("test binary path"))
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_VAR, test)
        .envs(vars.iter().copied())
        .output()
        .expect("spawn the test binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let report = || format!("{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "child run of {test} failed:\n{}", report());
    // A filter that matches nothing also exits 0.
    assert!(stdout.contains("1 passed"), "child run of {test} ran no test:\n{}", report());
}
