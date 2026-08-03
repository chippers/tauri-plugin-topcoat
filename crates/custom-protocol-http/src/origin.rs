//! Origin normalisation: one origin for the server, whatever the platform gave
//! the webview.
//!
//! A custom protocol is reached as `scheme://localhost` on macOS, iOS and
//! Linux, and as `http://scheme.localhost` on Windows and Android, where the
//! webview cannot register a non-standard scheme. A server that saw those
//! differences would have to treat its own address as platform-conditional,
//! and so would every application on top of it.
//!
//! Instead the server always sees `https://scheme.localhost`. `https` because a
//! server that gates anything on its own scheme should behave here as it does
//! in production, and this transport is more isolated than the TLS it stands
//! in for. `.localhost` is reserved by RFC 6761 and can never resolve off the
//! machine, so a canonical URL that escapes into a real fetch fails closed
//! instead of reaching a host somebody registered.

use http::{HeaderMap, HeaderName, HeaderValue, Request, Uri, header, uri};

/// The URL shape a webview gives a custom protocol.
///
/// The only place in this crate that names an operating system. A new platform
/// adds a variant here and nowhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// `<scheme>://localhost/...` - macOS, iOS, Linux.
    Scheme,

    /// `http://<scheme>.localhost/...` - Windows and Android, where the
    /// webview rewrites custom schemes onto http.
    HttpSubdomain,

    /// `https://<scheme>.localhost/...` - as [`HttpSubdomain`], for a webview
    /// configured to use https for that rewrite.
    ///
    /// [`HttpSubdomain`]: Platform::HttpSubdomain
    HttpsSubdomain,
}

impl Platform {
    /// The shape this build's webview uses, before any https override.
    #[must_use]
    pub const fn current() -> Platform {
        #[cfg(any(target_os = "windows", target_os = "android"))]
        {
            Platform::HttpSubdomain
        }
        #[cfg(not(any(target_os = "windows", target_os = "android")))]
        {
            Platform::Scheme
        }
    }
}

/// A `scheme://host` origin, compared without regard to ASCII case.
///
/// `http`'s own scheme and authority, which compare that way already and drop
/// straight into a `Uri`. Not `url::Origin`: per the URL standard a non-special
/// scheme has an *opaque* origin, and `url` gives every opaque origin a fresh
/// counter value, so `topcoat://localhost` does not compare equal to itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    scheme: uri::Scheme,
    authority: uri::Authority,
}

impl Origin {
    const fn new(scheme: uri::Scheme, authority: uri::Authority) -> Origin {
        Origin { scheme, authority }
    }

    /// The scheme half.
    #[must_use]
    pub const fn scheme(&self) -> &uri::Scheme {
        &self.scheme
    }

    /// The authority half, which for every origin here is a bare host.
    #[must_use]
    pub const fn authority(&self) -> &uri::Authority {
        &self.authority
    }

    /// Whether `url` is a URL within this origin.
    ///
    /// Parsed and not prefix-matched: `topcoat://localhost.evil.example` begins
    /// with `topcoat://localhost` and belongs to somebody else. Anything naming
    /// no origin - a relative reference, a `data:` document - is not in one.
    #[must_use]
    pub fn covers(&self, url: &str) -> bool {
        url.parse::<Uri>().is_ok_and(|uri| self.holds(&uri))
    }

    /// The same question, already parsed.
    fn holds(&self, uri: &Uri) -> bool {
        uri.scheme() == Some(&self.scheme) && uri.authority() == Some(&self.authority)
    }

    /// This origin with `path_and_query` on the end.
    fn join(&self, path_and_query: uri::PathAndQuery) -> Result<Uri, http::Error> {
        Uri::builder()
            .scheme(self.scheme.clone())
            .authority(self.authority.clone())
            .path_and_query(path_and_query)
            .build()
    }
}

/// `scheme://host`, as an `Origin` header spells one: no trailing slash.
impl core::fmt::Display for Origin {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}://{}", self.scheme, self.authority)
    }
}

