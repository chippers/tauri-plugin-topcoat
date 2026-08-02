//! The protocol handler: one webview request, one router response.
//!
//! Every rule applied here was decided in `custom-protocol-http`. What is left
//! is the ordering and the awaits.

use std::{
    borrow::Cow,
    convert::Infallible,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use bytes::Bytes;
use custom_protocol_http::{
    Origins,
    tower::{CanonicalOriginLayer, RefuseUnsupportedLayer, follow_redirects},
};
use http::{HeaderValue, Request, Response, StatusCode, header};
use http_body_util::{BodyExt, Full};
use topcoat::router::{Body, Router, to_bytes};
use tower::{Service, ServiceBuilder, ServiceExt};

/// The request body the stack carries.
///
/// `Full<Bytes>` rather than topcoat's own body because a redirect that
/// preserves its body has to send it twice, and only a cloneable body can be.
type ReqBody = Full<Bytes>;

/// Everything one protocol scheme needs to serve a router.
pub(crate) struct Bridge {
    origins: Origins,
    router: Arc<Router>,
}

impl Bridge {
    pub(crate) fn new(origins: Origins, router: Router) -> Bridge {
        Bridge {
            origins,
            router: Arc::new(router),
        }
    }

    pub(crate) const fn origins(&self) -> &Origins {
        &self.origins
    }

    /// Serves one request from the webview.
    ///
    /// The stack is assembled per request rather than held, because the webview
    /// it is serving is part of it. Assembly is a few `Arc` clones.
    pub(crate) async fn serve(&self, request: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
        let (parts, body) = request.into_parts();
        let request = Request::from_parts(parts, Full::new(Bytes::from(body)));

        // The unsupported check goes under the follower, so every hop is
        // checked rather than only the one that survives.
        let service = ServiceBuilder::new()
            .layer(CanonicalOriginLayer::new(self.origins.clone()))
            .layer(follow_redirects::<ReqBody, Infallible>())
            .layer(RefuseUnsupportedLayer::new())
            .service(TopcoatService {
                router: Arc::clone(&self.router),
            });

        match service.oneshot(request).await {
            Ok(response) => deliver(response).await,
            Err(infallible) => match infallible {},
        }
    }
}

/// topcoat as a tower service.
///
/// Not to be confused with [`topcoat::router::RouterService`], which is a
/// `hyper` service over `Incoming` bodies for the `serve` feature this plugin
/// exists to avoid.
///
/// The adapter is the whole of what ties this plugin to topcoat: everything
/// above it in the stack would work the same over an axum router or a
/// `ServeDir`.
#[derive(Clone)]
struct TopcoatService {
    router: Arc<Router>,
}

impl Service<Request<ReqBody>> for TopcoatService {
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Response<Body>, Infallible>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<ReqBody>) -> Self::Future {
        let router = Arc::clone(&self.router);

        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let bytes = body
                .collect()
                .await
                .map(http_body_util::Collected::to_bytes)
                .unwrap_or_default();
            let request = Request::from_parts(parts, Body::from(bytes.to_vec()));
            Ok(router.handle(request).await)
        })
    }
}

/// Buffers the response for the webview, which has no way to stream one.
async fn deliver(response: Response<Body>) -> Response<Cow<'static, [u8]>> {
    let (parts, body) = response.into_parts();
    let Ok(bytes) = to_bytes(body, usize::MAX).await else {
        return status_only(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the response body could not be read",
        );
    };
    Response::from_parts(parts, Cow::Owned(bytes.to_vec()))
}

/// A request arrived before the plugin finished starting, which should not be
/// reachable: a webview has to exist to make one, and `setup` runs first.
pub(crate) fn unavailable() -> Response<Cow<'static, [u8]>> {
    status_only(
        StatusCode::SERVICE_UNAVAILABLE,
        "the topcoat plugin has not finished starting",
    )
}

