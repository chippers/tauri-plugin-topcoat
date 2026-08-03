//! A topcoat application over toasty, in a Tauri window, with no port bound.
//!
//! This is topcoat's own `examples/toasty-todo` - the same model, the same four
//! routes, the same components - put behind a custom protocol with its database
//! in a file where a desktop application's data belongs. It renders on the
//! server, persists with an ORM, and never opens a socket in either direction.
//!
//! ```text
//! cargo run -p example-todos
//! ```
//!
//! Three things force a real change rather than a transcription, and each one
//! is argued where it happens:
//!
//! * the window enforces a `Content-Security-Policy`, so upstream's
//!   `onchange="this.form.submit()"` and its inline `style=` attributes cannot
//!   survive - see `POLICY` and `toggle_checkbox` in [`app`];
//! * `push_schema` fails on the second launch against a file, which upstream
//!   never discovers because its database is in memory - see `ready` in
//!   [`store`];
//! * the database path only exists once Tauri is running, while the router has
//!   to be finished before the plugin is built - which is what `main` below is
//!   for, and the only genuinely awkward part.
//!
//! Every decision the plugin makes prints as it happens: the origin rewrite,
//! and the redirect it follows in-process so that Post/Redirect/Get works in a
//! webview that follows none.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod store;

use store::Store;
use tauri::Manager;

/// The database file, under the application's own data directory.
const DATABASE: &str = "todos.db";

fn main() {
    // Debug, because the redirect the transport follows in-process lives there
    // and watching it happen is half the reason to run this.
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    tauri::Builder::default()
        .setup(|app| {
            // Every window in `tauri.conf.json` is declared `"create": false`,
            // because Tauri creates configured windows before this hook runs -
            // and a webview asking for `/` before the protocol is registered
            // gets nothing. Registering first and building the window second is
            // the whole reason this is not four lines in `main`.
            let path = app.path().app_data_dir()?.join(DATABASE);

            // A plain block, because `setup` runs on the main thread and
            // outside Tauri's runtime. It also forces that runtime into
            // existence, which is the one the protocol handler spawns onto.
            let store = tauri::async_runtime::block_on(Store::open(&path))?;

            app.handle().plugin(app::plugin(store).build()?)?;

            // The exact complement of what Tauri already did before this hook,
            // so a `create: true` window added later cannot be built twice.
            for window in app.config().app.windows.iter().filter(|w| !w.create) {
                tauri::WebviewWindowBuilder::from_config(app.handle(), window)?.build()?;
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the application runs");
}
