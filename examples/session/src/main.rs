//! topcoat's sessions in a Tauri window, with the token held out of the webview.
//!
//! This is topcoat's own `examples/session`, and the diff is the plugin's whole
//! argument about sessions. Upstream calls `.cookies()` and hands its
//! `SessionConfig` to topcoat's own `sessions`; here `.cookies()` is gone and
//! the configuration goes to [`tauri_plugin_topcoat::Builder::sessions`]
//! instead. Minting, hashing, expiry, `start` and `stop`, and the application's
//! own session storage all stay where upstream put them.
//!
//! ```text
//! cargo run -p example-session
//! ```
//!
//! Left upstream's way the sign-in only appears to work: topcoat sets a
//! `__Host-` prefixed, `Secure`, `HttpOnly` cookie, WebKit throws away every
//! cookie a custom protocol sets, and the next request arrives logged out. The
//! plugin refuses that response with a `502` rather than let it look fine.
//!
//! Every decision the plugin makes prints as it happens: the origin rewrite,
//! the redirect followed, the token handed over or withheld.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
    time::SystemTime,
};

use serde::Deserialize;
use topcoat::{
    Result,
    context::{Cx, CxBuilder, app_context},
    router::{
        Body, HeaderValue, Next, Response, Router, RouterBuilderDiscoverExt,
        content::Form,
        error::{SeeOther, see_other},
        header, layer, layout, page, route,
    },
    session::{self, SessionConfig, TokenHash},
    view::view,
};

fn main() {
    // Debug, because the token handed over or withheld lives there and watching
    // it is the point of running this.
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

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
/// One function, so the tests below drive the same configuration the window
/// does.
///
/// Upstream this reads `.cookies().sessions(SessionConfig::default())`. The
/// `.cookies()` is gone because no cookie survives this transport in either
/// direction, and moving the configuration onto the plugin changes only where
/// the token lives: in this process, keyed by the webview that asked, never
/// handed to the document at all.
fn plugin() -> tauri_plugin_topcoat::Builder {
    let router = Router::builder()
        .app_context(Database::default())
        .discover();

    tauri_plugin_topcoat::Builder::new(router).sessions(SessionConfig::builder())
}

/// The policy every response leaves with.
///
/// A session is what makes this worth sending. The token is ambient with
/// respect to the webview, so whatever document that webview is showing can
/// spend it; a policy is what decides which documents can get in front of it.
/// Nothing on this page runs a script or fetches a sub-resource, which is what
/// lets `default-src` be `'none'` rather than a list.
///
/// `tauri.conf.json` cannot carry this. Tauri attaches the `csp` it configures
/// to the assets it serves from `tauri://localhost`, and every document here
/// comes from the plugin's protocol instead - so the application sends the
/// header, the same as any server behind this transport would.
///
/// Malformed, it fails the build rather than the first response.
const POLICY: HeaderValue = HeaderValue::from_static(
    "default-src 'none'; \
     form-action 'self'; \
     base-uri 'none'; \
     frame-ancestors 'none'",
);

/// Attaches [`POLICY`] to every response the application renders.
///
/// Redirects included, since the plugin follows those here rather than leaving
/// them to the webview. Not a framework error, though: a `404` propagates past
/// this layer instead of returning through it, and leaves without the header.
/// Those bodies are `text/plain` with nothing in them anybody chose, so the gap
/// is narrow - but it is a gap, not a claim this layer gets to make.
#[layer("/")]
async fn policed(cx: &mut CxBuilder, body: Body, next: Next<'_>) -> Result<Response> {
    let mut response = next.run(cx, body).await?;
    response
        .headers_mut()
        .insert(header::CONTENT_SECURITY_POLICY, POLICY);

    Ok(response)
}

#[layout("/")]
async fn root(slot: Result) -> Result {
    // Wrap every page in a complete HTML document.
    view! {
        <!DOCTYPE html>
        <html>
            <head>
                <title>"Sessions"</title>

                // No `topcoat::dev::script()`. It reloads the page once a new
                // build is serving, and a rebuild restarts this process - so
                // the window is already new and holds nothing stale to
                // discard. [`POLICY`] would refuse its `<script src>` anyway,
                // the dev server being an origin of its own.
            </head>
            <body>(slot?)</body>
        </html>
    }
}

#[page("/")]
async fn home(cx: &Cx) -> Result {
    // Read the current user from the active session, when one exists.
    view! {
        if let Some(user) = current_user(cx).await? {
            <div>
                "currently logged in as: "
                (&user.name)
            </div>

            <form method="POST" action="/logout"><button>"log out"</button></form>
        } else {
            <div>"currently not logged in"</div>

            <form method="POST" action="/login">
                <input name="name" placeholder="Username" required="true">
                <button>"log in"</button>
            </form>
        }
    }
}

// --- API routes -------------------------------------------------------------

#[derive(Deserialize)]
struct LoginForm {
    name: String,
}

#[route(POST "/login")]
async fn login(cx: &Cx, Form(form): Form<LoginForm>) -> Result<SeeOther> {
    // A real application would verify credentials before starting the session.
    let session = session::start(cx).await?;

    // Associate the new session with the submitted user.
    db(cx).create(session, User { name: form.name });

    // Redirect the browser back to the home page. No webview follows a
    // `Location` from a custom protocol, so the plugin follows it in the
    // process and the window is handed the page this names.
    Ok(see_other("/"))
}

#[route(POST "/logout")]
async fn logout(cx: &Cx) -> Result<SeeOther> {
    // Stop the current session and delete its record from the database.
    if let Some(token_hash) = session::stop(cx).await? {
        db(cx).delete(&token_hash);
    }

    Ok(see_other("/"))
}

// --- In-memory demo database ------------------------------------------------

#[derive(Debug, Clone)]
struct User {
    name: String,
}

// Retrieve the database registered as application context.
fn db(cx: &Cx) -> &Database {
    app_context(cx)
}

// Resolve the current session token to a user.
async fn current_user(cx: &Cx) -> Result<Option<User>> {
    let Some(token_hash) = session::token_hash(cx).await? else {
        return Ok(None);
    };

    Ok(db(cx).read(&token_hash))
}

/// A persisted session record containing the authenticated user and expiry.
///
/// The application's half of a session, and the storage topcoat deliberately
/// does not own: it hands back a hash and an expiry and leaves where they live
/// to you, while the plugin holds the token itself.
#[derive(Debug)]
struct Record {
    user: User,
    expires_at: SystemTime,
}

#[derive(Debug, Default)]
struct Database {
    sessions: Mutex<HashMap<TokenHash, Record>>,
}

impl Database {
    fn create(&self, session: session::Session, user: User) {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                session.token_hash,
                Record {
                    user,
                    expires_at: session.expires_at,
                },
            );
    }

    fn read(&self, token_hash: &TokenHash) -> Option<User> {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(token_hash)
            // Ignore expired sessions.
            //
            // This is not the redundant check it looks like; it is the one a
            // desktop application actually needs. The plugin also withholds an
            // expired token, but it measures the deadline with an `Instant`,
            // and on macOS that is `CLOCK_UPTIME_RAW` - it does not advance
            // while the machine is asleep. Close a laptop for a week and the
            // plugin still believes the token is live. `expires_at` is the
            // `SystemTime` topcoat handed us at sign-in, and a wall clock does
            // not stop for a closed lid.
            .filter(|record| record.expires_at > SystemTime::now())
            .map(|record| record.user.clone())
    }

    fn delete(&self, token_hash: &TokenHash) {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(token_hash);
    }
}