fn status_only(status: StatusCode, reason: &str) -> Response<Cow<'static, [u8]>> {
    let body = format!("{status}\n\n{reason}\n");
    let mut response = Response::new(Cow::Owned(body.into_bytes()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

/// End-to-end tests: a real topcoat [`Router`] driven through [`Bridge::serve`].
///
/// The handlers are plain `fn` pointers because `Route` cannot capture state.
#[cfg(test)]
mod tests {
    use custom_protocol_http::Platform;
    use http::{HeaderMap, HeaderName, Method};
    use topcoat::{
        context::Cx,
        router::{
            IntoResponse, Path, Response as RouterResponse, RouteFn, RouteFuture,
            headers as request_headers,
        },
    };

    use super::*;

    fn hello(cx: &Cx, _body: Body) -> RouteFuture<'_> {
        Box::pin(async move { "hello".into_response(cx) })
    }

    /// A mutation that answers with Post/Redirect/Get, the shape no webview
    /// follows on its own. It redirects to the route that echoes the cookie, so
    /// a test can see what the followed hop carried.
    fn add_todo(_cx: &Cx, _body: Body) -> RouteFuture<'_> {
        Box::pin(async move {
            let mut response = RouterResponse::new(Body::empty());
            *response.status_mut() = StatusCode::SEE_OTHER;
            response
                .headers_mut()
                .insert(header::LOCATION, HeaderValue::from_static("/whoami"));
            Ok(response)
        })
    }

    /// Answers with a status nothing else produces, so "this route ran" is
    /// distinguishable from "this request was refused" without shared state
    /// that parallel tests would race on.
    fn teapot(_cx: &Cx, _body: Body) -> RouteFuture<'_> {
        Box::pin(async move {
            let mut response = RouterResponse::new(Body::empty());
            *response.status_mut() = StatusCode::IM_A_TEAPOT;
            Ok(response)
        })
    }

    /// Answers with the session cookie topcoat's default token store emits,
    /// which no WebKit cookie store keeps.
    fn login(_cx: &Cx, _body: Body) -> RouteFuture<'_> {
        Box::pin(async move {
            let mut response = RouterResponse::new(Body::from("logged in"));
            response.headers_mut().insert(
                header::SET_COOKIE,
                HeaderValue::from_static("__Host-session=t0ken; Path=/; Secure; HttpOnly"),
            );
            Ok(response)
        })
    }

    /// Post/Redirect/Get onto a hop that sets a cookie, so the response the
    /// webview would have seen is fine and the one it would not is not.
    fn add_todo_then_login(_cx: &Cx, _body: Body) -> RouteFuture<'_> {
        Box::pin(async move {
            let mut response = RouterResponse::new(Body::empty());
            *response.status_mut() = StatusCode::SEE_OTHER;
            response
                .headers_mut()
                .insert(header::LOCATION, HeaderValue::from_static("/login"));
            Ok(response)
        })
    }

    /// The shape of a topcoat login on the default cookie store: mint the
    /// session, set the cookie, and answer the `POST` with a redirect. The
    /// cookie is on the hop the webview never sees.
    fn sign_in_then_home(_cx: &Cx, _body: Body) -> RouteFuture<'_> {
        Box::pin(async move {
            let mut response = RouterResponse::new(Body::empty());
            *response.status_mut() = StatusCode::SEE_OTHER;
            let headers = response.headers_mut();
            headers.insert(header::LOCATION, HeaderValue::from_static("/"));
            headers.insert(
                header::SET_COOKIE,
                HeaderValue::from_static("__Host-session=t0ken; Path=/; Secure; HttpOnly"),
            );
            Ok(response)
        })
    }

    /// The stream topcoat's `sse` feature would produce.
    fn events(_cx: &Cx, _body: Body) -> RouteFuture<'_> {
        Box::pin(async move {
            let mut response = RouterResponse::new(Body::from("data: hello\n\n"));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            Ok(response)
        })
    }

    /// Echoes one request header back, so a test can see what the router saw.
    fn echo_cookie(cx: &Cx, _body: Body) -> RouteFuture<'_> {
        Box::pin(async move {
            let value = request_headers(cx)
                .get(header::COOKIE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("<none>")
                .to_owned();
            value.into_response(cx)
        })
    }

    fn echo_host(cx: &Cx, _body: Body) -> RouteFuture<'_> {
        Box::pin(async move {
            let headers = request_headers(cx);
            let host = headers
                .get(header::HOST)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("<none>");
            let encoding = headers
                .get(header::ACCEPT_ENCODING)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("<none>");
            format!("{host} {encoding}").into_response(cx)
        })
    }

    /// Sends the caller somewhere this plugin must not follow.
    fn offsite(_cx: &Cx, _body: Body) -> RouteFuture<'_> {
        Box::pin(async move {
            let mut response = RouterResponse::new(Body::empty());
            *response.status_mut() = StatusCode::SEE_OTHER;
            response.headers_mut().insert(
                header::LOCATION,
                HeaderValue::from_static("https://evil.example/"),
            );
            Ok(response)
        })
    }

    fn bridge() -> Bridge {
        let router = Router::builder()
            .route(RouteFn::new(Method::GET, path("/"), hello))
            .route(RouteFn::new(Method::POST, path("/todos"), add_todo))
            .route(RouteFn::new(Method::GET, path("/login"), login))
            .route(RouteFn::new(
                Method::POST,
                path("/todos-then-login"),
                add_todo_then_login,
            ))
            .route(RouteFn::new(
                Method::POST,
                path("/sign-in-then-home"),
                sign_in_then_home,
            ))
            .route(RouteFn::new(Method::GET, path("/events"), events))
            .route(RouteFn::new(Method::GET, path("/whoami"), echo_cookie))
            .route(RouteFn::new(Method::GET, path("/headers"), echo_host))
            .route(RouteFn::new(Method::POST, path("/offsite"), offsite))
            .route(RouteFn::new(Method::POST, path("/must-not-run"), teapot))
            .build();
        let origins =
            Origins::new("topcoat", Platform::Scheme).expect("`topcoat` is a valid scheme");
        Bridge::new(origins, router)
    }

    fn path(literal: &'static str) -> Cow<'static, Path> {
        Cow::Borrowed(Path::new(literal))
    }

    fn request(method: Method, path: &str, headers: &[(HeaderName, &str)]) -> Request<Vec<u8>> {
        let mut builder = Request::builder()
            .method(method)
            .uri(format!("topcoat://localhost{path}"));
        for (name, value) in headers {
            builder = builder.header(name, *value);
        }
        builder.body(Vec::new()).expect("a valid request")
    }

    /// The shape WKWebView gives a `fetch` POST: a `Referer` and nothing else.
    const WEBKIT_POST: &[(HeaderName, &str)] = &[(header::REFERER, "topcoat://localhost/")];

    async fn send(bridge: &Bridge, request: Request<Vec<u8>>) -> (StatusCode, String, HeaderMap) {
        let response = bridge.serve(request).await;
        let (parts, body) = response.into_parts();
        (
            parts.status,
            String::from_utf8_lossy(&body).into_owned(),
            parts.headers,
        )
    }

    #[tokio::test]
    async fn a_page_is_served() {
        let (status, body, _) = send(&bridge(), request(Method::GET, "/", &[])).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "hello");
    }

    #[tokio::test]
    async fn the_router_sees_one_origin_and_no_accept_encoding() {
        let (_, body, _) = send(
            &bridge(),
            request(
                Method::GET,
                "/headers",
                &[(header::ACCEPT_ENCODING, "gzip, br")],
            ),
        )
        .await;
        assert_eq!(body, "topcoat.localhost <none>");
    }

    #[tokio::test]
    async fn a_mutation_with_nothing_attributing_it_reaches_the_route() {
        let (status, body, _) = send(&bridge(), request(Method::POST, "/must-not-run", &[])).await;
        assert_eq!(status, StatusCode::IM_A_TEAPOT, "{body}");
    }

    #[tokio::test]
    async fn a_request_for_a_foreign_authority_is_refused() {
        let outbound = Request::builder()
            .method(Method::GET)
            .uri("https://evil.example/")
            .body(Vec::new())
            .expect("a valid request");
        let (status, body, _) = send(&bridge(), outbound).await;

        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.contains("evil.example"), "{body}");
    }

    #[tokio::test]
    async fn no_cookie_header_is_attached_to_anything() {
        let (_, body, _) = send(&bridge(), request(Method::GET, "/whoami", &[])).await;
        assert_eq!(body, "<none>");
    }

    #[tokio::test]
    async fn post_redirect_get_is_followed_in_process() {
        let (status, body, _) = send(&bridge(), request(Method::POST, "/todos", WEBKIT_POST)).await;

        assert_eq!(status, StatusCode::OK, "the webview was handed a redirect");
        assert_eq!(body, "<none>", "the redirect target was not fetched");
    }

    #[tokio::test]
    async fn a_redirect_off_our_origin_is_handed_over_unfollowed() {
        let (status, _, headers) =
            send(&bridge(), request(Method::POST, "/offsite", WEBKIT_POST)).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(
            headers.get(header::LOCATION).and_then(|v| v.to_str().ok()),
            Some("https://evil.example/")
        );
    }

    #[tokio::test]
    async fn a_set_cookie_is_refused_with_the_cookie_named() {
        let (status, body, headers) = send(&bridge(), request(Method::GET, "/login", &[])).await;

        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(body.contains("__Host-session"), "{body}");
        assert!(
            headers.get(header::SET_COOKIE).is_none(),
            "the cookie was passed on to a webview that will not keep it"
        );
    }

    #[tokio::test]
    async fn a_streaming_response_is_refused_with_the_capability_named() {
        let (status, body, _) = send(&bridge(), request(Method::GET, "/events", &[])).await;

        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(body.contains("streams"), "{body}");
    }

    #[tokio::test]
    async fn a_cookie_on_a_followed_hop_is_refused_not_swallowed() {
        let (status, body, _) =
            send(&bridge(), request(Method::POST, "/sign-in-then-home", &[])).await;

        assert_eq!(
            status,
            StatusCode::BAD_GATEWAY,
            "the redirect was followed past a Set-Cookie: {body}"
        );
        assert!(body.contains("__Host-session"), "{body}");
    }

    #[tokio::test]
    async fn an_unsupported_redirect_hop_is_refused_rather_than_followed() {
        let (status, body, _) =
            send(&bridge(), request(Method::POST, "/todos-then-login", &[])).await;

        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(body.contains("__Host-session"), "{body}");
    }

    #[tokio::test]
    async fn an_unknown_path_is_the_routers_own_404() {
        let (status, _, _) = send(&bridge(), request(Method::GET, "/nope", &[])).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
