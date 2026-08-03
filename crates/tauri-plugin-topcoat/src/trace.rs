//! What this plugin reports, and the only place its `tracing` feature is
//! `cfg`ed.
//!
//! The token is never reported. Nothing here takes one.

pub(crate) fn record_status(status: http::StatusCode) {
    #[cfg(feature = "tracing")]
    tracing::Span::current().record("status", status.as_u16());
    #[cfg(not(feature = "tracing"))]
    let _ = status;
}

/// Reaches stderr without the feature. A window that silently refuses to
/// navigate is the one failure where saying nothing misleads.
pub(crate) fn blocked_navigation(webview: &str, url: &str) {
    #[cfg(feature = "tracing")]
    tracing::warn!(webview, url, "blocked a navigation off this origin");
    #[cfg(not(feature = "tracing"))]
    eprintln!("tauri-plugin-topcoat: blocked navigation to {url} in webview {webview}");
}

/// Not a refusal - confinement is off and the application asked for this -
/// but it explains the anonymous requests that follow.
pub(crate) fn left_our_origin(webview: &str, url: &str) {
    #[cfg(feature = "tracing")]
    tracing::debug!(
        webview,
        url,
        "a webview is showing another origin; it will not be handed a session"
    );
    #[cfg(not(feature = "tracing"))]
    let _ = (webview, url);
}

pub(crate) fn served_before_ready() {
    #[cfg(feature = "tracing")]
    tracing::error!("a request arrived before the plugin finished starting");
}

/// Which rule in `Webviews::read` declined. An enum rather than a string, so a
/// new rule has to name itself here before it compiles.
#[cfg(feature = "session")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Withheld {
    UnknownWebview,
    ShowingAnotherOrigin,
    NoToken,
    Expired,
}

#[cfg(all(feature = "session", feature = "tracing"))]
impl Withheld {
    const fn reason(self) -> &'static str {
        match self {
            Withheld::UnknownWebview => "unknown webview",
            Withheld::ShowingAnotherOrigin => "showing another origin",
            Withheld::NoToken => "no token held",
            Withheld::Expired => "token expired",
        }
    }
}

#[cfg(feature = "session")]
pub(crate) fn session_withheld(webview: &str, withheld: Withheld) {
    #[cfg(feature = "tracing")]
    tracing::debug!(
        webview,
        reason = withheld.reason(),
        "withheld the session token"
    );
    #[cfg(not(feature = "tracing"))]
    let _ = (webview, withheld);
}

#[cfg(feature = "session")]
pub(crate) fn session_presented(webview: &str) {
    #[cfg(feature = "tracing")]
    tracing::trace!(webview, "presented the session token");
    #[cfg(not(feature = "tracing"))]
    let _ = webview;
}

#[cfg(feature = "session")]
pub(crate) fn session_issued(webview: &str) {
    #[cfg(feature = "tracing")]
    tracing::debug!(webview, "issued a session token");
    #[cfg(not(feature = "tracing"))]
    let _ = webview;
}

#[cfg(feature = "session")]
pub(crate) fn session_cleared(webview: &str) {
    #[cfg(feature = "tracing")]
    tracing::debug!(webview, "discarded the session token");
    #[cfg(not(feature = "tracing"))]
    let _ = webview;
}
