//! Session tokens held in this process instead of in the webview.
//!
//! [`TokenStore`] is topcoat's seam for deciding where a session token lives
//! between requests, and this is the implementation that keeps it here rather
//! than in a cookie WebKit would throw away. Why that is worth doing, and what
//! it costs, is argued once on [`Builder::sessions`](crate::Builder::sessions).
//!
//! # The two invariants
//!
//! Both live in `Webviews::read`, and both fail closed.
//!
//! **A token goes only to the webview it was issued to.** The map is keyed by
//! the label Tauri puts on every protocol request, which is the one identifier
//! the shell knows for certain, as against a header the webview may or may not
//! have sent.
//!
//! **A token goes only to a webview showing one of our own documents.**
//! Navigation confinement normally makes the alternative unreachable, but an
//! application can turn confinement off, so this does not assume it is on.

use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

use topcoat::{
    context::Cx,
    router::extensions,
    session::{Token, TokenStore, TokenStoreFuture},
};

/// Which rule in [`Webviews::read`] declined.
///
/// An enum rather than a bare [`None`], so a new rule has to name itself before
/// it compiles, and so a test can say which one fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Withheld {
    UnknownWebview,
    ShowingAnotherOrigin,
    NoToken,
    Expired,
}

/// Which webview a request came from, carried where the webview cannot reach.
///
/// An `http::Extensions` entry is process-side data with no wire representation,
/// so a document inside the webview cannot supply, forge or observe one. The
/// protocol handler overwrites it on every request.
#[derive(Debug, Clone)]
struct RequestWebview(String);

/// What the shell knows about each webview: where it has been, and what it
/// holds.
#[derive(Debug)]
pub(crate) struct Webviews {
    state: Mutex<HashMap<String, Webview>>,
}

#[derive(Debug, Default)]
struct Webview {
    /// Whether the last navigation observed for this webview was to our own
    /// origin. `false` until one is, so a webview nobody has watched is not
    /// handed a token.
    showing_ours: bool,
    held: Option<Held>,
}

#[derive(Debug)]
struct Held {
    token: Token,
    /// `None` when the lifetime could not be added to the current instant,
    /// which is a lifetime long enough that the process will not outlive it.
    expires: Option<Instant>,
}

