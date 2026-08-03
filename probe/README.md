# Custom-protocol conformance probe

What a Tauri custom protocol does with a response differs across the three
webviews, and those differences shape the design of a topcoat bridge. This app
registers `probe://`, serves its own UI over it, and reports what survived.

```
cargo xtask probe
```

Two windows open. The page answers every question it can reach for itself,
provokes the ones only the server can see, and the same table lands in the main
window and on stdout. Stdout also carries a line per request, as the server saw
it.

The second window closes itself the moment it has reported. That leaves a
legible rule: a second window still on screen is one that never reported at
all.

The main window stays up, so the table is still there to read and the devtools
are still there to open. Close it when you are done, or pass `--exit` to have
the run quit as soon as it reports. Either way, after 90 seconds it gives up and
reports whatever it got, so a wedged webview still tells you something.

The clock in the header is the one thing that never finishes, and it is not a
local timer. Every animation frame the page asks the server for the header and
paints what comes back, formatted characters and all: the elapsed time, the
number of round trips, what they have been averaging. So the clock is an
assertion. None of it can advance unless the protocol is still answering at
frame rate, and a stutter up there is a stutter in the thing being measured. It
is also this repository's whole premise written small and run fast, since a
server rendering its responses for a webview to display is the arrangement a
topcoat bridge exists to carry.

The page contributes only the timings, because only the end that sent a request
knows when it sent it. It reports two, and they disagree by a factor of five, so
the transport pane shows the round trip taken apart.

A request issued from inside a `requestAnimationFrame` callback is not timed
until the browser has finished rendering, because no response can be delivered
mid-render. So the request that draws the clock reads around a millisecond,
while the identical request in a loop doing nothing else reads around two
hundred microseconds - and the second is the number the devtools network panel
shows. Both are true; only one is about the transport. The clock is therefore
paced by frames but *timed* by a short burst of back-to-back round trips, run
once at startup and again once the probes are done.

Individual readings are never shown either. `performance.now()` is clamped to
something coarse in every webview, so one sample of a sub-millisecond span is a
quantisation artefact where the mean of a few hundred is not.

The clock is the one request stdout does not log. Sixty lines a second would
bury the ones worth reading.

Building it needs Node and pnpm, because the page is a real frontend build and
the probe serves that build's output. It is the only crate here that needs
either; the root README says what CI pins them to.

## The two things that cannot drift

**No probe can be missing a row.** Every question lives in one table in
`src/report.rs`, and the report is an array indexed by it. Each entry also
declares the answer that stands when nothing reports it, so an empty row never
has to be interpreted.

**No type is written twice.** The ids, the verdicts and the report rows are
Rust types; `ui/src/bindings.ts` is generated from them by
[Specta]/[tauri-specta] and the page imports it. Both channels land in that one
file, because one builder exports them: `probe_ipc` crosses by Tauri's IPC, and
the report crosses as JSON over the custom protocol. The page's probe table is
keyed by the generated union, so adding a probe in Rust makes TypeScript refuse
to build until the page either answers it or marks it the server's. A test
rewrites the bindings whenever they stop matching the Rust and fails saying so,
and CI type-checks the page against them, so neither half can quietly stop
agreeing with the other.

That is why the probe is built this way rather than with a hand-written
contract: a conformance tool whose two halves disagree about a probe's name
reports nothing you can trust.

None of it is advice for a topcoat application, which has no JSON surface to
hold together and already generates the client for the one boundary it does have
- see the root README.

## Results

`yes` means the webview behaved the way a real HTTP client would. `info` is a
value rather than a verdict, and `unknown` means nothing established one.

Every probe also declares what it is *expected* to answer, which is the macOS
column below written down where the run can check itself against it. So on a
platform already measured the whole column agrees and says nothing, and a row
whose expectation prints in `[brackets]` is one where this webview did not do
what the last one did - which is the entire reason to run this on a second
platform. A predicted `no` is a `no` that is not news, and several of them are
the findings the rest of this repository is designed around.

| probe | macOS 26 / WKWebView | Windows / WebView2 | Linux / WebKitGTK |
| --- | --- | --- | --- |
| Content-Encoding: gzip is decoded | no | | |
| an ES module loads over the scheme | yes | | |
| Resource Timing records what was fetched | no | | |
| a fetch POST delivers its body | yes | | |
| a form POST delivers its body | yes | | |
| Set-Cookie comes back on the next request | no | | |
| document.cookie sees them | no | | |
| fetch follows a 303 | no | | |
| a navigation follows a 303 | no | | |
| a Range request header arrives | yes | | |
| the CSP document rendered at all | yes | | |
| a Content-Security-Policy we send is enforced | yes | | |
| invoke reaches a Tauri command | yes | | |
| ...and the command knows which window called | main | | |
| ...in a window we send a CSP with | yes | | |
| ...in a subframe of one | no | | |
| localStorage works | yes | | |
| sessionStorage works | yes | | |
| IndexedDB works | yes | | |
| a foreign document runs in a frame at all | yes | | |
| a cross-origin POST reaches the server | yes | | |
| ...and names its origin when it does | no | | |
| ...and what scheme its Referer names | data | | |

