//! Singular developer entrypoint.
//!
//! It shells out; everything here is something you could type yourself. The
//! point is that the order and the gates are written down once, and that CI
//! runs them by name rather than keeping its own copy.

use std::path::Path;
use std::process::{Command, ExitCode};

/// The hooks this repository tracks, which git has to be pointed at.
const HOOKS: &str = ".githooks";

/// The profile the size run builds under, and the directory it lands in.
const CRUNCH: &str = "crunch";

/// What the size run adds on top of the profile, all of it nightly.
///
/// Rebuilding the standard library is where the remaining bytes are: the one
/// cargo ships is compiled for speed and carries panic machinery this never
/// reaches. `optimize_for_size` picks the small codepath inside it.
const CRUNCH_UNSTABLE: &[&str] = &[
    "-Z",
    "build-std=std,panic_abort",
    "-Z",
    "build-std-features=optimize_for_size",
];

/// What rustc gets that the profile cannot say.
///
/// `immediate-abort` is a panic strategy nightly understands and stable rejects
/// outright, so it travels as a flag: a `Cargo.toml` stable cannot parse would
/// break every other task here. The other two drop panic locations and the
/// `Debug` formatting these binaries never print.
const CRUNCH_RUSTFLAGS: &str =
    "-Zlocation-detail=none -Zfmt-debug=none -Zunstable-options -Cpanic=immediate-abort";

/// Every window this workspace can open, which is what a size run covers.
const APPLICATIONS: &[&str] = &[
    "example-hello",
    "example-session",
    "example-todos",
    "topcoat-probe",
];

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

/// The two libraries somebody outside this repository would build on.
///
/// Not everything under `crates/`: `example-harness` is test scaffolding for
/// the examples, and the rest of the workspace is binaries with private items.
/// Neither has anything rustdoc could say beyond the source.
const DOCUMENTED: &[&str] = &["-p", "custom-protocol-http", "-p", "tauri-plugin-topcoat"];

/// What rustdoc gets beyond `cargo doc`'s own flags.
const RUSTDOCFLAGS: &[&str] = &[
    "-Z",
    "unstable-options",
    "--enable-index-page",
    "--generate-link-to-definition",
    "--extern-html-root-takes-precedence",
    "--default-setting",
    "preferred-dark-theme=ayu",
    // enables the `doc_cfg` gate, making feature badges
    "--cfg",
    "docsrs",
];

