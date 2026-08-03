//! The HTTP surface the page exercises.
//!
//! No route decides what a result means; each one records what it saw and the
//! meaning of that - including the meaning of never being asked at all -
//! lives in the probe table.

use std::{borrow::Cow, collections::BTreeMap, io::Write as _};

use flate2::{Compression, write::GzEncoder};
use http::{HeaderValue, Method, Request, Response, StatusCode, Uri, header};
use serde::{Deserialize as _, Serialize, de::value::StrDeserializer};
use tauri::{AppHandle, AssetResolver, Manager as _, Runtime};
use url::{Url, form_urlencoded};

use crate::Run;
use crate::clock::CLOCK;
use crate::report::{Answer, Probe};

/// One buffered body, which is all a custom protocol can deliver.
type Reply = Response<Cow<'static, [u8]>>;

/// The document the window opens on.
const INDEX: &str = "/index.html";

/// Where the two IPC frames live. Everything under it names the probe it is
/// answering as its next path segment.
const IPC: &str = "/ipc/";

/// Cookies covering the shapes topcoat's session store emits.
const COOKIES: &[&str] = &[
    "probe_plain=1; Path=/",
    "probe_secure=1; Path=/; Secure",
    "__Host-probe=1; Path=/; Secure",
    "probe_lax=1; Path=/; SameSite=Lax",
];

/// The policy `/csp` is served with. Strict enough that the document's own
/// inline script is forbidden, which is what makes it measurable.
const POLICY: &str = "default-src 'self'; script-src 'none'";

/// The policy `/ipc` is served with: this origin and nothing else, which is
/// what an application that sets a CSP at all would send. Tauri is not given a
/// chance to amend it - `tauri.conf.json` sets `csp: null` - so this is the
/// policy a framework's own response would carry.
const IPC_POLICY: &str = "default-src 'self'";

/// Answers one request.
///
/// `app` is here for two things it alone can reach. Its asset resolver is the
/// frontend Tauri already embedded in this binary, and serving that from here
/// rather than letting Tauri serve it is the whole arrangement in miniature:
/// the build pipeline is the ordinary one, and the protocol handler is what
/// hands the bytes over. Its windows are the other - the policed one closes
/// itself out of [`ipc`] once it has reported.
pub fn route<R: Runtime>(request: &Request<Vec<u8>>, run: &Run, app: &AppHandle<R>) -> Reply {
    let assets = app.asset_resolver();
    let report = &run.report;
    let method = request.method();
    let path = request.uri().path();
    let query = query(request.uri());

    if let Some(rest) = path.strip_prefix(IPC) {
        return ipc(request, run, app, &assets, method, rest);
    }

    match (method, path) {
        // The page, its stylesheet, its scripts and the two documents loaded
        // as templates all come out of the same place.
        //
        // Deliberately unpoliced. The page frames a `data:` URL to get an
        // opaque origin, so anything short of `frame-src data:` blocks it and
        // the four foreign probes would report what our own policy did rather
        // than what the webview does. The documents that need one carry their
        // own, below.
        (&Method::GET, "/") => asset(&assets, INDEX),

        // A document that needs a policy attached, which an embedded asset
        // cannot carry on its own.
        (&Method::GET, "/csp") => policed(&assets, "/csp.html", Some(POLICY)),

        // The page picks the plaintext, so what it compares against is its own
        // and nothing here has to agree with it about a magic string.
        (&Method::GET, "/gzip") => gzip(param(&query, "text")),

        (&Method::POST, "/echo") => {
            report.record(Probe::FetchPostHeaders, Answer::info(header_names(request)));
            json(&serde_json::json!({ "body": String::from_utf8_lossy(request.body()) }))
        }

        (&Method::GET, "/cookies/set") => {
            let mut response = text("the cookies are set");
            for cookie in COOKIES {
                response
                    .headers_mut()
                    .append(header::SET_COOKIE, HeaderValue::from_static(cookie));
            }
            response
        }
        (&Method::GET, "/cookies/echo") => {
            report.record(
                Probe::SetCookieReturned,
                match value(request, &header::COOKIE) {
                    Some(cookie) => Answer::yes(cookie),
                    None => Answer::no("no Cookie header came back"),
                },
            );
            text("ok")
        }

        // `via` separates the fetch probe from the navigation probe, which land
        // on the same route. `echo` comes back as the body, so the page can
        // recognise its own request without trusting `Response.url`.
        (&Method::GET, "/redirect/303") => see_other(
            "/redirect/landed",
            &[
                ("via", param(&query, "via")),
                ("echo", param(&query, "echo")),
            ],
        ),
        (&Method::GET, "/redirect/landed") => {
            if param(&query, "via") == "nav" {
                report.record(Probe::NavigationFollows303, Answer::yes(""));
            }
            text(param(&query, "echo").to_owned())
        }

        // A form post and a fetch post are handled by different webview code
        // and can disagree, so the headers are recorded for each separately.
        (&Method::POST, "/form") => {
            let body = String::from_utf8_lossy(request.body()).into_owned();
            report.record(
                Probe::FormPostBody,
                if body.is_empty() {
                    Answer::no("the form post arrived with an empty body")
                } else {
                    Answer::yes(body)
                },
            );
            report.record(Probe::FormPostHeaders, Answer::info(header_names(request)));
            text("the form post arrived")
        }

        (&Method::GET, "/foreign/loaded") => {
            report.record(Probe::ForeignDocumentRan, Answer::yes(""));
            text("ok")
        }
        (&Method::POST, "/foreign") => {
            report.record(Probe::ForeignPostDelivered, Answer::yes(""));
            report.record(
                Probe::ForeignPostOrigin,
                match value(request, &header::ORIGIN) {
                    Some(origin) => Answer::yes(origin),
                    None => Answer::no("the request arrived with no Origin"),
                },
            );
            report.record(
                Probe::ForeignPostReferer,
                match value(request, &header::REFERER) {
                    Some(referer) => Answer::info(scheme(referer)),
                    None => Answer::no("no Referer either"),
                },
            );
            text("ok")
        }

        (&Method::GET, "/range") => {
            report.record(
                Probe::RangeHeaderArrives,
                match value(request, &header::RANGE) {
                    Some(range) => Answer::yes(range),
                    None => Answer::no("no Range header arrived"),
                },
            );
            text("ok")
        }

        (&Method::GET, "/csp/rendered") => {
            report.record(Probe::CspDocumentRendered, Answer::yes(""));
            text("ok")
        }
        (&Method::GET, "/csp/script") => {
            report.record(
                Probe::CspEnforced,
                Answer::no("the blocked inline script ran"),
            );
            text("ok")
        }

        (&Method::GET, CLOCK) => json(&run.clock.tick(request.uri(), &query)),

        (&Method::GET, "/report") => json(&report.sheet()),
        (&Method::POST, "/answers") => answers(request, run),
        (&Method::POST, "/done") => {
            // A full channel means the run is already ending.
            let _ = run.finish.try_send(());
            text("ok")
        }

        // The stylesheet, the scripts, and the two documents the page fetches
        // as templates. Anything the build emitted is reachable by its own path.
        (&Method::GET, path) => asset(&assets, path),

        _ => not_found(),
    }
}

