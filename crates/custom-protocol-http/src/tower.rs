//! The rules as tower layers, so they stack in front of any service.
//!
//! [`Origins::accept`] and [`unsupported`] are the decisions; this module is
//! them wired as middleware, which is how a shell actually wants to apply them.
//! The inner service can be anything: a topcoat router, an axum router, a
//! `ServeDir`. None of it knows it is behind a webview.
//!
//! ```ignore
//! let service = ServiceBuilder::new()
//!     .layer(CanonicalOriginLayer::new(origins.clone()))
//!     .layer(follow_redirects())
//!     .layer(RefuseUnsupportedLayer::new())
//!     .service(your_router);
//! ```
//!
//! # Order
//!
//! Outermost first, and it matters. The origin rewrite runs before anything
//! else, because every layer under it gets to assume one canonical origin.
//!
//! [`RefuseUnsupportedLayer`] goes **under** the redirect follower. Over it,
//! the check only ever sees the response that survives the last hop, and a
//! `Set-Cookie` on a hop that itself redirects is exactly how a login answers a
//! `POST`. Following past it drops the cookie with nothing left to read
//! anywhere. Under it, every hop is checked before anything is followed.
//!
//! # Why are redirects not in here?
//!
//! Because `tower-http` already does it. Its `FollowRedirect` re-enters the
//! inner service, and its [`SameOrigin`] policy refuses a `Location` off our
//! own origin - without it, the shell would fetch from the internet on the
//! application's say-so. [`Limited`] caps the hop count. Both are re-exported,
//! so you need no direct dependency on it.
//!
//! Writing it here would have meant a partial reimplementation of the Fetch
//! standard. The tests in this module pin the behaviour we lean on, so an
//! upgrade that changes it fails here instead of in your application.
//!
//! [`Origins::accept`]: crate::Origins::accept
//! [`unsupported`]: crate::unsupported
//! [`SameOrigin`]: follow_redirect::policy::SameOrigin
//! [`Limited`]: follow_redirect::policy::Limited

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use http::{HeaderValue, Request, Response, StatusCode, header};
use tower_layer::Layer;
use tower_service::Service;

pub use tower_http::follow_redirect::{self, FollowRedirect, FollowRedirectLayer};

use crate::{Denial, Origins, Outcome, Unsupported, unsupported};

/// How many redirects to follow before delivering the last one unfollowed.
///
/// Twenty is what browsers allow.
pub const MAX_REDIRECTS: usize = 20;

/// The future every layer here returns.
///
/// Boxed rather than a hand-written state machine: the alternative needs either
/// `unsafe` to pin an enum, which this crate forbids, or a proc-macro
/// dependency. One allocation per request is the price, and a webview custom
/// protocol serves a handful of requests per interaction rather than a wire's
/// worth.
type BoxFuture<T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send>>;

/// The redirect policy this transport needs.
///
/// Three rules, and each is a refusal the shell would otherwise hand-roll:
/// follow only within our own origin, because a `Location` elsewhere would have
/// the shell fetch from the internet on the application's say-so; stop at
/// [`MAX_REDIRECTS`]; and re-send the body on the redirects that preserve it,
/// which `tower-http` will not do unless told how to clone one.
///
/// Private: half a correct follower, and handing out half invites assembling
/// the rest wrongly. The whole is [`follow_redirects`].
fn same_origin_policy<B, E>()
-> impl follow_redirect::policy::Policy<B, E> + Clone + Send + Sync + 'static
where
    B: Clone,
{
    use follow_redirect::policy::{Limited, PolicyExt, SameOrigin, clone_body_fn};

    SameOrigin::new()
        .and::<_, B, E>(Limited::new(MAX_REDIRECTS))
        .and::<_, B, E>(clone_body_fn(|body: &B| Some(body.clone())))
}

/// The redirect follower this transport needs, policy and all.
///
/// Take this rather than building [`FollowRedirectLayer`] yourself, whose
/// defaults are not all ones a custom protocol wants.
///
/// Extensions are dropped on every hop: the ones on an inbound request are the
/// shell's, and replaying them onto a request the *application* asked for would
/// make one hop's process-side state the next hop's. A shell needing per-hop
/// state attaches it beneath this layer, where nothing is inherited.
pub fn follow_redirects<B, E>()
-> FollowRedirectLayer<impl follow_redirect::policy::Policy<B, E> + Clone + Send + Sync + 'static>
where
    B: Clone,
{
    FollowRedirectLayer::with_policy(same_origin_policy::<B, E>()).preserve_extensions(false)
}

