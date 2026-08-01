#![forbid(unsafe_code)]

//! HTTP semantics for a webview custom protocol.
//!
//! A webview reaches a custom protocol with a URL whose shape depends on the
//! platform, and reads the answer with a client that is not quite an HTTP
//! client. A server behind that protocol needs the ordinary web it was written
//! against. This crate is the translation.
//!
//! # Why is this its own crate?
//!
//! None of it is specific to a shell, and every rule is a plain function over
//! plain values - tested exhaustively without opening a window, on any
//! operating system. No specification is written for a custom protocol
//! handler, so the rules start from what the `probe` binary measured and aim at
//! the layer where a standard already holds: a canonical origin an ordinary
//! CSRF check can read.
//!
//! # The decision
//!
//! [`Origins`] turns a request the webview delivered into one the server may
//! serve: refusing any URL that names somebody else's origin, and rewriting the
//! rest into the single canonical origin the server sees on every platform.
//! [`Platform`] is the only place in the crate that names an operating system.
//!
//! # What it does not do
//!
//! It attaches no credential - no cookie jar, no token - so a server may still
//! reason that a request with neither `Origin` nor `Sec-Fetch-Site` has no
//! ambient authority to forge with. Supply one here and that reasoning is
//! false; ambient authority belongs to the shell, per webview, or to nobody.
//!
//! # Why `Sec-Fetch-Site` is never there
//!
//! Not a webview defect, and not something a later version fixes. Fetch
//! Metadata is appended only to a [potentially trustworthy URL][trustworthy],
//! and `scheme://localhost` has an [opaque origin][origin] - which that
//! algorithm answers "Not Trustworthy". So on macOS, iOS and Linux the headers
//! cannot arrive. That opaque origin is also why [`Origin`] cannot be a thin
//! wrapper around `url::Origin`.
//!
//! [`Platform::HttpSubdomain`] differs: a host ending in `.localhost` *is*
//! potentially trustworthy, so Windows and Android should send Fetch Metadata
//! where the others cannot. Design against the weakest case: `Origin` alone,
//! compared against `Host`.
//!
//! [trustworthy]: https://w3c.github.io/webappsec-secure-contexts/#potentially-trustworthy-url
//! [origin]: https://url.spec.whatwg.org/#concept-url-origin

mod origin;

pub use origin::{CanonicalRequest, Denial, Origin, OriginError, Origins, Outcome, Platform};

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Origins>();
    assert_send_sync::<Origin>();
    assert_send_sync::<Denial>();
};
