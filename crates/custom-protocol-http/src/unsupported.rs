//! What a custom protocol cannot deliver, recognised in the server's response.
//!
//! A protocol handler answers with one buffered body and no connection. Several
//! ordinary HTTP capabilities therefore have no path to the webview, and a
//! server that was not told so emits them anyway: the response is well formed,
//! the transport quietly drops part of it, and the application misbehaves with
//! nothing to read in a log.
//!
//! Recognising a capability is a pure function over headers and status, and
//! each rule below tests a header the server actually set. The shell turns that
//! answer into a failure the developer can see.

use http::{HeaderName, HeaderValue, Response, StatusCode, header};
use mime::Mime;

/// A capability the server used and this transport cannot carry.
///
/// Exhaustive on purpose. The probe binary keeps finding these one at a time,
/// and each new one should break every `match` that reports to a developer
/// rather than fall through to a generic message.
///
/// Each carries the header that gave it away, not a sentence about it, so
/// `Display` is the default rendering rather than the only one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsupported {
    /// The response body arrives over time rather than all at once.
    ///
    /// A protocol handler returns bytes, once. Server-sent events, chunked
    /// transfer, and any long-lived body have nowhere to go.
    Streaming {
        /// The header that gave it away.
        header: HeaderName,
        /// What it said.
        value: HeaderValue,
    },
    /// The body is compressed.
    ///
    /// [`Origins::accept`](crate::Origins::accept) removes `Accept-Encoding` on
    /// the way in, so a server that negotiates will not compress. One that
    /// compresses unconditionally, or serves bodies compressed ahead of time,
    /// reaches WKWebView. It does not decode what a custom protocol hands it,
    /// and renders the bytes as text.
    ///
    /// Nothing is lost by storing bodies compressed and decompressing before
    /// answering, because there is no wire here to save the bytes on.
    Compression {
        /// The `Content-Encoding` the server applied.
        encoding: HeaderValue,
    },
    /// The response sets a cookie.
    ///
    /// WebKit drops every `Set-Cookie` a custom protocol emits, so the value
    /// never comes back and the server sees each request as the first.
    SetCookie {
        /// The name of the first cookie the response tried to set.
        name: String,
    },
    /// The response asks to change protocols.
    ///
    /// WebSockets need an HTTP upgrade, and there is no connection under a
    /// protocol handler to upgrade.
    Upgrade,
}

impl core::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Unsupported::Streaming { header, value } => write!(
                f,
                "the response streams (`{header}: {}`), and a custom protocol delivers one \
                 buffered body",
                lossy(value)
            ),
            Unsupported::Compression { encoding } => write!(
                f,
                "the response is `{}`-encoded, and the webview will not decode what a custom \
                 protocol hands it",
                lossy(encoding)
            ),
            Unsupported::SetCookie { name } => write!(
                f,
                "the response sets the cookie `{name}`, and the webview discards cookies from a \
                 custom protocol"
            ),
            Unsupported::Upgrade => f.write_str(
                "the response asks to change protocols, and there is no connection here",
            ),
        }
    }
}

impl core::error::Error for Unsupported {}

/// Recognises the first capability in `response` this transport cannot carry.
///
/// Reported in the order declared by [`Unsupported`], most structural first: a
/// response can trip several rules, and being told it streams is more use than
/// being told it also sets a cookie. Fixing one reveals the next.
#[must_use]
pub fn unsupported<B>(response: &Response<B>) -> Option<Unsupported> {
    let headers = response.headers();

    if let Some(value) = headers.get(header::TRANSFER_ENCODING) {
        return Some(Unsupported::Streaming {
            header: header::TRANSFER_ENCODING,
            value: value.clone(),
        });
    }
    // The only media type whose whole purpose is to stay open.
    if let Some(value) = headers.get(header::CONTENT_TYPE)
        && media_type(value).is_some_and(|media_type| {
            media_type.type_() == mime::TEXT && media_type.subtype() == mime::EVENT_STREAM
        })
    {
        return Some(Unsupported::Streaming {
            header: header::CONTENT_TYPE,
            value: value.clone(),
        });
    }

    if let Some(value) = headers.get(header::CONTENT_ENCODING) {
        return Some(Unsupported::Compression {
            encoding: value.clone(),
        });
    }

    if let Some(value) = headers.get(header::SET_COOKIE) {
        return Some(Unsupported::SetCookie { name: name(value) });
    }

    if response.status() == StatusCode::SWITCHING_PROTOCOLS || headers.contains_key(header::UPGRADE)
    {
        return Some(Unsupported::Upgrade);
    }

    None
}

/// The media type a `Content-Type` names.
///
/// `mime` for the same reason redirects are `tower-http`'s: a grammar written
/// halfway reads right and is subtly wrong. Trimmed first, because a value
/// built in process never went past a parser that would have.
fn media_type(value: &HeaderValue) -> Option<Mime> {
    value.to_str().ok()?.trim().parse().ok()
}