/// Rewrites every request into the canonical origin, and refuses the ones that
/// name somebody else's.
#[derive(Debug, Clone)]
pub struct CanonicalOriginLayer {
    origins: Origins,
}

impl CanonicalOriginLayer {
    /// Rewrites into the origin pair `origins` describes.
    #[must_use]
    pub const fn new(origins: Origins) -> CanonicalOriginLayer {
        CanonicalOriginLayer { origins }
    }
}

impl<S> Layer<S> for CanonicalOriginLayer {
    type Service = CanonicalOrigin<S>;

    fn layer(&self, inner: S) -> CanonicalOrigin<S> {
        CanonicalOrigin {
            inner,
            origins: self.origins.clone(),
        }
    }
}

/// The service [`CanonicalOriginLayer`] produces.
#[derive(Debug, Clone)]
pub struct CanonicalOrigin<S> {
    inner: S,
    origins: Origins,
}

/// `ResBody: From<Vec<u8>>` because a refusal has to be built here, without
/// the inner service: the whole point is that it is never called. Bodies that
/// carry a message all satisfy it - `Full<Bytes>`, `axum::body::Body`,
/// topcoat's.
impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for CanonicalOrigin<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>>,
    S::Future: Send + 'static,
    ResBody: From<Vec<u8>> + Send + 'static,
{
    type Response = Response<ResBody>;
    type Error = S::Error;
    type Future = BoxFuture<Response<ResBody>, S::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request<ReqBody>) -> Self::Future {
        match self.origins.accept(request) {
            Outcome::Serve(request) => {
                let future = self.inner.call(request.into_inner());
                Box::pin(future)
            }
            Outcome::Deny(denial) => Box::pin(async move { Ok(refused(&denial)) }),
        }
    }
}

/// Replaces a response this transport cannot carry with one that says so.
#[derive(Debug, Clone, Copy, Default)]
pub struct RefuseUnsupportedLayer;

impl RefuseUnsupportedLayer {
    /// Refuses every capability [`Unsupported`] names.
    #[must_use]
    pub const fn new() -> RefuseUnsupportedLayer {
        RefuseUnsupportedLayer
    }
}

impl<S> Layer<S> for RefuseUnsupportedLayer {
    type Service = RefuseUnsupported<S>;

    fn layer(&self, inner: S) -> RefuseUnsupported<S> {
        RefuseUnsupported { inner }
    }
}

/// The service [`RefuseUnsupportedLayer`] produces.
#[derive(Debug, Clone)]
pub struct RefuseUnsupported<S> {
    inner: S,
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for RefuseUnsupported<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>>,
    S::Future: Send + 'static,
    ResBody: From<Vec<u8>> + Send + 'static,
{
    type Response = Response<ResBody>;
    type Error = S::Error;
    type Future = BoxFuture<Response<ResBody>, S::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request<ReqBody>) -> Self::Future {
        let future = self.inner.call(request);
        Box::pin(async move {
            let response = future.await?;
            match unsupported(&response) {
                Some(unsupported) => Ok(unsupported_response(&unsupported)),
                None => Ok(response),
            }
        })
    }
}

/// A `403`: the request named an origin this protocol does not serve.
fn refused<B: From<Vec<u8>>>(denial: &Denial) -> Response<B> {
    status_only(StatusCode::FORBIDDEN, &denial.to_string())
}

/// A `502`, because the server answered correctly and this transport is the
/// thing that cannot carry the answer. The body names the capability, since the
/// alternative - delivering half a response - is a bug hunt that starts with a
/// blank window.
fn unsupported_response<B: From<Vec<u8>>>(unsupported: &Unsupported) -> Response<B> {
    status_only(StatusCode::BAD_GATEWAY, &unsupported.to_string())
}