/// The application driven the way the webview drives it.
///
/// Origin rewriting, redirect following and the session transport included,
/// with no window in sight.
#[cfg(test)]
mod tests {
    use example_harness::{get, response, submit};
    use tauri_plugin_topcoat::{Platform, Session};
    use topcoat::router::StatusCode;

    use super::*;

    /// One session is one window: the token it holds lives for its lifetime.
    fn window() -> Session {
        plugin()
            .session(Platform::Scheme)
            .expect("the plugin is configured correctly")
    }

    async fn logged_in_window() -> Session {
        let window = window();
        let (status, _) = submit(&window, "/login", "name=ada").await;
        assert_eq!(
            status,
            StatusCode::OK,
            "logging in did not land back on the page"
        );
        window
    }

    #[tokio::test]
    async fn the_page_renders() {
        let (status, body) = get(&window(), "/").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("currently not logged in"), "{body}");
    }

    /// The one this example exists for.
    ///
    /// Upstream the token comes back on a cookie. Here nothing came back at
    /// all, so a second request that still knows who you are proves the plugin
    /// held it.
    #[tokio::test]
    async fn logging_in_survives_the_next_request() {
        let window = logged_in_window().await;

        let (status, body) = get(&window, "/").await;

        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("currently logged in as: ada"),
            "the session was lost: {body}"
        );
    }

    #[tokio::test]
    async fn logging_out_ends_the_session() {
        let window = logged_in_window().await;

        let (status, body) = submit(&window, "/logout", "").await;

        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("currently not logged in"),
            "the session outlived the log out: {body}"
        );
    }

    /// The token is keyed by the webview that asked, so one window's login is
    /// not another's.
    #[tokio::test]
    async fn a_second_window_is_not_logged_in_by_the_first() {
        let first = logged_in_window().await;
        let second = window();

        let (_, body) = get(&second, "/").await;
        assert!(body.contains("currently not logged in"), "{body}");

        let (_, first_body) = get(&first, "/").await;
        assert!(first_body.contains("logged in as: ada"), "{first_body}");
    }

    /// The layer attaches it, not the layout, so it rides what no layout
    /// renders.
    #[tokio::test]
    async fn the_policy_rides_what_the_application_renders() {
        let served = response(&window(), "/").await;

        assert_eq!(
            served.headers().get(header::CONTENT_SECURITY_POLICY),
            Some(&POLICY),
        );
    }
}