/// The name a `Set-Cookie` value sets, or the whole value when it has no `=`.
fn name(value: &HeaderValue) -> String {
    let value = lossy(value);
    match value.split_once('=') {
        Some((name, _)) => name.trim().to_owned(),
        None => value,
    }
}

/// A header value as text. Values are almost always ASCII; one that is not
/// should still reach the developer rather than become an empty string.
fn lossy(value: &HeaderValue) -> String {
    String::from_utf8_lossy(value.as_bytes()).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Named headers, so a typo does not measure the wrong one and pass.
    fn response(headers: &[(HeaderName, &str)]) -> Response<()> {
        with_status(StatusCode::OK, headers)
    }

    fn with_status(status: StatusCode, headers: &[(HeaderName, &str)]) -> Response<()> {
        let mut builder = Response::builder().status(status);
        for (name, value) in headers {
            builder = builder.header(name, *value);
        }
        builder.body(()).expect("a valid response")
    }

    #[test]
    fn an_ordinary_response_is_supported() {
        let response = response(&[
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CONTENT_LENGTH, "42"),
            (header::CACHE_CONTROL, "no-store"),
        ]);
        assert_eq!(unsupported(&response), None);
    }

    #[test]
    fn chunked_transfer_is_streaming() {
        let response = response(&[(header::TRANSFER_ENCODING, "chunked")]);
        assert_eq!(
            unsupported(&response),
            Some(Unsupported::Streaming {
                header: header::TRANSFER_ENCODING,
                value: HeaderValue::from_static("chunked"),
            })
        );
    }

    #[test]
    fn server_sent_events_are_streaming() {
        let plain = response(&[(header::CONTENT_TYPE, "text/event-stream")]);
        assert_eq!(
            unsupported(&plain),
            Some(Unsupported::Streaming {
                header: header::CONTENT_TYPE,
                value: HeaderValue::from_static("text/event-stream"),
            })
        );
        // Parameters and leading whitespace must not hide it.
        let parameterized =
            response(&[(header::CONTENT_TYPE, " text/event-stream; charset=utf-8")]);
        assert!(matches!(
            unsupported(&parameterized),
            Some(Unsupported::Streaming { .. })
        ));
        // Nor an unusual case, which the grammar says is the same media type.
        let shouted = response(&[(header::CONTENT_TYPE, "TEXT/EVENT-STREAM")]);
        assert!(matches!(
            unsupported(&shouted),
            Some(Unsupported::Streaming { .. })
        ));
    }

    #[test]
    fn a_content_type_that_merely_starts_alike_is_supported() {
        let response = response(&[(header::CONTENT_TYPE, "text/event-streamlined")]);
        assert_eq!(unsupported(&response), None);
    }

    #[test]
    fn a_content_encoding_is_compression() {
        for encoding in ["gzip", "br", "zstd", "deflate"] {
            let response = response(&[(header::CONTENT_ENCODING, encoding)]);
            assert_eq!(
                unsupported(&response),
                Some(Unsupported::Compression {
                    encoding: HeaderValue::from_str(encoding).expect("a valid header value"),
                })
            );
        }
    }

    #[test]
    fn a_set_cookie_is_named_by_its_cookie() {
        let response = response(&[(
            header::SET_COOKIE,
            "__Host-session=abc123; Path=/; Secure; HttpOnly; SameSite=Lax",
        )]);
        assert_eq!(
            unsupported(&response),
            Some(Unsupported::SetCookie {
                name: "__Host-session".to_owned()
            })
        );
    }

    #[test]
    fn a_set_cookie_without_a_pair_still_reports() {
        let response = response(&[(header::SET_COOKIE, "malformed")]);
        assert_eq!(
            unsupported(&response),
            Some(Unsupported::SetCookie {
                name: "malformed".to_owned()
            })
        );
    }

    #[test]
    fn switching_protocols_is_an_upgrade() {
        let response = with_status(StatusCode::SWITCHING_PROTOCOLS, &[]);
        assert_eq!(unsupported(&response), Some(Unsupported::Upgrade));
    }

    #[test]
    fn an_upgrade_header_is_an_upgrade() {
        let response = response(&[(header::UPGRADE, "websocket")]);
        assert_eq!(unsupported(&response), Some(Unsupported::Upgrade));
    }

    #[test]
    fn the_most_structural_problem_is_reported_first() {
        let response = response(&[
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CONTENT_ENCODING, "gzip"),
            (header::SET_COOKIE, "a=b"),
        ]);
        assert!(matches!(
            unsupported(&response),
            Some(Unsupported::Streaming { .. })
        ));
    }

    #[test]
    fn a_redirect_is_not_unsupported_here() {
        let response = with_status(StatusCode::SEE_OTHER, &[(header::LOCATION, "/done")]);
        assert_eq!(unsupported(&response), None);
    }
}