/// A protocol scheme that could not be turned into an origin pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginError {
    /// The scheme name is not usable as both a URL scheme and a DNS label.
    ///
    /// On Windows and Android the scheme becomes the first label of
    /// `<scheme>.localhost`, so it is held to the stricter of the two rules:
    /// a leading ASCII letter followed by letters, digits, and hyphens, within
    /// the 63 octets a DNS label may be.
    InvalidScheme {
        /// The scheme as given.
        scheme: String,
    },
}

impl core::fmt::Display for OriginError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            OriginError::InvalidScheme { scheme } => write!(
                f,
                "`{scheme}` is not a usable protocol scheme: it must start with an ASCII letter, \
                 contain only ASCII letters, digits, and hyphens, and be at most {MAX_LABEL} \
                 characters"
            ),
        }
    }
}

impl core::error::Error for OriginError {}

/// Why a request was refused before the server saw it.
///
/// Exhaustive on purpose: a new way to refuse should make the shell reconsider
/// what it reports, at compile time.
///
/// A refusal becomes a `403` body and a log line, so the fields are an address
/// and nothing else: there is none a path or a query could arrive in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Denial {
    /// The URL was not for the origin this protocol serves.
    ///
    /// Nothing routable should produce this; it means the webview handed us a
    /// request meant for somewhere else.
    ForeignAuthority {
        /// The scheme, where there was one; authority-form names none.
        scheme: Option<uri::Scheme>,
        /// The authority the request named.
        authority: uri::Authority,
    },
    /// The canonical URL could not be rebuilt from its validated parts.
    ///
    /// Unreachable in practice, and refusing beats unwrapping.
    MalformedUri,
}

impl core::fmt::Display for Denial {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Denial::ForeignAuthority {
                scheme: Some(scheme),
                authority,
            } => write!(
                f,
                "`{scheme}://{authority}` is not the origin this protocol serves"
            ),
            Denial::ForeignAuthority {
                scheme: None,
                authority,
            } => write!(f, "`{authority}` is not the origin this protocol serves"),
            Denial::MalformedUri => f.write_str("the canonical URL could not be built"),
        }
    }
}

impl core::error::Error for Denial {}

/// The pair of origins a custom protocol lives between, and the sole entry
/// point for turning a webview request into one the server may serve.
#[derive(Debug, Clone)]
pub struct Origins {
    canonical: Origin,
    platform: Origin,
    /// The `Host` every admitted request leaves with, built once.
    host: HeaderValue,
    /// The canonical origin as an `Origin` header, likewise.
    origin: HeaderValue,
}

impl Origins {
    /// Builds the origin pair for a protocol named `scheme` on `platform`.
    ///
    /// # Errors
    ///
    /// [`OriginError::InvalidScheme`] if the name cannot serve as both a URL
    /// scheme and a DNS label.
    pub fn new(scheme: &str, platform: Platform) -> Result<Origins, OriginError> {
        Origins::build(scheme, platform).ok_or_else(|| OriginError::InvalidScheme {
            scheme: scheme.to_owned(),
        })
    }

    /// The pair, or [`None`] for a name that cannot make one.
    ///
    /// One `Option` and not four error paths: every step below rejects the same
    /// mistake, and asking anyway is what keeps this free of an unwrap.
    fn build(scheme: &str, platform: Platform) -> Option<Origins> {
        if !is_usable_scheme(scheme) {
            return None;
        }
        let scheme = uri::Scheme::try_from(scheme.to_ascii_lowercase().as_str()).ok()?;
        let host = uri::Authority::try_from(format!("{scheme}.{LOCALHOST}").as_str()).ok()?;

        let canonical = Origin::new(uri::Scheme::HTTPS, host.clone());
        let platform = match platform {
            Platform::Scheme => Origin::new(scheme, uri::Authority::from_static(LOCALHOST)),
            Platform::HttpSubdomain => Origin::new(uri::Scheme::HTTP, host.clone()),
            Platform::HttpsSubdomain => canonical.clone(),
        };

        Some(Origins {
            host: HeaderValue::try_from(host.as_str()).ok()?,
            origin: HeaderValue::try_from(canonical.to_string()).ok()?,
            canonical,
            platform,
        })
    }