/// Serves the IPC document, and takes what it found out.
///
/// One document, framed twice: `/ipc/ipc-invoke-in-frame/` and
/// `/ipc/ipc-invoke-under-csp/`, identical but for the policy the second one
/// carries. Without that pair the measurement would be worthless - a failure
/// under the policy proves nothing if IPC never reaches a subframe in the first
/// place - and running both means the page can tell those two apart.
///
/// The probe being answered rides in the path rather than in the document, so
/// the two frames are one file and its beacons can be plain relative URLs. The
/// page builds those paths out of the generated ids, and an id nothing declares
/// does not deserialize.
fn ipc<R: Runtime>(
    request: &Request<Vec<u8>>,
    run: &Run,
    app: &AppHandle<R>,
    assets: &AssetResolver<R>,
    method: &Method,
    rest: &str,
) -> Reply {
    let (id, action) = rest.split_once('/').unwrap_or((rest, ""));
    let Ok(probe) = Probe::deserialize(StrDeserializer::<serde::de::value::Error>::new(id)) else {
        return not_found();
    };

    match (method, action) {
        (&Method::GET, "") => {
            let policy = matches!(probe, Probe::IpcInvokeUnderCsp).then_some(IPC_POLICY);
            policed(assets, "/ipc.html", policy)
        }

        // Provisional, and overwritten the moment the document reports for
        // real. It separates a policy that stopped the script from one that
        // stopped the document loading at all.
        (&Method::GET, "loaded") => {
            run.report.record(
                probe,
                Answer::no("the document loaded but never reported an answer"),
            );
            text("ok")
        }

        (&Method::POST, "result") => match serde_json::from_slice::<Answer>(request.body()) {
            Ok(answer) => {
                run.report.record(probe, answer);
                // The policed window has said the one thing it was opened to
                // say, so it stops sitting on top of the table. Closed from
                // here and not from the document, because what that window is
                // measuring is whether a policy left it any IPC to close
                // itself with - the answer arrived over the protocol, so this
                // works on the runs where the answer is `no`.
                if matches!(probe, Probe::IpcInvokeUnderCsp)
                    && let Some(window) = app.get_webview_window(crate::ipc::POLICED)
                {
                    let _ = window.close();
                }
                text("ok")
            }
            Err(error) => unreadable("an IPC document", &error),
        },

        _ => not_found(),
    }
}

/// Takes the answers the page reached for itself.
///
/// The keys deserialize straight into [`Probe`], so an id nothing declares is a
/// serde error naming it rather than a row that quietly stays `unknown`. The
/// page cannot spell one anyway: its map is keyed by the exported union.
fn answers(request: &Request<Vec<u8>>, run: &Run) -> Reply {
    let sent = match serde_json::from_slice::<BTreeMap<Probe, Answer>>(request.body()) {
        Ok(sent) => sent,
        Err(error) => return unreadable("the page", &error),
    };

    for (probe, answer) in sent {
        run.report.record(probe, answer);
    }
    text("ok")
}

