//! The smallest thing this plugin can serve.
//!
//! A page, a component, and the wiring that puts them in a window. Nothing here
//! is about the transport, which is the point: a topcoat application does not
//! know it is not on a socket. What the plugin does to a request is argued in
//! the root README, demonstrated in `examples/session`, and measured in `probe`.
//!
//! ```text
//! cargo run -p example-hello
//! ```
//!
//! This is topcoat's own `examples/hello-world`, with the plugin in place of
//! `topcoat::start`, no `topcoat::dev::script()`, and one test upstream has no
//! reason to carry.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use topcoat::{
    Result,
    router::{Router, RouterBuilderDiscoverExt, page},
    view::{component, view},
};

fn main() {
    let plugin = plugin()
        .build()
        .expect("the plugin is configured correctly");

    tauri::Builder::default()
        .plugin(plugin)
        .run(tauri::generate_context!())
        .expect("the application runs");
}

/// The application and its transport, as one value.
///
/// Where `topcoat::start` would bind a port. The router is the same value
/// either way; only what calls it changes.
///
/// It is a function rather than four lines inside `main` so the test below
/// drives the configuration the window does.
fn plugin() -> tauri_plugin_topcoat::Builder {
    tauri_plugin_topcoat::Builder::new(Router::builder().discover())
}

#[page("/")]
async fn home() -> Result {
    // This page is rendered when the window requests the root route (`/`).
    view! {
        <!DOCTYPE html>
        <html>
            <head>
                <title>"Hello world"</title>

                // No `topcoat::dev::script()`. It reloads the page once a new
                // build is serving, and a rebuild restarts this process - so
                // the window is already new and holds nothing stale to
                // discard.
            </head>
            <body>hello(name: "World")</body>
        </html>
    }
}

#[component]
async fn hello(name: &str) -> Result {
    // Components can accept arguments and render reusable HTML.
    view! {
        <h1>
            "Hello, "
            (name)
            "!"
        </h1>
    }
}

/// One assertion, because compiling is not rendering.
///
/// [`Session`](tauri_plugin_topcoat::Session) drives the whole transport with
/// no window, so this is what the webview would have been handed. Without it a
/// page that routed nowhere would still pass CI.
#[cfg(test)]
mod tests {
    use example_harness::get;
    use tauri_plugin_topcoat::Platform;
    use topcoat::router::StatusCode;

    use super::*;

    #[tokio::test]
    async fn the_page_renders() {
        let window = plugin()
            .session(Platform::Scheme)
            .expect("the plugin is configured correctly");

        let (status, body) = get(&window, "/").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<h1>Hello, World!</h1>"), "{body}");
    }
}