Headers the server received, macOS:

- on a `fetch` POST: `accept`, `content-type`, `referer`, `user-agent`
- on a form POST: the same, plus `origin` and `upgrade-insecure-requests`

Absent on both: `Origin` (for `fetch`), every `Sec-Fetch-*`, `Host`, `Cookie`,
`Accept-Encoding`.

## Why each probe is here

**Compression.** topcoat enables gzip and brotli by default and negotiates from
`Accept-Encoding`. If the webview does not decode what it is handed, a
compressed response renders as garbage.

**Modules and subresources.** The page is a document, a stylesheet, a script and
an ES module, each fetched separately, because that is what a real frontend
build emits. A webview that mangles any of them over a custom scheme breaks
every app built on it, not just this one.

**Resource Timing.** `performance.getEntriesByType("resource")` comes back
empty on macOS - not short, empty, for the stylesheet and the scripts and the
module and every `fetch`, though `navigation` and `paint` entries are there. So
an application served over a custom scheme cannot time its own transport from
the page: no real-user monitoring of its own asset loads, no latency histogram.
The devtools network panel still shows the timings, because it reads them from
inside the engine rather than from anything the page can call. It is also why
the clock in the header holds its own stopwatch.

**Request bodies.** topcoat's shards and procedures POST via `fetch`; forms POST
by navigating. The two paths are handled by different webview code and can
disagree.

**Cookies.** topcoat's default session token store forces `Secure` and the
`__Host-` prefix. This is what the whole design turns on: cookies do not
survive. The bridge carries the session token in the process instead and
refuses any other cookie outright rather than dropping it quietly.

**Redirects.** Post/Redirect/Get is the normal shape of a topcoat mutation. A
webview that does not follow `Location` from a scheme handler breaks it.

**IPC.** The question underneath everything else: does a page a framework
rendered still reach Tauri's own APIs? On macOS it does, including from a window
whose document we served with `default-src 'self'` - an application can set its
own policy and keep every native capability. It does *not* reach a subframe, and
that is worth knowing before designing around iframes; the probe asks all three
because a failure in one alone would not say which cause it was.

The command also reports which window called it, because that identity is what
`tauri-plugin-topcoat` hangs a session on.

**Headers.** topcoat's CSRF check reads `Sec-Fetch-Site`, falls back to
comparing `Origin` against `Host`, and passes when neither is present - on the
reasoning that such a client carries no ambient cookies. That reasoning has to
keep holding, which is why the bridge attaches no credential to a request and
strips what a client attached by itself. A security question, not a
compatibility one.

The `Sec-Fetch-*` absence is not a webview defect and will not be fixed. Fetch
Metadata is appended only to a [potentially trustworthy
URL](https://w3c.github.io/webappsec-secure-contexts/#potentially-trustworthy-url),
and a non-special scheme has an [opaque
origin](https://url.spec.whatwg.org/#concept-url-origin), which that algorithm
returns "Not Trustworthy" for. So `scheme://localhost` can never receive them.
`http://scheme.localhost` is a different matter - a host ending in `.localhost`
*is* potentially trustworthy - so Windows and Android should send Fetch
Metadata where macOS, iOS and Linux cannot. Worth confirming, because it means
the CSRF signal is strictly weaker on the platforms measured here.

**Cross-origin requests.** With `Sec-Fetch-Site` structurally unavailable,
`Origin` is the only thing left that can distinguish a mutation our own document
asked for from one a foreign document forged. Fetch requires `Origin` on every
request whose method is not `GET` or `HEAD`, so a hostile frame should name
itself. The probe puts a document on an opaque origin - a `data:` URL in an
iframe - and has it POST at us. An image beacon in that same document is what
separates "the POST was refused" from "the document never ran".

On macOS it runs, the POST is delivered, and **it arrives with no `Origin`**.
Nothing in the request distinguishes it from one this page made, except a
`Referer` naming the `data:` URL. So any ambient authority the shell attaches to
a request is forgeable by anything that can get a frame onto the page, which is
the second reason the bridge attaches none.

**CSP.** Determines whether the bridge can set a policy, and whether a policy
set by the app would break Tauri's injected IPC script.

**Web storage.** A custom scheme could be an opaque origin, in which case
`localStorage` throws on access rather than returning empty and the page loses
every client-side persistence option at once. On macOS it is a real origin and
all three work, so cookies are the only thing missing.

[Specta]: https://github.com/specta-rs/specta
[tauri-specta]: https://github.com/specta-rs/tauri-specta
