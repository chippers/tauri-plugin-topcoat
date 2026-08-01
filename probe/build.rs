//! Builds the page, then the app.
//!
//! In that order, because `tauri.conf.json` points `frontendDist` at `ui/dist`
//! and Tauri embeds it at compile time. Doing it here rather than asking for it
//! in a README is the difference between a build step and a build step people
//! forget: `cargo run -p topcoat-probe` cannot be run against a stale page.
//!
//! The generated TypeScript is the one thing this cannot do for itself - it
//! comes from a builder that only exists once this crate is compiled - so
//! `cargo xtask bindings` (or any `cargo test`) writes it.
//!
//! Which is why `pnpm run build` transpiles and does not type-check. Changing a
//! Rust type invalidates the page, and if the build refused to proceed until
//! the page type-checked, the only thing that could fix it - a `cargo test`
//! that regenerates the types - could never compile far enough to run. The
//! type-check is a gate, so it lives where gates live: `cargo xtask check`.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// What the page is built from, and what it is built into.
const UI: &str = "ui";
const BUILT: &str = "dist";
const INSTALLED: &str = "node_modules";

fn main() {
    let ui = Path::new(env!("CARGO_MANIFEST_DIR")).join(UI);

    for input in inputs(&ui) {
        println!("cargo::rerun-if-changed={}", input.display());
    }

    if !ui.join(INSTALLED).is_dir() {
        pnpm(&ui, &["install", "--frozen-lockfile"]);
    }
    pnpm(&ui, &["run", "build"]);

    tauri_build::build();
}

/// Every path under `ui` that the build reads, sorted.
///
/// Walked rather than listed, because a file someone adds to `ui/` is exactly
/// the file a hand-written list would forget, and a forgotten input is a page
/// that silently does not rebuild. Directories are included as well as files,
/// so adding one counts as a change too.
fn inputs(ui: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk(ui, &mut found);
    found.sort();
    found
}

fn walk(directory: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };

    for entry in entries.flatten() {
        // The build's own output and its dependencies. Watching either would
        // rebuild forever.
        if entry.file_name() == BUILT || entry.file_name() == INSTALLED {
            continue;
        }

        let path = entry.path();
        if path.is_dir() {
            walk(&path, found);
        }
        found.push(path);
    }
}

fn pnpm(ui: &Path, arguments: &[&str]) {
    let attempt = |name: &str| Command::new(name).args(arguments).current_dir(ui).status();

    // npm and corepack install pnpm on Windows as `pnpm.cmd`, a batch shim, and
    // the only extension `Command` fills in for a bare name is `.exe`.
    let status = attempt("pnpm")
        .or_else(|_| attempt("pnpm.cmd"))
        .unwrap_or_else(|error| {
            panic!(
                "the probe's page is built with pnpm, which could not be run ({error}).\n\
                 Install Node and pnpm - `corepack enable` is enough - or build \
                 only what does not need either: `cargo run -p example-todos`."
            )
        });

    assert!(
        status.success(),
        "`pnpm {}` failed in {}",
        arguments.join(" "),
        ui.display()
    );
}
