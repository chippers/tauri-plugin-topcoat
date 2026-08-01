//! Singular developer entrypoint.
//!
//! It shells out; everything here is something you could type yourself. The
//! point is that the order and the gates are written down once, and that CI
//! runs them by name rather than keeping its own copy.

use std::process::{Command, ExitCode};

/// The hooks this repository tracks, which git has to be pointed at.
const HOOKS: &str = ".githooks";

fn help() {
    eprintln!(
        "cargo xtask <task>

  lint       fmt, then clippy over every feature, and the docs
  test       the suites and the doctests
  check      lint then test, which between them are what CI runs
  hooks      run check before every commit, too (once per clone)"
    );
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let task = arguments.first().map(String::as_str).unwrap_or("help");

    let ok = match task {
        "lint" => lint(),
        "test" => test(),
        "check" => lint() && test(),
        "hooks" => hooks(),

        _ => {
            help();
            task == "help"
        }
    };

    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Everything that can be answered without running the code.
///
/// Cheapest first, so a failure arrives as early as it can.
fn lint() -> bool {
    cargo(&["fmt", "--all", "--check"])
        && lint_with(&["--workspace", "--all-targets"])
        && lint_with(&["--workspace", "--all-targets", "--all-features"])
        && cargo(&["doc", "--workspace", "--no-deps", "--all-features"])
}

/// Everything you only learn by running it.
fn test() -> bool {
    cargo(&["test", "--workspace", "--all-targets"])
        && cargo(&["test", "--workspace", "--all-targets", "--all-features"])
}

/// Points git at the hooks this repository tracks.
///
/// Git only ever looks in `.git/hooks`, which nothing can track, so a hook
/// committed to a repository is a hook nobody runs until somebody says this.
/// The hook itself is one line: `cargo xtask check`, the same gate CI names
/// and the same one you type, rather than a third copy that can drift.
fn hooks() -> bool {
    run("git", &["config", "core.hooksPath", HOOKS])
}

/// Runs clippy over one selection with a warning counted as a failure.
///
/// A warning nobody is forced to read is a warning that stays, so clippy denies
/// its own here rather than leaning on a `-D warnings` that CI's environment
/// sets and your shell does not.
fn lint_with(arguments: &[&str]) -> bool {
    let mut clippy = vec!["clippy"];
    clippy.extend_from_slice(arguments);
    clippy.extend_from_slice(&["--", "-D", "warnings"]);
    cargo(&clippy)
}

/// Runs cargo against the committed `Cargo.lock`.
///
/// No task here has any business changing it: a gate that resolved its own
/// dependencies would be reporting on a workspace nobody else has.
fn cargo(arguments: &[&str]) -> bool {
    let mut locked = vec!["--locked"];
    locked.extend_from_slice(arguments);
    run("cargo", &locked)
}

fn run(program: &str, arguments: &[&str]) -> bool {
    eprintln!("$ {program} {}", arguments.join(" "));

    match Command::new(program).args(arguments).status() {
        Ok(status) => status.success(),
        Err(error) => {
            eprintln!("could not run {program}: {error}");
            false
        }
    }
}