impl Webviews {
    pub(crate) fn new() -> Webviews {
        Webviews {
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Records whether a webview is now showing one of our documents.
    pub(crate) fn observe(&self, label: &str, ours: bool) {
        let mut state = self.lock();
        let webview = state.entry(label.to_owned()).or_default();
        webview.showing_ours = ours;
        // The token stays: what changes is only whether it is handed out while
        // the webview is away.
    }

    /// The token to present for this request.
    ///
    /// Names the rule that declined rather than returning a bare `None`: all
    /// four fail closed and look identical from outside, where each is just a
    /// request that was not signed in.
    fn read(&self, label: &str, now: Instant) -> Result<Token, Withheld> {
        let state = self.lock();
        let webview = state.get(label).ok_or(Withheld::UnknownWebview)?;
        if !webview.showing_ours {
            return Err(Withheld::ShowingAnotherOrigin);
        }
        let held = webview.held.as_ref().ok_or(Withheld::NoToken)?;
        if held.expires.is_some_and(|expires| now >= expires) {
            return Err(Withheld::Expired);
        }
        Ok(held.token.clone())
    }

    fn write(&self, label: &str, token: Token, max_age: Duration, now: Instant) {
        let held = Held {
            token,
            expires: now.checked_add(max_age),
        };
        self.lock().entry(label.to_owned()).or_default().held = Some(held);
    }

    fn delete(&self, label: &str) {
        if let Some(webview) = self.lock().get_mut(label) {
            webview.held = None;
        }
    }

    /// A poisoned map is recovered rather than propagated: every mutation here
    /// is a single field assignment, so the contents are sound, and logging a
    /// window out because an unrelated task panicked would be its own bug.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Webview>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The [`TokenStore`] that keeps the token in this process.
///
/// Installed by [`Builder::sessions`](crate::Builder::sessions), which is the
/// only way to obtain one: the store and the shell have to be looking at the
/// same state, and letting an application wire that up itself is an invitation
/// to wire it up wrongly.
#[derive(Debug)]
pub(crate) struct WebviewTokenStore {
    webviews: std::sync::Arc<Webviews>,
}

impl WebviewTokenStore {
    pub(crate) const fn new(webviews: std::sync::Arc<Webviews>) -> WebviewTokenStore {
        WebviewTokenStore { webviews }
    }
}

/// Names the requesting webview on a request bound for the router.
///
/// Unconditional, so a value from anywhere else is replaced rather than
/// trusted. Free rather than a method on [`Webviews`], because naming the
/// asker needs nothing the map knows - only [`requesting_webview`], its one
/// reader, does.
pub(crate) fn attach<B>(mut request: http::Request<B>, label: &str) -> http::Request<B> {
    request
        .extensions_mut()
        .insert(RequestWebview(label.to_owned()));
    request
}

/// The webview a request came from, as the protocol handler named it.
///
/// `None` means the request did not come through this plugin, which the shell
/// makes unreachable and which is therefore treated as "no session" rather than
/// guessed at.
fn requesting_webview(cx: &Cx) -> Option<&str> {
    extensions(cx)
        .get::<RequestWebview>()
        .map(|webview| webview.0.as_str())
}

impl TokenStore for WebviewTokenStore {
    fn read<'a>(&'a self, cx: &'a Cx) -> TokenStoreFuture<'a, Option<Token>> {
        // Resolved before the future is built, so the lock is never held across
        // a yield point.
        let token = requesting_webview(cx).and_then(|label| self.webviews.read(label, now()).ok());
        Box::pin(async move { Ok(token) })
    }

    fn write<'a>(
        &'a self,
        cx: &'a Cx,
        token: Token,
        max_age: Duration,
    ) -> TokenStoreFuture<'a, ()> {
        if let Some(label) = requesting_webview(cx) {
            self.webviews.write(label, token, max_age, now());
        }
        Box::pin(async move { Ok(()) })
    }

    fn delete<'a>(&'a self, cx: &'a Cx) -> TokenStoreFuture<'a, ()> {
        if let Some(label) = requesting_webview(cx) {
            self.webviews.delete(label);
        }
        Box::pin(async move { Ok(()) })
    }
}

/// The monotonic clock, and the only one in this crate. Every rule that depends
/// on time takes the instant as a value.
fn now() -> Instant {
    Instant::now()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LABEL: &str = "main";

    fn webviews_showing_ours() -> Webviews {
        let webviews = Webviews::new();
        webviews.observe(LABEL, true);
        webviews
    }

    fn bytes(token: &Token) -> [u8; 32] {
        *token.dangerous_as_array()
    }

    #[test]
    fn a_written_token_reads_back() {
        let webviews = webviews_showing_ours();
        let token = Token::random();
        let now = Instant::now();
        webviews.write(LABEL, token.clone(), Duration::from_secs(60), now);

        let read = webviews
            .read(LABEL, now)
            .expect("the token was just written");
        assert_eq!(bytes(&read), bytes(&token));
    }

    /// By name, not by absence: a test checking only for absence passes while
    /// the wrong rule does the work.
    #[track_caller]
    fn assert_withheld(result: Result<Token, Withheld>, expected: Withheld) {
        match result {
            Err(actual) => assert_eq!(actual, expected, "the wrong rule declined"),
            Ok(_) => panic!("handed over the token, expected {expected:?}"),
        }
    }

    #[test]
    fn a_webview_with_no_token_reads_none() {
        let webviews = webviews_showing_ours();
        assert_withheld(webviews.read(LABEL, Instant::now()), Withheld::NoToken);
    }

    #[test]
    fn one_webviews_token_is_not_anothers() {
        let webviews = webviews_showing_ours();
        webviews.observe("second", true);
        let now = Instant::now();
        webviews.write(LABEL, Token::random(), Duration::from_secs(60), now);

        assert_withheld(webviews.read("second", now), Withheld::NoToken);
    }

    #[test]
    fn a_webview_showing_a_foreign_document_is_not_handed_the_token() {
        let webviews = webviews_showing_ours();
        let now = Instant::now();
        webviews.write(LABEL, Token::random(), Duration::from_secs(60), now);
        assert!(webviews.read(LABEL, now).is_ok());

        webviews.observe(LABEL, false);
        assert_withheld(webviews.read(LABEL, now), Withheld::ShowingAnotherOrigin);

        webviews.observe(LABEL, true);
        assert!(webviews.read(LABEL, now).is_ok());
    }

    #[test]
    fn a_webview_nobody_has_watched_is_not_handed_the_token() {
        let webviews = Webviews::new();
        let now = Instant::now();
        webviews.write(LABEL, Token::random(), Duration::from_secs(60), now);
        assert_withheld(webviews.read(LABEL, now), Withheld::ShowingAnotherOrigin);
    }

    #[test]
    fn an_expired_token_reads_none() {
        let webviews = webviews_showing_ours();
        let now = Instant::now();
        webviews.write(LABEL, Token::random(), Duration::from_secs(60), now);

        let later = now + Duration::from_secs(61);
        assert_withheld(webviews.read(LABEL, later), Withheld::Expired);
    }

    #[test]
    fn a_token_expires_at_its_deadline_rather_than_after_it() {
        let webviews = webviews_showing_ours();
        let now = Instant::now();
        let lifetime = Duration::from_secs(60);
        webviews.write(LABEL, Token::random(), lifetime, now);

        assert_withheld(webviews.read(LABEL, now + lifetime), Withheld::Expired);
        assert!(
            webviews
                .read(LABEL, now + lifetime - Duration::from_nanos(1))
                .is_ok()
        );
    }

    #[test]
    fn deleting_discards_the_token_and_keeps_the_webview() {
        let webviews = webviews_showing_ours();
        let now = Instant::now();
        webviews.write(LABEL, Token::random(), Duration::from_secs(60), now);
        webviews.delete(LABEL);

        assert!(webviews.read(LABEL, now).is_err());
        webviews.write(LABEL, Token::random(), Duration::from_secs(60), now);
        assert!(webviews.read(LABEL, now).is_ok());
    }

    #[test]
    fn a_rewrite_replaces_the_previous_token() {
        let webviews = webviews_showing_ours();
        let now = Instant::now();
        webviews.write(LABEL, Token::random(), Duration::from_secs(60), now);
        let rotated = Token::random();
        webviews.write(LABEL, rotated.clone(), Duration::from_secs(60), now);

        let read = webviews.read(LABEL, now).expect("a token is held");
        assert_eq!(bytes(&read), bytes(&rotated));
    }

    #[test]
    fn attach_overwrites_whatever_was_there() {
        let mut request = http::Request::new(Vec::<u8>::new());
        request
            .extensions_mut()
            .insert(RequestWebview("forged".to_owned()));

        let request = attach(request, LABEL);

        let attached = request
            .extensions()
            .get::<RequestWebview>()
            .expect("attach inserts one");
        assert_eq!(attached.0, LABEL);
    }
}
