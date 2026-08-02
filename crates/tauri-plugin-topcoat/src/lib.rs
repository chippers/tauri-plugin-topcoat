#![forbid(unsafe_code)]

//! Serve a topcoat application to a Tauri webview over a custom protocol.
//!
//! No port is bound and no socket is opened. topcoat's router is already a
//! function from an HTTP request to an HTTP response - `Router::handle` needs
//! none of its `serve` feature - so a Tauri custom protocol can call it
//! directly, and the request never leaves the process.
//!
//! ```no_run
//! use topcoat::router::Router;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // With topcoat's `discover` feature this is `Router::builder().discover()`.
//! let plugin = tauri_plugin_topcoat::Builder::new(Router::builder()).build()?;
//!
//! tauri::Builder::default().plugin(plugin);
//! // ...then `.run(tauri::generate_context!())` as usual.
//! # Ok(())
//! # }
//! ```
//!
//! Point the window at `topcoat://localhost/`; Tauri rewrites that to
//! `http://topcoat.localhost/` on the platforms that need it.
//!
//! # What it adds
//!
//! Two things the webview gets wrong, both measured on macOS by the `probe`
//! binary here; nobody has run it on the other two yet:
//!
//! * **One origin.** The server always sees `https://<scheme>.localhost`,
//!   whatever URL shape the platform handed the webview.
//! * **Redirects.** No webview follows a `Location` from a custom protocol, so
//!   Post/Redirect/Get is followed here instead.
//!
//! # What you cannot do
//!
//! A custom protocol response is one buffered blob, so nothing streams:
//! topcoat's `sse` feature, `datastar`, and any long-lived body have nowhere to
//! go. WebSockets need an HTTP upgrade a protocol handler can't perform.
//! Compression is dropped on the way in, and cookies don't survive in either
//! direction.
//!
//! None of that fails quietly. Use one and you get a `502` naming it, because
//! delivering half a response would have you debugging your application instead
//! of this transport.
//!
//! Everything else - pages, shards, procedures, forms, assets - goes through
//! untouched.
//!
//! # Tauri commands
//!
//! They keep working, with nothing to configure: `invoke` needs the injected
//! IPC script, and Tauri treats a page on a registered custom protocol as a
//! local origin. An application that sets a strict `Content-Security-Policy`
//! must allow `connect-src ipc: http://ipc.localhost` itself.

mod protocol;

use std::{borrow::Cow, sync::Arc, sync::OnceLock};

use custom_protocol_http::Origins;
use protocol::Bridge;
use tauri::{AppHandle, Runtime, Url, Webview, plugin::TauriPlugin};
use topcoat::{context::BaseUrl, router::RouterBuilder};

pub use custom_protocol_http::{Origin, OriginError, Platform};

/// The protocol scheme used unless [`Builder::scheme`] says otherwise.
pub const DEFAULT_SCHEME: &str = "topcoat";

/// Why a plugin could not be built.
///
/// Every variant is a configuration mistake, caught before the application
/// runs rather than as a puzzling failure once it does. More will be found, and
/// finding one should not break an application that already handles the others.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The protocol scheme is not usable. See [`OriginError`].
    #[error(transparent)]
    Scheme(OriginError),
    /// The application's base URL is the origin this protocol serves.
    ///
    /// topcoat resolves absolute URLs against its base URL, for links that
    /// leave the application: mail, feeds, sitemaps. Pointed at our own origin
    /// it would produce URLs the webview fetches for real, against a host that
    /// cannot exist. A desktop application either leaves the base URL unset or
    /// sets it to its public website.
    #[error(
        "the router's base URL `{}` is the origin this protocol serves (`{origin}`); leave it \
         unset, or set it to the application's public website",
        .base_url.as_str()
    )]
    BaseUrlCollision {
        /// The base URL the router was given.
        base_url: BaseUrl,
        /// The origin this protocol serves.
        origin: Origin,
    },
}

/// Builds the plugin.
///
/// Takes a [`RouterBuilder`] rather than a finished `Router` so the base URL
/// can be checked before the router is sealed, which is the only moment that
/// mistake is still visible.
pub struct Builder {
    router: RouterBuilder,
    scheme: String,
    /// `None` reads the webview's own `useHttpsScheme` from the application
    /// configuration, so the two cannot disagree.
    https_scheme: Option<bool>,
    allow_external_navigation: bool,
}