    /// The origin the server sees, on every platform.
    #[must_use]
    pub const fn canonical(&self) -> &Origin {
        &self.canonical
    }

    /// The origin the webview speaks on this platform.
    #[must_use]
    pub const fn platform(&self) -> &Origin {
        &self.platform
    }

    /// Whether `url` names the origin this protocol serves.
    ///
    /// The shell uses this to refuse, at startup, an application whose public
    /// base URL is our own canonical origin: absolute URLs built from it would
    /// escape the protocol and be fetched for real, which is a broken image
    /// long after the mistake rather than an error at the moment of it.
    #[must_use]
    pub fn collides_with(&self, url: &str) -> bool {
        self.canonical.covers(url) || self.platform.covers(url)
    }

    /// Admits a request and rewrites it into the canonical origin.
    ///
    /// Admission is only the question of address: a request naming somebody
    /// else's origin is refused, and everything else is rewritten. Deciding
    /// whether a request may *act* is the server's job, and this crate attaches
    /// no credential that would undermine the answer.
    ///
    /// `Origin` and `Referer` are moved onto the canonical origin only when
    /// they were ours to begin with. A foreign value passes through untouched,
    /// so a server running its own origin checks still sees a stranger as a
    /// stranger.
    ///
    /// `Host` is set, because no webview reliably sends one and a server
    /// comparing `Origin` against it needs both. `Accept-Encoding` is dropped,
    /// because nothing here is on a wire and WKWebView will not decode what it
    /// is handed anyway. Hop-by-hop headers
    /// go too, including the ones a `Connection` names - except `Host`,
    /// `Origin` and `Referer`, which a client does not get to delete.
    pub fn accept<B>(&self, mut request: Request<B>) -> Outcome<B> {
        if let Err(denial) = self.check_authority(request.uri()) {
            return Outcome::Deny(denial);
        }

        let path_and_query = request
            .uri()
            .path_and_query()
            .cloned()
            .unwrap_or_else(|| uri::PathAndQuery::from_static("/"));
        let Ok(rewritten) = self.canonical.join(path_and_query) else {
            return Outcome::Deny(Denial::MalformedUri);
        };
        crate::trace::rewrote_origin(&self.platform, rewritten.path());
        *request.uri_mut() = rewritten;

        self.rewrite_headers(request.headers_mut());
        Outcome::Serve(CanonicalRequest { request })
    }

    /// Rejects a request addressed to anything but the origin we serve.
    ///
    /// A URI naming no authority is a relative request, which cannot name a
    /// foreign origin and so is ours. There is no third case: [`Uri`] has no
    /// shape carrying a scheme without one.
    fn check_authority(&self, uri: &Uri) -> Result<(), Denial> {
        let Some(authority) = uri.authority() else {
            return Ok(());
        };
        if self.platform.holds(uri) {
            Ok(())
        } else {
            Err(Denial::ForeignAuthority {
                scheme: uri.scheme().cloned(),
                authority: authority.clone(),
            })
        }
    }

    fn rewrite_headers(&self, headers: &mut HeaderMap) {
        headers.insert(header::HOST, self.host.clone());

        // An `Origin` carries no path, so ours is replaced outright where a
        // `Referer` has to be rebuilt around one.
        if self.is_ours(headers.get(header::ORIGIN)) {
            headers.insert(header::ORIGIN, self.origin.clone());
        }
        if let Some(rebased) = self.rebase(headers.get(header::REFERER)) {
            headers.insert(header::REFERER, rebased);
        }

        headers.remove(header::ACCEPT_ENCODING);
        for name in AMBIENT_AUTHORITY {
            headers.remove(name);
        }
        remove_hop_by_hop(headers);
    }

