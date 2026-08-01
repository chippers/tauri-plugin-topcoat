//! The one channel the probe does not run over its custom protocol.
//!
//! Tauri's IPC is separate from anything served, and a page that arrived over a
//! custom scheme still has to be able to reach it. That is the question a
//! server-rendered application has to answer before it can touch a single
//! native capability: the framework renders the page, and Tauri's own APIs have
//! to still be there when it does.
//!
//! Asked three times, because one answer would not mean anything. From the main
//! window, from a window whose document carries a `Content-Security-Policy` -
//! which is what an application that sets headers at all would send - and from
//! a subframe. Only the set says whether a failure was the policy or the frame.
//!
//! This is also where the probe's TypeScript comes from. The same builder that
//! registers the command exports every type in [`crate::report`], so the page
//! never describes one of those shapes a second time.

use serde::Serialize;
use specta::Type;
use tauri::{App, WebviewUrl, WebviewWindowBuilder, Window};
use tauri_specta::{Builder, collect_commands};

use crate::clock::Clock;
use crate::report::{Answer, Probe, Sheet};

/// What a Tauri command saw when the page invoked it.
#[derive(Debug, Serialize, Type)]
pub struct Invoked {
    /// The token the page sent, back verbatim. Getting its own token back is
    /// how the page knows the round trip carried an argument intact.
    pub echoed: String,
    /// The label of the window the call came from.
    ///
    /// `tauri-plugin-topcoat` hangs a session on exactly this identity, so a
    /// command that could not tell which window called it would sink the whole
    /// arrangement.
    pub window: String,
}

/// Answers an `invoke` from the page.
#[tauri::command]
#[specta::specta]
pub fn probe_ipc(window: Window, sent: String) -> Invoked {
    Invoked {
        echoed: sent,
        window: window.label().to_owned(),
    }
}

/// The label the policed window is known by.
///
/// Shared, because [`crate::route`] closes it the moment it reports and a label
/// that drifted would leave it sitting on top of the table forever.
pub const POLICED: &str = "policed";

/// Opens a second window on a document served with a policy.
///
/// A window and not a frame, because a frame answers a different question:
/// Tauri injects the IPC bridge into a window's main document, so a subframe
/// fails whether or not a policy is involved. The probe asks both - the frame
/// from the page, this window from here - and only the pair says which of the
/// two a failure was.
///
/// A framework that renders its own responses sets its own headers, so this is
/// the case that decides whether such an application keeps its native
/// capabilities. It is worth a whole window to answer properly.
///
/// # Errors
///
/// Whatever [`WebviewWindowBuilder::build`] returns when the window cannot be
/// created.
pub fn open_the_policed_window(app: &App) -> tauri::Result<()> {
    let url = format!(
        "{}://localhost/ipc/{}/",
        crate::SCHEME,
        Probe::IpcInvokeUnderCsp.id()
    )
    .parse()
    .expect("a scheme and a path make a URL");

    WebviewWindowBuilder::new(app, POLICED, WebviewUrl::CustomProtocol(url))
        .title("ipc under a policy")
        .inner_size(360.0, 120.0)
        .build()?;

    Ok(())
}

/// The command set, and the types that cross to the page by other means.
///
/// [`Sheet`], [`Answer`] and [`Clock`] appear in no command signature - they
/// travel over the custom protocol as JSON - so they are registered by hand.
/// The export is the same either way, which is the point: one boundary, one
/// generated file, whichever channel a value happened to arrive on. `Row` is
/// not named here because `Sheet` is made of them and comes and gets it.
pub fn builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new()
        .commands(collect_commands![probe_ipc])
        .typ::<Sheet>()
        .typ::<Answer>()
        .typ::<Clock>()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use specta_typescript::Typescript;

    /// Where the generated TypeScript lands. Resolved at compile time, so it
    /// does not depend on the directory the test was run from.
    const BINDINGS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/ui/src/bindings.ts");

    /// Holds the committed TypeScript to what the Rust types generate.
    ///
    /// It writes the file when they differ and then fails, so the fix is
    /// already applied by the time anyone reads why: run it again and it
    /// passes, commit the diff and CI passes. This is the whole of the drift
    /// gate - there is no second place recording where the file lives, and
    /// nothing to remember to run.
    #[test]
    fn test_the_bindings_are_what_the_rust_generates() {
        let scratch = std::env::temp_dir().join("topcoat-probe-bindings.ts");
        super::builder()
            .export(Typescript::default(), &scratch)
            .expect("the TypeScript bindings export");

        let generated = fs::read_to_string(&scratch).expect("the export is readable");
        let committed = fs::read_to_string(BINDINGS).unwrap_or_default();
        if generated == committed {
            return;
        }

        fs::write(BINDINGS, &generated).expect("the bindings are writable");
        panic!(
            "{BINDINGS} did not match the Rust types, and has been rewritten to match. Commit it."
        );
    }
}