impl core::fmt::Debug for Builder {
    /// The router is the bulk of the value and has no `Debug` of its own, so
    /// what is shown is the configuration a reader would be checking.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Builder")
            .field("scheme", &self.scheme)
            .field("https_scheme", &self.https_scheme)
            .field("allow_external_navigation", &self.allow_external_navigation)
            .finish_non_exhaustive()
    }
}

impl Builder {
    /// Starts a plugin for `router`, on the [`DEFAULT_SCHEME`].
    #[must_use]
    pub fn new(router: RouterBuilder) -> Builder {
        Builder {
            router,
            scheme: DEFAULT_SCHEME.to_owned(),
            https_scheme: None,
            allow_external_navigation: false,
        }
    }

    /// Serves the application under a different scheme name.
    ///
    /// The window's URL must match: `<scheme>://localhost/`.
    #[must_use]
    pub fn scheme(mut self, scheme: impl Into<String>) -> Builder {
        self.scheme = scheme.into();
        self
    }

    /// Overrides [`WebviewWindowBuilder::use_https_scheme`], which changes the
    /// URL shape on the platforms that rewrite custom schemes onto http.
    ///
    /// Left alone, the plugin reads the same `useHttpsScheme` the webview does
    /// out of the application configuration, so there is no second place to
    /// keep in step. Set this only for a webview built in code with a setting
    /// the configuration does not carry.
    ///
    /// [`WebviewWindowBuilder::use_https_scheme`]: tauri::webview::WebviewWindowBuilder::use_https_scheme
    #[must_use]
    pub const fn use_https_scheme(mut self, https: bool) -> Builder {
        self.https_scheme = Some(https);
        self
    }

    /// Lets a webview showing this application navigate to another origin.
    ///
    /// Off by default. A desktop application usually wants an external link
    /// opened in the user's browser rather than replacing its own UI, and a
    /// webview that cannot reach another origin cannot host content that would
    /// try to forge requests against this one.
    #[must_use]
    pub const fn allow_external_navigation(mut self, allow: bool) -> Builder {
        self.allow_external_navigation = allow;
        self
    }

    /// Builds the plugin.
    ///
    /// # Errors
    ///
    /// [`Error::Scheme`] if the scheme name is unusable, and
    /// [`Error::BaseUrlCollision`] if the router's base URL is the origin this
    /// protocol serves.
    pub fn build<R: Runtime>(self) -> Result<TauriPlugin<R>, Error> {
        self.validate(Platform::current())?;

        // Built in `setup`, the first moment the application configuration is
        // readable, so the URL shape is discovered rather than restated.
        let bridge: Arc<OnceLock<Bridge>> = Arc::new(OnceLock::new());
        let building = Arc::clone(&bridge);
        let serving = Arc::clone(&bridge);
        let navigating = Arc::clone(&bridge);

        let Builder {
            router,
            scheme,
            https_scheme,
            allow_external_navigation,
        } = self;
        let confined = !allow_external_navigation;
        let protocol_scheme = scheme.clone();

        Ok(tauri::plugin::Builder::new("topcoat")
            .setup(move |app, _api| {
                let origins = Origins::new(&scheme, platform_for(app, https_scheme))?;
                let _ = building.set(Bridge::new(origins, router.build()));
                Ok(())
            })
            .register_asynchronous_uri_scheme_protocol(
                protocol_scheme,
                move |_context, request, responder| {
                    let bridge = Arc::clone(&serving);
                    tauri::async_runtime::spawn(async move {
                        responder.respond(match bridge.get() {
                            Some(bridge) => bridge.serve(request).await,
                            None => protocol::unavailable(),
                        });
                    });
                },
            )
            .on_navigation(move |webview, url| may_navigate(&navigating, webview, url, confined))
            .build())
    }