    /// Whether a header value is a URL on the origin the webview speaks.
    fn is_ours(&self, value: Option<&HeaderValue>) -> bool {
        value
            .and_then(|value| value.to_str().ok())
            .is_some_and(|url| self.platform.covers(url))
    }

    /// Moves a URL from the platform origin onto the canonical one, or [`None`]
    /// if it was never ours to move.
    fn rebase(&self, value: Option<&HeaderValue>) -> Option<HeaderValue> {
        let url: Uri = value?.to_str().ok()?.parse().ok()?;
        if !self.platform.holds(&url) {
            return None;
        }
        let rebased = self.canonical.join(url.path_and_query()?.clone()).ok()?;
        HeaderValue::try_from(rebased.to_string()).ok()
    }
}

/// The host reserved by RFC 6761, which every origin here is under.
const LOCALHOST: &str = "localhost";

/// Credentials a client attaches by itself, without the document asking.
///
/// `Authorization` is deliberately not here: a document sets it explicitly and
/// a cross-origin attacker cannot, so it is the one credential CSRF already
/// cannot forge.
const AMBIENT_AUTHORITY: &[HeaderName] = &[header::COOKIE];

/// Headers that describe a connection rather than a message. A custom protocol
/// has no connection to describe.
const HOP_BY_HOP: &[HeaderName] = &[
    header::CONNECTION,
    header::PROXY_AUTHENTICATE,
    header::PROXY_AUTHORIZATION,
    header::TE,
    header::TRAILER,
    header::TRANSFER_ENCODING,
    header::UPGRADE,
];

/// Headers a `Connection` may not name away.
///
/// Dropping one is normally fail-safe; for these three it is the opposite,
/// since a request arriving with no `Origin` is one a server passes.
const DECIDES_ADMISSION: &[HeaderName] = &[header::HOST, header::ORIGIN, header::REFERER];

/// Removes the headers that describe a connection rather than a message.
///
/// `Connection` names further ones - RFC 7230 6.1 - so what it names goes with
/// it, less anything in [`DECIDES_ADMISSION`].
fn remove_hop_by_hop(headers: &mut HeaderMap) {
    let named: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|token| HeaderName::try_from(token.trim()).ok())
        .filter(|name| !DECIDES_ADMISSION.contains(name))
        .collect();

    for name in named.iter().chain(HOP_BY_HOP) {
        headers.remove(name);
    }
}

/// A request that has been admitted and rewritten into the canonical origin.
///
/// Constructible only by [`Origins::accept`], so the rewrite cannot be skipped
/// on the way to the server.
#[derive(Debug)]
pub struct CanonicalRequest<B> {
    request: Request<B>,
}

impl<B> CanonicalRequest<B> {
    /// Borrows the underlying request.
    #[must_use]
    pub const fn get_ref(&self) -> &Request<B> {
        &self.request
    }

    /// Takes the underlying request, to hand to the server.
    #[must_use]
    pub fn into_inner(self) -> Request<B> {
        self.request
    }
}

/// What to do with a request the webview delivered.
#[derive(Debug)]
#[must_use]
pub enum Outcome<B> {
    /// Serve it: hand the inner request to the server.
    Serve(CanonicalRequest<B>),
    /// Refuse it, without troubling the server.
    Deny(Denial),
}

/// The octets a DNS label may be, per RFC 1035.
///
/// Shorter than the 64 `http` allows a scheme, so it is the rule both are held
/// to.
const MAX_LABEL: usize = 63;

/// Whether `scheme` works as both a URL scheme and a DNS label.
fn is_usable_scheme(scheme: &str) -> bool {
    let mut characters = scheme.chars();
    scheme.len() <= MAX_LABEL
        && characters
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic())
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '-')
        && !scheme.ends_with('-')
}

#[cfg(test)]
mod tests {
    use http::Method;

    use super::*;

    fn origins(platform: Platform) -> Origins {
        Origins::new("topcoat", platform).expect("`topcoat` is a valid scheme")
    }

    fn get(uri: &str) -> Request<()> {
        Request::builder()
            .uri(uri)
            .body(())
            .expect("a valid request")
    }

