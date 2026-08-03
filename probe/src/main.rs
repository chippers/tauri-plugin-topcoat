//! A conformance probe for Tauri custom protocols.
//!
//! The three webviews disagree about what a custom-scheme response means. This
//! app registers a `probe://` protocol, serves its own UI over it, and reports
//! what survived the round trip. Every question it asks is declared in one
//! table, in [`report`]; `README.md` says what to make of the answers.
//!
//! ```text
//! cargo run -p topcoat-probe            # leaves the windows up
//! cargo run -p topcoat-probe -- --exit  # quits once the report is printed
//! ```

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod clock;
mod ipc;
mod report;
mod route;

use std::{
    fmt::Write as _,
    sync::{Arc, mpsc},
    time::Duration,
};

use http::{Request, header};
use tauri::{Runtime, UriSchemeContext, UriSchemeResponder};

use crate::clock::Clockwork;
use crate::report::Report;

/// The scheme the probe serves itself over.
///
/// The webview reaches it as `probe://localhost` on macOS, iOS and Linux, and
/// as `http://probe.localhost` on Windows and Android, where wry rewrites
/// custom schemes onto http.
const SCHEME: &str = "probe";

/// How long to wait for a page that never says it is finished. A wedged webview
/// still has to tell us what it managed, and a run that reports nothing until a
/// human intervenes is a run nobody can script.
const DEADLINE: Duration = Duration::from_secs(90);

/// Long enough for the page to receive the response to its last request before
/// the process goes away, on the runs where it goes away.
const GRACE: Duration = Duration::from_millis(250);

/// Quits once the report is printed, instead of leaving the windows up.
const EXIT: &str = "--exit";

/// Request headers worth a line each. The rest are noise, and the ones that
/// carry a verdict have probes of their own.
const LOGGED: &[header::HeaderName] = &[
    header::ORIGIN,
    header::REFERER,
    header::HOST,
    header::COOKIE,
    header::CONTENT_TYPE,
    header::RANGE,
];

/// Everything one run accumulates, shared by every request.
///
/// `finish` is how the page says it has stopped learning things.
#[derive(Debug)]
struct Run {
    report: Report,
    clock: Clockwork,
    finish: mpsc::SyncSender<()>,
}

fn main() {
    let (finish, finished) = mpsc::sync_channel::<()>(1);
    let run = Arc::new(Run {
        report: Report::new(),
        clock: Clockwork::default(),
        finish,
    });
    let quit = std::env::args().any(|argument| argument == EXIT);

    {
        let run = Arc::clone(&run);
        std::thread::spawn(move || {
            if finished.recv_timeout(DEADLINE).is_ok() {
                std::thread::sleep(GRACE);
            }
            println!("\n{}", run.report.render());
            if quit {
                std::process::exit(0);
            }
        });
    }

    tauri::Builder::default()
        .invoke_handler(ipc::builder().invoke_handler())
        .register_asynchronous_uri_scheme_protocol(SCHEME, move |context, request, responder| {
            serve(context, request, responder, Arc::clone(&run));
        })
        .setup(|app| {
            ipc::open_the_policed_window(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the probe runs");
}

/// Serves one custom-protocol request.
///
/// The response is produced on the async runtime rather than inline, which is
/// the shape a real plugin needs - the router is async - and exercises
/// responding from a thread other than the one the handler was called on.
fn serve<R: Runtime>(
    context: UriSchemeContext<'_, R>,
    request: Request<Vec<u8>>,
    responder: UriSchemeResponder,
    run: Arc<Run>,
) {
    let app = context.app_handle().clone();

    tauri::async_runtime::spawn(async move {
        // Every route but the header clock, which asks once a frame and would
        // otherwise push everything worth reading off the top of the terminal.
        if request.uri().path() != clock::CLOCK {
            println!("{}", log(&request));
        }
        responder.respond(route::route(&request, &run, &app));
    });
}

/// One request, as the server saw it.
fn log(request: &Request<Vec<u8>>) -> String {
    let mut line = format!("[request] {} {}", request.method(), request.uri());
    for name in LOGGED {
        if let Some(value) = request.headers().get(name) {
            let value = short(value.to_str().unwrap_or("<not utf-8>"));
            let _ = write!(line, " {name}={value}");
        }
    }
    if !request.body().is_empty() {
        let body = short(&String::from_utf8_lossy(request.body()));
        let _ = write!(line, " body={body:?}");
    }
    line
}

/// Enough of a value to recognise it.
///
/// A `data:` referer is an entire document url-encoded, and a log nobody can
/// read is a log nobody reads.
fn short(value: &str) -> String {
    const LIMIT: usize = 60;

    let mut short: String = value.chars().take(LIMIT).collect();
    if value.chars().nth(LIMIT).is_some() {
        short.push_str("...");
    }
    short
}
