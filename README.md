# tauri-plugin-topcoat

Serve a [topcoat](https://github.com/tokio-rs/topcoat) application to a Tauri
webview over a custom protocol. No port is bound, no socket is opened, and the
request never leaves the process. **[API documentation](https://chippers.github.io/tauri-plugin-topcoat/)**.

![The custom-protocol probe on macOS and on iOS, each reporting what its webview
did with the same request](.github/assets/probe.png)

Note: Android is currently unsupported due to a webview API limitation. Whatever
general workaround Tauri lands on for Android will be used here.

topcoat's router is already a function from an HTTP request to an HTTP response.
`Router::handle` needs none of its `serve` feature, so a Tauri custom protocol
can just call it.

```rust
let plugin = tauri_plugin_topcoat::Builder::new(Router::builder().discover()).build()?;

tauri::Builder::default()
    .plugin(plugin)
    .run(tauri::generate_context!())?;
```

Point the window at `topcoat://localhost/`.


## What it does

Two translations. No specification is written for a custom protocol handler -
nothing obliges one to follow a `Location` or keep a cookie - so what a webview
does has to be measured. Then you look for a standard solid enough to stand on,
so the ordinary web keeps working with nothing rewritten for the transport.
Tauri has been doing that for a while, so this builds on their work.


### Normalised origin

Your protocol is `scheme://localhost` on macOS, iOS and Linux,
and `http://scheme.localhost` on Windows and Android. The router always sees
`https://scheme.localhost`, so you never write a platform-conditional notion of
your own address.

That's what lets an ordinary CSRF check work. Such a check parses `Origin` and
ignores any scheme that isn't `http` or `https`, so `scheme://localhost` matches
nothing and your own form post gets a `403`. Rewritten, it passes, and a foreign
origin still doesn't. topcoat's check is more forgiving about the scheme, but it
compares `Origin` against `Host`, so the two have to move together or it refuses
you just the same.


### Redirects

No webview follows a `Location` from a custom protocol, so
Post/Redirect/Get - the ordinary shape of a mutation - is followed in the
process by `tower-http`'s `FollowRedirect` under a same-origin policy.

The plugin attaches no authority to your requests, and strips what a client
attached by itself: no cookie jar, no token. `Authorization` is left alone,
because a document sets that one deliberately and a foreign page cannot.

It does rewrite headers. `Host` gets set, because no webview reliably sends one
and a server comparing `Origin` against it needs both. `Origin` and `Referer`
get moved onto the canonical origin when they were yours to begin with; a
foreign one is left alone so it still looks foreign.

Attaching no authority is a security property. topcoat decides whether a
request may act partly by noticing that a request with neither `Origin` nor
`Sec-Fetch-Site` also has no ambient authority to forge with, and a webview's
`fetch` to a custom protocol sends neither. Put a cookie jar here and that
reasoning is false, and the whole CSRF question moves out of topcoat and into
this plugin.

`Sec-Fetch-Site` is missing by specification, so don't lean on it: Fetch
Metadata goes only to a trustworthy origin, and a custom scheme is not one.
Windows and Android should be the exception, because a host ending in
`.localhost` is trustworthy and `http://topcoat.localhost` is where they put
you. Nobody has run the probe there to confirm it. `Origin` compared against
`Host` is the signal that exists everywhere. [The probe's
README](probe/README.md#why-each-probe-is-here) cites the specifications and
says why the absence is permanent.

## What you can't do

A custom protocol response is one buffered blob, so nothing streams: topcoat's
`sse` feature, `datastar`, and any long-lived body have nowhere to go.
WebSockets don't have an underlying transport to upgrade. Compression is
dropped on the way in, because there's no wire to save bytes on. I also don't
know what every platform decodes properly. Cookies don't survive in either
direction.

None of that fails quietly. Use one and you get a specific `502`; delivering
half a response would have you debugging your application instead of this
plugin. The list is an enum, so the next incompatibility somebody measures
breaks the build until it has a message.

Everything else - pages, shards, procedures, forms, assets - goes through
untouched.

## Sessions

topcoat puts its session token in a `__Host-` prefixed, `Secure`, `HttpOnly`
cookie, and WebKit throws away every cookie a custom protocol sets. Sign in and
it looks like it worked; the next request arrives anonymous.

The `session` feature swaps the transport at topcoat's own seam. `TokenStore`
decides where the token lives between requests and nothing else, so minting,
hashing, expiry, `start` and `stop` and `rotate`, and your session storage are
all still topcoat's.

```rust
let plugin = tauri_plugin_topcoat::Builder::new(router)
    .sessions(SessionConfig::builder())
    .build()?;
```

The token stays in your process, keyed by the webview that asked, and never
crosses into the webview at all. Not `document.cookie`, not a header a script
can read, not anything WebKit writes to disk. A browser has to hand a client its
token because the server is somewhere else. Here it's the same process, and
Tauri tells you which webview every request came from.

So a script that can read every byte the webview holds still can't read the
session token, because the token was never in there.

The cost: the token is ambient with respect to the webview, so whatever document
that webview is showing can use it. Navigation confinement defends that and is
on by default - leave it on. Even with it off, a webview seen on somebody
else's origin stops being handed the token. The feature itself is off by
default, because the core holds no credential and this is what changes that.

Confinement governs navigation, not sub-resources. Embed a frame from somewhere
else and your webview is still showing your origin, so the token is still handed
out - what stops that frame spending it is topcoat's origin check, which
refuses a mutation whose `Origin` isn't yours. Serve a `Content-Security-Policy`
if you'd rather it couldn't load at all. The probe measures whether a webview
attaches `Origin` in that case, since that's the assumption underneath.

## Tracing

The `tracing` feature reports what the plugin decided, which nothing else can
see: a request served and how it ended, a navigation blocked, a response refused
and which capability did it, a session handed over or withheld and which of the
four rules withheld it. Each request is a `serve` span naming its webview.

The sessions example turns it on, so `cargo xtask session` shows a redirect
being followed and a token handed over as they happen.

The token never appears, in any form - none of the functions that report a
session take a token at all.

## Types

topcoat already generates its own client, so a topcoat application needs nothing
here. `#[procedure]` expands to a `Procedure<A, R>` that keeps both types, its
route id is minted at macro expansion, and the handle reaches the page as
`{"t": "Procedure", "id": ...}` - identity and type mapping both derived from
the Rust. There is no hand-written client that could drift from it.

The probe is the different case, because it does have a JSON surface of its own:
a page telling a Rust server what a webview just did. It generates its
TypeScript from the Rust as well, and [its
README](probe/README.md#the-two-things-that-cannot-drift) says what holds the
two halves together.

## Layout

| |                                                                                          |
| --- |------------------------------------------------------------------------------------------|
| `crates/custom-protocol-http` | The rules, as pure functions and tower layers. Tied to no shell, publishable on its own. |
| `crates/example-harness` | Requests in the shape a webview delivers them, for the examples' tests.                  |
| `crates/tauri-plugin-topcoat` | The Tauri half: protocol handler, navigation confinement, session transport.             |
| `examples/hello` | The least that renders. topcoat's own, with the plugin where `start` was.                |
| `examples/session` | topcoat's own sessions example, with the token held out of the webview.                  |
| `examples/todos` | topcoat's toasty example, persisting to SQLite, and the tests that drive it without a window. |
| `probe` | What each webview does with a custom protocol. Run it per platform.                      |
| `xtask` | `cargo xtask <task>`, the dev entrypoint.                                                |

CI runs `cargo xtask lint` and `cargo xtask test`; `cargo xtask check` is both,
and defines no gate CI does not. `cargo xtask hooks` points git at `.githooks`,
so `check` runs before every commit too - once per clone, since nothing can
track `.git/hooks` for you. `cargo xtask showcase` opens all three examples and
then the probe, one at a time, each up until you close it.

`release` stays near cargo's defaults, because the first thing anyone does here
is clone and run `showcase` once, and fat LTO turns that into a coffee break.
The size knobs live in `crunch`. `cargo xtask crunch` builds every application
under it and prints the sizes - 1.7 to 3.2 MiB on an aarch64 mac. It wants
nightly and `rust-src`, since most of what is left is the standard library.

Every example needs nothing but Rust. The todos one additionally compiles SQLite
from C, because `toasty-driver-sqlite` pins `rusqlite`'s `bundled` feature with
no way to turn it off - a cold build is 15 to 30 seconds longer, and the
compiler it needs is one you already have, since linking any Rust binary wants
it. The probe is the one crate that needs a toolchain you might not: Node and
pnpm, because its page is a real frontend build. CI runs the current Node LTS;
the pnpm version is pinned in `probe/ui/package.json`, so `corepack enable` gets
you the one CI uses.

`custom-protocol-http` is a tower stack, so the inner service can be anything -
an axum router, a `ServeDir`, topcoat. Its tests assert the `FollowRedirect`
behaviour we depend on, so an upgrade that changes it fails there instead of in
your app.

## Testing your application

`Builder::session` drives the same configuration without a window - origin
rewriting, redirect following, refusals and the session transport included - on
a plain `async` call. `Router::handle` on its own skips all of it.

```rust
let session = plugin().session(Platform::Scheme)?;
let response = session.serve(request).await;
```

Naming the platform lets you check a request the way Windows delivers it while
running on macOS. Building it from the same `Builder` your app uses means the
test can't be configured differently from the app it stands in for.

## Status

A personal experiment, and a small one on purpose. It exists to show the
combination works and to write down what the three webviews do with a custom
protocol, which is knowledge that otherwise lives in scattered issues and radar
numbers.

It comes with no support, and I have no plans to maintain it. Issues and pull
requests may sit. If you want to depend on any of it, fork it or vendor it.

What I'd rather not be is the person responsible for keeping it current. If you
think this belongs somewhere it would actually be looked after, I'm glad to
help you move it there - open an issue and we'll work out where.

Measured on macOS only. Run `cargo xtask probe` on Windows or Linux and it
prints the column to paste into `probe/README.md`. Nobody has done that yet, so
those two columns are empty rather than confirmed.

## Trademarks

TAURI&reg; is a trademark of the Tauri Programme within The Commons Conservancy.
This plugin is not built, endorsed or approved by them; only what lives in the
[`tauri-apps`](https://github.com/tauri-apps) organization is official. The name
follows the `tauri-plugin-*` convention their [trademark
policy](https://v2.tauri.app/about/trademark/) allows for third-party plugins.
No Tauri logo or wordmark is reproduced anywhere here; the probe's window borrows
the two brand colours as accents and nothing else.

## Commits

Conventional Commits, with scopes shortened to one per crate instead of the full
package name - `tauri-plugin-topcoat` leaves no room for a description inside a
72 character subject:

| scope | crate |
| --- | --- |
| `probe` | `topcoat-probe` |
| `protocol-http` | `custom-protocol-http` |
| `plugin` | `tauri-plugin-topcoat` |
| `example` | every `example-*` |

The examples share a scope rather than take one each, because which one a commit
touched belongs in the subject where a reader will see it.

Root-level configuration takes no scope; workflow changes take `ci`. Subjects
carry the change, pull requests carry the reasoning; a body is for the rare
commit that needs one.