    #[test]
    fn every_platform_presents_the_same_canonical_origin() {
        for platform in [
            Platform::Scheme,
            Platform::HttpSubdomain,
            Platform::HttpsSubdomain,
        ] {
            let canonical = origins(platform);
            let canonical = canonical.canonical();
            assert_eq!(canonical.to_string(), "https://topcoat.localhost");
            assert_eq!(canonical.scheme(), &uri::Scheme::HTTPS);
            assert_eq!(canonical.authority(), "topcoat.localhost");
        }
    }

    #[test]
    fn platform_origins_match_what_each_webview_speaks() {
        assert_eq!(
            origins(Platform::Scheme).platform().to_string(),
            "topcoat://localhost"
        );
        assert_eq!(
            origins(Platform::HttpSubdomain).platform().to_string(),
            "http://topcoat.localhost"
        );
        let https = origins(Platform::HttpsSubdomain);
        assert_eq!(https.platform(), https.canonical());
    }

    #[test]
    fn a_scheme_that_is_not_a_dns_label_is_refused() {
        let too_long = "t".repeat(MAX_LABEL + 1);
        for scheme in [
            "", "1topcoat", "top coat", "top_coat", "top.coat", "topcoat-", &too_long,
        ] {
            assert_eq!(
                Origins::new(scheme, Platform::Scheme).err(),
                Some(OriginError::InvalidScheme {
                    scheme: scheme.to_owned()
                }),
                "{scheme:?} was accepted"
            );
        }
        assert!(Origins::new("top-coat2", Platform::Scheme).is_ok());
        assert!(Origins::new(&"t".repeat(MAX_LABEL), Platform::Scheme).is_ok());
    }

    #[test]
    fn the_path_and_query_survive_the_rewrite() {
        let Outcome::Serve(request) =
            origins(Platform::Scheme).accept(get("topcoat://localhost/a/b?c=d&e=%20f"))
        else {
            panic!("a GET from our own origin should be served");
        };
        assert_eq!(
            request.get_ref().uri().to_string(),
            "https://topcoat.localhost/a/b?c=d&e=%20f"
        );
    }

    #[test]
    fn the_windows_shape_rewrites_to_the_same_canonical_url() {
        let Outcome::Serve(request) =
            origins(Platform::HttpSubdomain).accept(get("http://topcoat.localhost/a?b=c"))
        else {
            panic!("a GET from our own origin should be served");
        };
        assert_eq!(
            request.get_ref().uri().to_string(),
            "https://topcoat.localhost/a?b=c"
        );
    }

    #[test]
    fn a_request_for_a_foreign_authority_is_refused() {
        let outcome = origins(Platform::Scheme).accept(get("https://evil.example/a"));
        let Outcome::Deny(denial) = outcome else {
            panic!("somebody else's origin should be refused: {outcome:?}");
        };
        assert_eq!(
            denial,
            Denial::ForeignAuthority {
                scheme: Some(uri::Scheme::HTTPS),
                authority: uri::Authority::from_static("evil.example"),
            }
        );
    }

    /// A refusal is a `403` body and a log line, and a path is not an address.
    #[test]
    fn a_refusal_names_an_origin_and_nothing_further() {
        let outcome =
            origins(Platform::Scheme).accept(get("https://evil.example/inbox?token=s3cret"));
        let Outcome::Deny(denial) = outcome else {
            panic!("somebody else's origin should be refused: {outcome:?}");
        };
        let said = denial.to_string();
        assert!(said.contains("evil.example"), "{said}");
        assert!(!said.contains("inbox"), "the path was reported: {said}");
        assert!(!said.contains("s3cret"), "the query was reported: {said}");
    }

    #[test]
    fn a_relative_request_is_ours_by_construction() {
        let outcome = origins(Platform::Scheme).accept(get("/a/b"));
        assert!(matches!(outcome, Outcome::Serve(_)), "{outcome:?}");
    }

