#![cfg_attr(docsrs, feature(doc_cfg))]
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
//! CSRF check can read, redirect following taken from `tower-http`.
//!
//! # The two decisions
//!
//! [`Origins`] turns a request the webview delivered into one the server may
//! serve: refusing any URL that names somebody else's origin, and rewriting the
//! rest into the single canonical origin the server sees on every platform.
//! [`Platform`] is the only place in the crate that names an operating system.
//!
//! [`unsupported`] reads the answer back, and names the capabilities a protocol
//! handler cannot deliver - streaming, compression, cookies, upgrades - so the
//! shell can fail where a developer will see it rather than drop half a response
//! in silence.
//!
//! # Applying them
//!
//! [`tower`] stacks both as middleware in front of any service, which is how a
//! shell actually wants them, and adds the third job from the ecosystem rather
//! than from here: no webview follows a `Location` from a custom protocol, and
//! `tower-http` already knows how to follow one.
//!
//! # What it does not do
//!
//! It attaches no credential - no cookie jar, no token - and strips what a
//! client attached by itself, so a server may still reason that a request with
//! neither `Origin` nor `Sec-Fetch-Site` has no ambient authority to forge
//! with. Supply one here and that reasoning is false; ambient authority belongs
//! to the shell, per webview, or to nobody. `Host` is the exception, and is set
//! because a server comparing `Origin` against it needs both.
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
#[cfg(feature = "tower")]
pub mod tower;
mod unsupported;

pub use origin::{CanonicalRequest, Denial, Origin, OriginError, Origins, Outcome, Platform};
pub use unsupported::{Unsupported, unsupported};

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Origins>();
    assert_send_sync::<Origin>();
    assert_send_sync::<Denial>();
    assert_send_sync::<Unsupported>();

    // A layer is built once and shared, so the same promise holds for it. The
    // failure this catches is remote: a non-`Send` field added here surfaces as
    // an unsatisfied bound wherever somebody spawns the stack.
    #[cfg(feature = "tower")]
    {
        assert_send_sync::<tower::CanonicalOriginLayer>();
        assert_send_sync::<tower::RefuseUnsupportedLayer>();
    }
};