    /// Drives the same application without a window, as `platform` would.
    ///
    /// Everything configured here applies, so a test exercises the transport
    /// the application actually runs on. Naming the platform lets a test check
    /// a request the way Windows delivers it while running on macOS.
    ///
    /// # Errors
    ///
    /// The same as [`Builder::build`].
    pub fn session(self, platform: Platform) -> Result<Session, Error> {
        let origins = self.validate(platform)?;
        Ok(Session(Bridge::new(origins, self.router.build())))
    }

    /// Checks what can be checked while the mistake is still attached to the
    /// call that made it, and returns the origins it had to build to do so.
    ///
    /// The canonical origin does not depend on the platform, so a collision
    /// found against one platform holds for all of them.
    fn validate(&self, platform: Platform) -> Result<Origins, Error> {
        let origins = Origins::new(&self.scheme, platform).map_err(Error::Scheme)?;
        if let Some(base_url) = self.router.get_app_context::<BaseUrl>()
            && origins.collides_with(base_url.as_str())
        {
            return Err(Error::BaseUrlCollision {
                base_url: base_url.clone(),
                origin: origins.canonical().clone(),
            });
        }
        Ok(origins)
    }
}

/// Drives a router exactly as a webview would, without one.
///
/// Testing a topcoat router with `Router::handle` skips everything this plugin
/// adds: the origin rewrite, the redirect following, and the refusal of a
/// response the transport cannot carry. A [`Session`] runs all three on a plain
/// `async` call with no window, so a test fails where the application would.
///
/// Built by [`Builder::session`], so a test cannot be configured differently
/// from the application it stands in for.
///
/// ```no_run
/// # use tauri_plugin_topcoat::{Builder, Platform};
/// # use topcoat::router::Router;
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let session = Builder::new(Router::builder()).session(Platform::Scheme)?;
///
/// let response = session
///     .serve(http::Request::get("topcoat://localhost/").body(Vec::new())?)
///     .await;
///
/// assert_eq!(response.status(), 200);
/// # Ok(())
/// # }
/// ```
pub struct Session(Bridge);

impl core::fmt::Debug for Session {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

impl Session {
    /// Serves one request, redirects included.
    pub async fn serve(
        &self,
        request: http::Request<Vec<u8>>,
    ) -> http::Response<Cow<'static, [u8]>> {
        self.0.serve(request).await
    }
}

/// Decides whether a webview may go where it is going.
///
/// Narrow on purpose. Only a webview already showing one of our pages is held
/// to confinement; every other webview in the application is none of this
/// plugin's business, and neither is the first navigation into one.
fn may_navigate<R: Runtime>(
    bridge: &OnceLock<Bridge>,
    webview: &Webview<R>,
    url: &Url,
    confined: bool,
) -> bool {
    let Some(bridge) = bridge.get() else {
        return true;
    };
    let ours = bridge.origins().platform();

    let Ok(current) = webview.url() else {
        return true;
    };
    if !confined || !ours.covers(current.as_str()) {
        return true;
    }

    let target_is_ours = ours.covers(url.as_str());
    if !target_is_ours {
        // Loud without any feature turned on: a window that silently refuses to
        // navigate is the one failure where saying nothing misleads.
        eprintln!(
            "tauri-plugin-topcoat: blocked navigation to {url} in webview {}",
            webview.label()
        );
    }
    target_is_ours
}

/// The URL shape this build's webview uses, from the application configuration
/// unless the caller insisted otherwise.
fn platform_for<R: Runtime>(app: &AppHandle<R>, explicit: Option<bool>) -> Platform {
    let https = explicit.unwrap_or_else(|| {
        let windows = &app.config().app.windows;
        let any = windows.iter().any(|window| window.use_https_scheme);
        if any && !windows.iter().all(|window| window.use_https_scheme) {
            eprintln!(
                "tauri-plugin-topcoat: windows disagree about `useHttpsScheme`; \
                 serving every window over https. Set `Builder::use_https_scheme` to choose."
            );
        }
        any
    });

    match (Platform::current(), https) {
        (Platform::HttpSubdomain | Platform::HttpsSubdomain, true) => Platform::HttpsSubdomain,
        (Platform::HttpSubdomain | Platform::HttpsSubdomain, false) => Platform::HttpSubdomain,
        // No http rewrite for the setting to apply to.
        (platform, _) => platform,
    }
}