    #[test]
    fn host_is_supplied_because_no_webview_reliably_sends_one() {
        let Outcome::Serve(request) = origins(Platform::Scheme).accept(get("topcoat://localhost/"))
        else {
            panic!("a GET from our own origin should be served");
        };
        assert_eq!(
            request
                .get_ref()
                .headers()
                .get(header::HOST)
                .map(|h| h.to_str().unwrap_or_default()),
            Some("topcoat.localhost")
        );
    }

    #[test]
    fn our_own_origin_and_referer_are_rebased() {
        let request = Request::builder()
            .uri("topcoat://localhost/submit")
            .method(Method::POST)
            .header(header::ORIGIN, "topcoat://localhost")
            .header(header::REFERER, "topcoat://localhost/form?x=1")
            .body(())
            .expect("a valid request");
        let Outcome::Serve(request) = origins(Platform::Scheme).accept(request) else {
            panic!("a POST from our own origin should be served");
        };
        let headers = request.get_ref().headers();
        assert_eq!(
            headers.get(header::ORIGIN).and_then(|h| h.to_str().ok()),
            Some("https://topcoat.localhost")
        );
        assert_eq!(
            headers.get(header::REFERER).and_then(|h| h.to_str().ok()),
            Some("https://topcoat.localhost/form?x=1")
        );
    }

    /// An `Origin` is an origin and a `Referer` is a URL, so only one of them
    /// comes back with a path.
    #[test]
    fn a_rebased_referer_is_a_url_and_a_rebased_origin_is_not() {
        let request = Request::builder()
            .uri("topcoat://localhost/submit")
            .method(Method::POST)
            .header(header::ORIGIN, "topcoat://localhost")
            .header(header::REFERER, "topcoat://localhost")
            .body(())
            .expect("a valid request");
        let Outcome::Serve(request) = origins(Platform::Scheme).accept(request) else {
            panic!("a POST from our own origin should be served");
        };
        let headers = request.get_ref().headers();
        assert_eq!(
            headers.get(header::ORIGIN),
            Some(&HeaderValue::from_static("https://topcoat.localhost"))
        );
        assert_eq!(
            headers.get(header::REFERER),
            Some(&HeaderValue::from_static("https://topcoat.localhost/")),
            "a URL with an empty path is spelled with the slash"
        );
    }

    #[test]
    fn a_foreign_origin_is_left_alone_rather_than_laundered() {
        let request = Request::builder()
            .uri("topcoat://localhost/read")
            .header(header::ORIGIN, "https://evil.example")
            .body(())
            .expect("a valid request");
        let Outcome::Serve(request) = origins(Platform::Scheme).accept(request) else {
            panic!("a GET is always served");
        };
        assert_eq!(
            request
                .get_ref()
                .headers()
                .get(header::ORIGIN)
                .and_then(|h| h.to_str().ok()),
            Some("https://evil.example")
        );
    }

    #[test]
    fn accept_encoding_and_hop_by_hop_headers_are_dropped() {
        let request = Request::builder()
            .uri("topcoat://localhost/")
            .header(header::ACCEPT_ENCODING, "gzip, br")
            .header(header::CONNECTION, "keep-alive")
            .header(header::TRANSFER_ENCODING, "chunked")
            .body(())
            .expect("a valid request");
        let Outcome::Serve(request) = origins(Platform::Scheme).accept(request) else {
            panic!("a GET is always served");
        };
        let headers = request.get_ref().headers();
        assert!(headers.get(header::ACCEPT_ENCODING).is_none());
        assert!(headers.get(header::CONNECTION).is_none());
        assert!(headers.get(header::TRANSFER_ENCODING).is_none());
    }

