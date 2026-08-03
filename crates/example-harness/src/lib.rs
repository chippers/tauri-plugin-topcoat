//! Requests in the shape a webview delivers them, for the examples' tests.
//!
//! The shape is a measured fact rather than a convention: a form post arrives
//! with an `Origin` and a `Referer`, and with no `Host` and no `Cookie`. A
//! `fetch` POST is the one that carries no `Origin`, which is the asymmetry
//! worth keeping straight - a test that sends the `fetch` shape and calls it a
//! form post never reaches the origin check the rewrite exists to satisfy.
//!
//! `probe` is where both were measured, and this is the one place the examples
//! write it down, so a platform that measures differently is one edit rather
//! than a hunt.

use std::borrow::Cow;

use http::{Request, Response, StatusCode, Uri, header};
use tauri_plugin_topcoat::{DEFAULT_SCHEME, Session};

/// The whole response to a `GET`, for a test that reads headers.
pub async fn response(window: &Session, path: &str) -> Response<Cow<'static, [u8]>> {
    let request = Request::get(url(path))
        .body(Vec::new())
        .expect("a valid request");

    window.serve(request).await
}

/// The status and body of a `GET`.
pub async fn get(window: &Session, path: &str) -> (StatusCode, String) {
    read(response(window, path).await)
}

/// The status and body of a form post.
///
/// `body` is urlencoded, as a `<form method="post">` sends it.
pub async fn submit(window: &Session, path: &str, body: &str) -> (StatusCode, String) {
    let request = Request::post(url(path))
        .header(
            header::CONTENT_TYPE,
            mime::APPLICATION_WWW_FORM_URLENCODED.as_ref(),
        )
        .header(header::ORIGIN, origin())
        .header(header::REFERER, url("/").to_string())
        .body(body.as_bytes().to_vec())
        .expect("a valid request");

    read(window.serve(request).await)
}

/// The host every window here is on, which the plugin serves under.
const HOST: &str = "localhost";

/// Where a window on the default scheme asks from.
fn url(path: &str) -> Uri {
    Uri::builder()
        .scheme(DEFAULT_SCHEME)
        .authority(HOST)
        .path_and_query(path)
        .build()
        .unwrap_or_else(|error| panic!("`{path}` is not a path a window could ask for: {error}"))
}

/// The origin such a window names itself by, as an `Origin` header spells one.
fn origin() -> String {
    format!("{DEFAULT_SCHEME}://{HOST}")
}

fn read(response: Response<Cow<'static, [u8]>>) -> (StatusCode, String) {
    let (parts, body) = response.into_parts();
    (parts.status, String::from_utf8_lossy(&body).into_owned())
}