fn status_only<B: From<Vec<u8>>>(status: StatusCode, reason: &str) -> Response<B> {
    let body = format!("{status}\n\n{reason}\n");
    let mut response = Response::new(B::from(body.into_bytes()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use bytes::Bytes;
    use http::Method;
    use http_body_util::{BodyExt, Full};
    use tower::{ServiceBuilder, ServiceExt, service_fn};

    use super::*;
    use crate::Platform;

    type ReqBody = Full<Bytes>;
    type ResBody = Full<Bytes>;

    fn origins() -> Origins {
        Origins::new("topcoat", Platform::Scheme).expect("`topcoat` is a valid scheme")
    }

    /// What the inner service was asked for, in order.
    type Seen = Arc<Mutex<Vec<(Method, String)>>>;

    fn request(method: Method, uri: &str) -> Request<ReqBody> {
        Request::builder()
            .method(method)
            .uri(uri)
            .body(Full::new(Bytes::from_static(b"body")))
            .expect("a valid request")
    }

    async fn body_of(response: Response<ResBody>) -> String {
        let bytes = BodyExt::collect(response.into_body())
            .await
            .expect("a full body never fails")
            .to_bytes();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// An inner service answering with `responses` in turn, recording what it
    /// was asked for. The last response repeats once the list runs out.
    fn recording(
        responses: Vec<Response<ResBody>>,
    ) -> (
        Seen,
        impl Service<
            Request<ReqBody>,
            Response = Response<ResBody>,
            Error = std::convert::Infallible,
            Future: Send,
        > + Clone
        + Send
        + 'static,
    ) {
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let remaining = Arc::new(Mutex::new(responses));
        let recorded = Arc::clone(&seen);
        let service = service_fn(move |request: Request<ReqBody>| {
            let recorded = Arc::clone(&recorded);
            let remaining = Arc::clone(&remaining);
            async move {
                recorded
                    .lock()
                    .expect("the recorder is not poisoned")
                    .push((request.method().clone(), request.uri().to_string()));
                let mut remaining = remaining.lock().expect("the queue is not poisoned");
                let response = if remaining.len() > 1 {
                    remaining.remove(0)
                } else {
                    let last = remaining.first().cloned();
                    last.unwrap_or_else(|| Response::new(Full::default()))
                };
                Ok::<_, std::convert::Infallible>(response)
            }
        });
        (seen, service)
    }

    fn ok() -> Response<ResBody> {
        Response::new(Full::new(Bytes::from_static(b"LANDED")))
    }

    fn redirect(status: StatusCode, location: &'static str) -> Response<ResBody> {
        let mut response = Response::new(Full::default());
        *response.status_mut() = status;
        response
            .headers_mut()
            .insert(header::LOCATION, HeaderValue::from_static(location));
        response
    }

    /// The whole stack, in the order a shell applies it.
    async fn serve(
        responses: Vec<Response<ResBody>>,
        request: Request<ReqBody>,
    ) -> (Seen, Response<ResBody>) {
        let (seen, inner) = recording(responses);
        let service = ServiceBuilder::new()
            .layer(CanonicalOriginLayer::new(origins()))
            .layer(follow_redirects::<ReqBody, std::convert::Infallible>())
            .layer(RefuseUnsupportedLayer::new())
            .service(inner);
        let response = service
            .oneshot(request)
            .await
            .expect("the inner service is infallible");
        (seen, response)
    }

    #[tokio::test]
    async fn the_inner_service_sees_one_canonical_origin() {
        let (seen, _) = serve(
            vec![ok()],
            request(Method::GET, "topcoat://localhost/a?b=c"),
        )
        .await;
        let seen = seen.lock().expect("not poisoned");
        assert_eq!(seen[0].1, "https://topcoat.localhost/a?b=c");
    }

    #[tokio::test]
    async fn a_foreign_authority_is_refused_without_reaching_the_service() {
        let (seen, response) =
            serve(vec![ok()], request(Method::GET, "https://evil.example/")).await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(
            seen.lock().expect("not poisoned").is_empty(),
            "the service ran"
        );
    }

    #[tokio::test]
    async fn a_set_cookie_becomes_a_502_naming_the_cookie() {
        let mut response = ok();
        response.headers_mut().insert(
            header::SET_COOKIE,
            HeaderValue::from_static("__Host-session=t0ken; Secure"),
        );
        let (_, response) =
            serve(vec![response], request(Method::GET, "topcoat://localhost/")).await;

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(body_of(response).await.contains("__Host-session"));
    }

    #[tokio::test]
    async fn a_cookie_on_a_hop_that_redirects_is_refused() {
        let mut hop = redirect(StatusCode::SEE_OTHER, "/landed");
        hop.headers_mut().insert(
            header::SET_COOKIE,
            HeaderValue::from_static("__Host-session=t0ken; Secure"),
        );
        let (seen, response) = serve(
            vec![hop, ok()],
            request(Method::POST, "topcoat://localhost/sign-in"),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(body_of(response).await.contains("__Host-session"));
        assert_eq!(
            seen.lock().expect("not poisoned").len(),
            1,
            "the redirect was followed past the cookie"
        );
    }

    #[tokio::test]
    async fn see_other_becomes_a_get_and_drops_the_body() {
        let (seen, response) = serve(
            vec![redirect(StatusCode::SEE_OTHER, "/landed"), ok()],
            request(Method::POST, "topcoat://localhost/todos"),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let seen = seen.lock().expect("not poisoned");
        assert_eq!(seen[0].0, Method::POST);
        assert_eq!(seen[1].0, Method::GET, "the hop kept the method");
        assert!(seen[1].1.ends_with("/landed"));
    }

    #[tokio::test]
    async fn moved_and_found_rewrite_a_post_and_leave_other_methods_alone() {
        for status in [StatusCode::MOVED_PERMANENTLY, StatusCode::FOUND] {
            let (seen, _) = serve(
                vec![redirect(status, "/landed"), ok()],
                request(Method::POST, "topcoat://localhost/todos"),
            )
            .await;
            assert_eq!(seen.lock().expect("not poisoned")[1].0, Method::GET);

            let (seen, _) = serve(
                vec![redirect(status, "/landed"), ok()],
                request(Method::PUT, "topcoat://localhost/todos"),
            )
            .await;
            assert_eq!(
                seen.lock().expect("not poisoned")[1].0,
                Method::PUT,
                "{status} rewrote a PUT"
            );
        }
    }

    #[tokio::test]
    async fn temporary_and_permanent_preserve_the_method() {
        for status in [
            StatusCode::TEMPORARY_REDIRECT,
            StatusCode::PERMANENT_REDIRECT,
        ] {
            let (seen, _) = serve(
                vec![redirect(status, "/landed"), ok()],
                request(Method::POST, "topcoat://localhost/todos"),
            )
            .await;
            assert_eq!(
                seen.lock().expect("not poisoned")[1].0,
                Method::POST,
                "{status} rewrote the method"
            );
        }
    }

    #[tokio::test]
    async fn a_redirect_off_our_origin_is_handed_over_unfollowed() {
        for location in [
            "https://evil.example/x",
            "//evil.example/x",
            "https://topcoat.localhost.evil.example/x",
        ] {
            let (seen, response) = serve(
                vec![redirect(StatusCode::SEE_OTHER, location), ok()],
                request(Method::POST, "topcoat://localhost/todos"),
            )
            .await;

            assert_eq!(
                response.status(),
                StatusCode::SEE_OTHER,
                "{location} was followed"
            );
            assert_eq!(seen.lock().expect("not poisoned").len(), 1);
        }
    }

    #[tokio::test]
    async fn a_relative_location_resolves_against_the_request() {
        let (seen, _) = serve(
            vec![redirect(StatusCode::SEE_OTHER, "done"), ok()],
            request(Method::POST, "topcoat://localhost/lists/mine"),
        )
        .await;

        assert!(
            seen.lock().expect("not poisoned")[1]
                .1
                .ends_with("/lists/done"),
            "a relative reference did not resolve against the request"
        );
    }

    #[tokio::test]
    async fn a_redirect_loop_stops_at_the_hop_limit() {
        let (seen, response) = serve(
            vec![redirect(StatusCode::SEE_OTHER, "/loop")],
            request(Method::POST, "topcoat://localhost/todos"),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            seen.lock().expect("not poisoned").len(),
            MAX_REDIRECTS + 1,
            "the hop limit is not what this crate names"
        );
    }
}