    /// RFC 7230 6.1: a `Connection` names further headers as this hop's only.
    #[test]
    fn a_header_connection_names_goes_with_it() {
        let request = Request::builder()
            .uri("topcoat://localhost/")
            .header(header::CONNECTION, "keep-alive, X-Internal-Trace")
            .header("x-internal-trace", "abc123")
            .header("x-end-to-end", "kept")
            .body(())
            .expect("a valid request");
        let Outcome::Serve(request) = origins(Platform::Scheme).accept(request) else {
            panic!("a GET is always served");
        };
        let headers = request.get_ref().headers();
        assert!(headers.get("x-internal-trace").is_none());
        assert!(headers.get(header::CONNECTION).is_none());
        assert!(headers.get("x-end-to-end").is_some(), "took an unnamed one");
    }

    /// The one a client would reach for.
    ///
    /// Stripping `Origin` is fail-open - a request carrying none is one a
    /// server passes - so it is not on offer.
    #[test]
    fn a_connection_cannot_name_away_the_evidence_against_it() {
        let request = Request::builder()
            .uri("topcoat://localhost/submit")
            .method(Method::POST)
            .header(header::CONNECTION, "origin, referer, host")
            .header(header::ORIGIN, "https://evil.example")
            .body(())
            .expect("a valid request");
        let Outcome::Serve(request) = origins(Platform::Scheme).accept(request) else {
            panic!("a POST is served; whether it may act is the server's call");
        };
        let headers = request.get_ref().headers();
        assert_eq!(
            headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()),
            Some("https://evil.example"),
            "a stranger deleted its own `Origin` and arrived looking local"
        );
        assert!(headers.get(header::HOST).is_some(), "nothing to compare to");
    }

    #[test]
    fn an_inbound_cookie_never_reaches_the_server() {
        let request = Request::builder()
            .uri("topcoat://localhost/")
            .header(header::COOKIE, "__Host-session=t0ken; other=1")
            .body(())
            .expect("a valid request");
        let Outcome::Serve(request) = origins(Platform::Scheme).accept(request) else {
            panic!("a GET is always served");
        };
        assert!(
            request.get_ref().headers().get(header::COOKIE).is_none(),
            "a credential arrived with neither `Origin` nor `Sec-Fetch-Site` to vouch for it"
        );
    }

    #[test]
    fn an_authorization_header_is_left_for_the_application() {
        let request = Request::builder()
            .uri("topcoat://localhost/")
            .header(header::AUTHORIZATION, "Bearer t0ken")
            .body(())
            .expect("a valid request");
        let Outcome::Serve(request) = origins(Platform::Scheme).accept(request) else {
            panic!("a GET is always served");
        };
        assert_eq!(
            request
                .get_ref()
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer t0ken"),
            "an explicit credential is not ambient and is not ours to drop"
        );
    }

    #[test]
    fn covers_compares_the_whole_origin_and_only_the_origin() {
        let origins = origins(Platform::Scheme);
        let origin = origins.platform();
        assert!(origin.covers("topcoat://localhost"));
        assert!(origin.covers("topcoat://localhost/"));
        assert!(origin.covers("topcoat://localhost/a?b#c"));
        assert!(origin.covers("TOPCOAT://LOCALHOST/a"));
        assert!(!origin.covers("topcoat://localhost.evil.example/"));
        assert!(!origin.covers("topcoat://localhostx"));
        assert!(!origin.covers("topcoat://localhost:8080/"));
        assert!(!origin.covers("topcoat://user@localhost/"));
        assert!(!origin.covers("https://localhost/"));
        assert!(!origin.covers("topcoat://"));
        // Names no origin at all, so it is not within this one.
        assert!(!origin.covers("/a/b"));
        assert!(!origin.covers("localhost"));
        assert!(!origin.covers("data:text/html,<p>hi"));
        assert!(!origin.covers("null"));
    }

    #[test]
    fn collides_with_catches_an_application_base_url_pointed_at_us() {
        let origins = origins(Platform::Scheme);
        assert!(origins.collides_with("https://topcoat.localhost"));
        assert!(origins.collides_with("https://topcoat.localhost/app"));
        assert!(origins.collides_with("topcoat://localhost"));
        assert!(!origins.collides_with("https://example.com"));
        assert!(!origins.collides_with("https://topcoat.localhost.evil.example"));
    }
}
