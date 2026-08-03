//! What this crate reports, and the only place its `tracing` feature is `cfg`ed.
//!
//! Decisions only. Nothing here reads a body, a header value or a query string:
//! a refusal is described by what was refused, which is a URL's authority or a
//! capability's name, and neither is a secret.

#[cfg(feature = "tower")]
use crate::{Denial, Unsupported};

#[cfg(feature = "tower")]
pub(crate) fn refused_foreign_origin(denial: &Denial) {
    #[cfg(feature = "tracing")]
    tracing::warn!(reason = %denial, "refused a request for another origin");
    #[cfg(not(feature = "tracing"))]
    let _ = denial;
}

#[cfg(feature = "tower")]
pub(crate) fn refused_unsupported(unsupported: &Unsupported) {
    #[cfg(feature = "tracing")]
    tracing::warn!(capability = %unsupported, "refused a response this transport cannot carry");
    #[cfg(not(feature = "tracing"))]
    let _ = unsupported;
}

/// The platform origin is the one thing invisible from inside the application,
/// and the first thing worth knowing when a route behaves differently on
/// Windows.
///
/// Taken unrendered, so it is spelled out only when somebody is listening.
pub(crate) fn rewrote_origin(from: &crate::Origin, path: &str) {
    #[cfg(feature = "tracing")]
    tracing::debug!(
        platform_origin = %from,
        path,
        "rewrote a request onto the canonical origin"
    );
    #[cfg(not(feature = "tracing"))]
    let _ = (from, path);
}
