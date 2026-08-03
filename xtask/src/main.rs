//! Singular developer entrypoint.
//!
//! It shells out; everything here is something you could type yourself. The
//! point is that the order and the gates are written down once, and that CI
//! runs them by name rather than keeping its own copy.

use std::path::Path;
use std::process::{Command, ExitCode};

/// The hooks this repository tracks, which git has to be pointed at.
const HOOKS: &str = ".githooks";

/// The probe's page. Its own build script builds it, so nothing here does; this
/// is only where the type-checker runs from.
const UI: &str = "probe/ui";

/// The feature selections that neither workspace-wide clippy run reaches.
///
/// Those two runs are all-default and all-features, so between them they never
/// build a default feature switched off, nor an optional one on its own. Which
/// leaves `custom-protocol-http` never seen without `tower`, and `tracing` -
/// which compiles to nothing when absent and to real calls when present - never
/// type-checked apart from `session`.
const FEATURES: &[&[&str]] = &[
    &["-p", "custom-protocol-http", "--no-default-features"],
    &[
        "-p",
        "custom-protocol-http",
        "--no-default-features",
        "--features",
        "tracing",
    ],
    &["-p", "tauri-plugin-topcoat", "--features", "session"],
    &["-p", "tauri-plugin-topcoat", "--features", "tracing"],
];

fn help() {
    eprintln!(
        "cargo xtask <task>

  bindings   hold the probe's TypeScript to its Rust types, rewriting it if not
  probe      run the conformance probe (--exit to quit once it reports)

  hello      the least that renders over the protocol
  session    sessions, with the token held out of the webview

  lint       fmt, the page's own two, clippy over every feature, and the docs
  test       the suites, the doctests, then the page against its bindings
  check      lint then test, which between them are what CI runs
  hooks      run check before every commit, too (once per clone)"
    );
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let task = arguments.first().map(String::as_str).unwrap_or("help");

    // Anything after the task belongs to what the task runs, not to us.
    let rest: Vec<&str> = arguments.iter().skip(1).map(String::as_str).collect();

    let ok = match task {
        // The regeneration already lives in a unit test, so this task is that
        // test by name. Driving a task off a test is untried here; if it starts
        // fighting the test runner, give it its own binary.
        "bindings" => cargo(&["test", "-p", "topcoat-probe", "bindings"]),

        "probe" => probe(&rest),

        "hello" => example("example-hello"),
        "session" => example("example-session"),

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
/// Cheapest first, so a failure arrives as early as it can. CI runs this on
/// every platform in the matrix rather than one, because clippy only ever sees
/// the `cfg` branches live on the host it ran on - and the branches that differ
/// per platform are what this workspace is about.
fn lint() -> bool {
    cargo(&["fmt", "--all", "--check"])
        && lint_with(&["--workspace", "--all-targets"])
        // no earlier: both live in `node_modules`, which the probe's build
        // script installs
        && pnpm(UI, &["run", "fmt:check"])
        && pnpm(UI, &["run", "lint"])
        && lint_with(&["--workspace", "--all-targets", "--all-features"])
        && FEATURES.iter().all(|selection| {
            let mut arguments = vec!["--all-targets"];
            arguments.extend_from_slice(selection);
            lint_with(&arguments)
        })
        && cargo(&["doc", "--workspace", "--no-deps", "--all-features"])
}

/// Everything you only learn by running it.
///
/// The page is type-checked last because the bindings it is checked against are
/// what the first of these tests rewrites.
fn test() -> bool {
    cargo(&["test", "--workspace", "--all-targets"])
        && cargo(&["test", "--workspace", "--all-targets", "--all-features"])
        // `--all-targets` skips doctests, and the usage examples are doctests.
        && cargo(&["test", "--workspace", "--doc", "--all-features"])
        && pnpm(UI, &["run", "typecheck"])
}

/// Runs one example application.
///
/// Release, because these are windows somebody is going to interact with and
/// the debug build of a webview application feels it.
fn example(package: &str) -> bool {
    cargo(&["run", "--release", "-p", package])
}

/// Runs the conformance probe with whatever followed the task name.
fn probe(rest: &[&str]) -> bool {
    let mut arguments = vec!["run", "--release", "-p", "topcoat-probe", "--"];
    arguments.extend_from_slice(rest);
    cargo(&arguments)
}

/// Points git at the hooks this repository tracks.
///
/// Git only ever looks in `.git/hooks`, which nothing can track, so a hook
/// committed to a repository is a hook nobody runs until somebody says this.
/// The hook itself is one line: `cargo xtask check`, the same gate CI names
/// and the same one you type, rather than a third copy that can drift.
fn hooks() -> bool {
    run("git", &["config", "core.hooksPath", HOOKS], None)
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
    run("cargo", &locked, None)
}

fn pnpm(cwd: impl AsRef<Path>, arguments: &[&str]) -> bool {
    run("pnpm", arguments, Some(cwd.as_ref()))
}

fn run(program: &str, arguments: &[&str], cwd: Option<&Path>) -> bool {
    eprintln!("$ {program} {}", arguments.join(" "));

    let attempt = |name: &str| {
        let mut command = Command::new(name);
        command.args(arguments);
        if let Some(dir) = cwd {
            command.current_dir(dir);
        }
        command.status()
    };

    // npm and corepack install pnpm on Windows as `pnpm.cmd`, a batch shim, and
    // the only extension `Command` fills in for a bare name is `.exe`.
    match attempt(program).or_else(|_| attempt(&format!("{program}.cmd"))) {
        Ok(status) => status.success(),
        Err(error) => {
            eprintln!("could not run {program}: {error}");
            false
        }
    }
}