fn help() {
    eprintln!(
        "cargo xtask <task>

  bindings   hold the probe's TypeScript to its Rust types, rewriting it if not
  probe      run the conformance probe (--exit to quit once it reports)

  hello      the least that renders over the protocol
  session    sessions, with the token held out of the webview
  todos      topcoat's toasty example, persisting to SQLite
  showcase   all three and then the probe, each up until you close it
  crunch     every application, as small as nightly can make it, with sizes

  lint       fmt, the page's own two, clippy over every feature, and the docs
  test       the suites, the doctests, then the page against its bindings
  check      lint then test, which between them are what CI runs
  docs       the libraries' documentation as docs.rs would build it (--open)
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
        "todos" => example("example-todos"),
        "showcase" => showcase(),
        "crunch" => crunch(),

        "lint" => lint(),
        "test" => test(),
        "check" => lint() && test(),
        "hooks" => hooks(),

        "docs" => docs(&rest),

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

/// The documentation that gets published.
///
/// Neither library reaches crates.io, so this builds what docs.rs would have
/// and the `docs` workflow serves it from GitHub Pages.
///
/// Nightly, for [`RUSTDOCFLAGS`] and for the `-Z rustdoc-map` below.
///
/// The gate is still `lint`; nothing here is asked to catch a regression.
fn docs(rest: &[&str]) -> bool {
    // The root page lists whatever is in the output directory, and `lint`
    // documents the whole workspace into it. Left alone, the index would
    // name xtask.
    if let Err(error) = std::fs::remove_dir_all("target/doc")
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("could not clear target/doc: {error}");
        return false;
    }

    // `+nightly` has to lead, so this skips the `cargo` helper.
    let mut doc = vec!["+nightly", "--locked", "doc", "--no-deps", "--all-features"];
    doc.extend_from_slice(DOCUMENTED);

    // `--no-deps` would otherwise leave every `http::Request` in a signature
    // as plain text. Point them at docs.rs, which has them.
    doc.extend_from_slice(&[
        "-Z",
        "rustdoc-map",
        "--config",
        r#"doc.extern-map.registries.crates-io="https://docs.rs/""#,
    ]);
    doc.extend_from_slice(rest);

    let flags = RUSTDOCFLAGS.join(" ");
    run("cargo", &doc, None, &[("RUSTDOCFLAGS", flags.as_str())])
}

/// Runs one example application.
///
/// Release, because these are windows somebody is going to interact with and
/// the debug build of a webview application feels it.
fn example(package: &str) -> bool {
    cargo(&["run", "--release", "-p", package])
}

/// Every application, built as small as this workspace knows how.
///
/// Nightly, and it needs `rust-src`: `rustup component add rust-src --toolchain
/// nightly`. Between the profile and [`CRUNCH_UNSTABLE`] this is a cold build
/// of the standard library under fat LTO, which is minutes. That is the whole
/// reason `release` does none of it.
///
/// Prints the size of each binary, since that is the only reason to run it.
fn crunch() -> bool {
    // `-Z build-std` will not rebuild the standard library for an implied
    // target, so the host has to be named.
    let Some(host) = host() else {
        eprintln!("could not read the host triple out of `rustc -vV`");
        return false;
    };

    // `+nightly` has to lead, so this skips the `cargo` helper.
    let mut arguments = vec![
        "+nightly",
        "--locked",
        "build",
        "--profile",
        CRUNCH,
        "--target",
        &host,
    ];
    arguments.extend_from_slice(CRUNCH_UNSTABLE);
    for application in APPLICATIONS {
        arguments.extend_from_slice(&["-p", application]);
    }

    if !run(
        "cargo",
        &arguments,
        None,
        &[("RUSTFLAGS", CRUNCH_RUSTFLAGS)],
    ) {
        return false;
    }

    for application in APPLICATIONS {
        let binary = format!("{application}{}", std::env::consts::EXE_SUFFIX);
        let built = Path::new("target").join(&host).join(CRUNCH).join(binary);
        match std::fs::metadata(&built) {
            Ok(file) => eprintln!("{application:<16} {:>6.2} MiB", mib(file.len())),
            Err(error) => eprintln!("{application:<16} {}: {error}", built.display()),
        }
    }
    true
}

/// A size in MiB, which is the unit a desktop binary is argued in.
fn mib(bytes: u64) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a binary size never reaches 2^53"
    )]
    let bytes = bytes as f64;
    bytes / (1024.0 * 1024.0)
}

/// The target triple this machine builds for, out of `rustc -vV`.
fn host() -> Option<String> {
    let reported = Command::new("rustc").arg("-vV").output().ok()?;
    let reported = String::from_utf8(reported.stdout).ok()?;
    reported
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
}

/// Every window the workspace has, in one sitting.
///
/// Each one blocks until you close it, and closing it is the only way on. The
/// probe is last and gets no `--exit`, so the run ends with its table up.
fn showcase() -> bool {
    eprintln!("four windows, in order. close each to move on; the probe is last and stays up.");

    example("example-hello") && example("example-session") && example("example-todos") && probe(&[])
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
    run("git", &["config", "core.hooksPath", HOOKS], None, &[])
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
    run("cargo", &locked, None, &[])
}

fn pnpm(cwd: impl AsRef<Path>, arguments: &[&str]) -> bool {
    run("pnpm", arguments, Some(cwd.as_ref()), &[])
}

fn run(program: &str, arguments: &[&str], cwd: Option<&Path>, env: &[(&str, &str)]) -> bool {
    let exported: String = env
        .iter()
        .map(|(name, value)| format!("{name}='{value}' "))
        .collect();
    eprintln!("$ {exported}{program} {}", arguments.join(" "));

    let attempt = |name: &str| {
        let mut command = Command::new(name);
        command.args(arguments);
        command.envs(env.iter().copied());
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