/// Serves one file out of the frontend Tauri embedded, with the content type
/// the build worked out for it.
///
/// A webview that mangles a subresource over a custom scheme is worth finding
/// out about, which is why nothing here is inlined into the document: the page
/// arrives as a document, a stylesheet and a script, the way any built frontend
/// would.
fn asset<R: Runtime>(assets: &AssetResolver<R>, path: &str) -> Reply {
    let Some(asset) = assets.get(path.to_owned()) else {
        return not_found();
    };

    let mut response = Response::new(Cow::Owned(asset.bytes));
    if let Ok(content_type) = HeaderValue::from_str(&asset.mime_type) {
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, content_type);
    }
    response
}

/// The same, optionally with a `Content-Security-Policy` on top.
fn policed<R: Runtime>(
    assets: &AssetResolver<R>,
    path: &str,
    policy: Option<&'static str>,
) -> Reply {
    let mut response = asset(assets, path);
    if let Some(policy) = policy {
        response.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(policy),
        );
    }
    response
}

fn gzip(text: &str) -> Reply {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(text.as_bytes())
        .expect("writing to a Vec cannot fail");
    let compressed = encoder.finish().expect("writing to a Vec cannot fail");

    let mut response = served(Cow::Owned(compressed), "text/plain; charset=utf-8");
    response
        .headers_mut()
        .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    response
}

/// A `303` to `path`, with `query` encoded onto it.
///
/// `path` is `'static` and everything else leaves the serializer as ASCII, so
/// nothing a webview sends can reach the `expect`.
fn see_other(path: &'static str, query: &[(&str, &str)]) -> Reply {
    let query: String = form_urlencoded::Serializer::new(String::new())
        .extend_pairs(query)
        .finish();
    let location = HeaderValue::try_from(format!("{path}?{query}"))
        .expect("a static path and percent-encoded ASCII make a header value");

    let mut response = Response::new(Cow::Borrowed(&b""[..]));
    *response.status_mut() = StatusCode::SEE_OTHER;
    response.headers_mut().insert(header::LOCATION, location);
    response
}

fn not_found() -> Reply {
    refuse(StatusCode::NOT_FOUND, "no such route")
}

/// A body this could not read, said out loud rather than left `unknown`.
fn unreadable(who: &str, error: &serde_json::Error) -> Reply {
    eprintln!("[warning] {who} sent something this cannot read: {error}");
    refuse(StatusCode::BAD_REQUEST, "unreadable")
}

fn refuse(status: StatusCode, body: &'static str) -> Reply {
    let mut response = text(body);
    *response.status_mut() = status;
    response
}

/// The query as decoded pairs.
///
/// The other half of the `URLSearchParams` the page assembles them with.
fn query(uri: &Uri) -> Query<'_> {
    form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes()).collect()
}

/// What [`query`] returns; the clock takes one too.
pub type Query<'q> = BTreeMap<Cow<'q, str>, Cow<'q, str>>;

/// One query value.
///
/// Absent and empty are the same answer to every reader here.
pub fn param<'q>(query: &'q Query<'_>, name: &str) -> &'q str {
    query.get(name).map_or("", Cow::as_ref)
}

fn value<'r>(request: &'r Request<Vec<u8>>, name: &header::HeaderName) -> Option<&'r str> {
    request
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
}

/// The scheme a URL names.
///
/// A `data:` referer is an entire document, and the only part of it that says
/// anything is the scheme, which is the part that is not ours. `url` and not
/// `http::Uri`, which rejects a URL with no authority - the shape this is for.
fn scheme(url: &str) -> String {
    match Url::parse(url) {
        Ok(url) => url.scheme().to_owned(),
        Err(error) => format!("not a URL: {error}"),
    }
}

/// The names of every header the request carried.
///
/// Names, not values: which headers a webview attaches is the question, and the
/// handful of values that matter have probes of their own.
fn header_names(request: &Request<Vec<u8>>) -> String {
    let mut names: Vec<&str> = request
        .headers()
        .keys()
        .map(header::HeaderName::as_str)
        .collect();
    names.sort_unstable();
    names.join(", ")
}

fn served(body: Cow<'static, [u8]>, content_type: &'static str) -> Reply {
    let mut response = Response::new(body);
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

fn text(body: impl Into<Cow<'static, str>>) -> Reply {
    let body = match body.into() {
        Cow::Borrowed(body) => Cow::Borrowed(body.as_bytes()),
        Cow::Owned(body) => Cow::Owned(body.into_bytes()),
    };
    served(body, "text/plain; charset=utf-8")
}

fn json<T: Serialize>(value: &T) -> Reply {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    served(Cow::Owned(body), "application/json")
}
