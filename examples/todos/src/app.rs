//! What the window is shown, and the four routes behind it.
//!
//! Reads top to bottom against topcoat's own `examples/toasty-todo`, which is
//! where the model, the routes and the components come from. Every place this
//! differs carries its reason where it happens; `main.rs` has the list.

use serde::Deserialize;
use topcoat::{
    Result,
    context::{Cx, CxBuilder, app_context},
    router::{
        Body, HeaderValue, IntoResponse, Next, Response, Router, RouterBuilderDiscoverExt,
        StatusCode,
        content::{Css, Form},
        error::{SeeOther, see_other},
        header, layer, layout, page, path_param, route,
    },
    view::{component, view},
};

use crate::store::{Store, Title, Todo};

/// The application and its transport, as one value.
///
/// One function, so the tests below drive the same configuration the window
/// does. It takes the [`Store`] rather than opening one, because
/// [`Session`](tauri_plugin_topcoat::Session) has no `AppHandle` to ask for a
/// data directory and a test has no business writing to one anyway.
pub fn plugin(store: Store) -> tauri_plugin_topcoat::Builder {
    tauri_plugin_topcoat::Builder::new(Router::builder().discover().app_context(store))
}

/// The database, for a handler.
///
/// Upstream hands back a cloned `Db`; here the clone is [`Store`]'s business,
/// so a handler asks for todos rather than issuing statements.
fn store(cx: &Cx) -> &Store {
    app_context(cx)
}

/// The policy every response leaves with.
///
/// Strict because it can afford to be, and it can afford to be because of what
/// is not here: no script of any kind, so `default-src 'none'` needs no
/// `script-src` beside it; no image, icon or font, so no `img-src` or
/// `font-src`; no `fetch` and no `invoke`, so no `connect-src`. The stylesheet
/// is the single exception, and it is served from this origin.
///
/// That is not an accident of a small example. Every route to client-side
/// interactivity in topcoat 0.5 ends at `script-src 'unsafe-eval'` - the
/// browser runtime compiles its expressions with `new Function`, and so does
/// Alpine - or at the `topcoat` CLI for an asset bundle, or at both. `eval`
/// here is worth more to an attacker than on the web, because the probe
/// measured that Tauri's IPC still reaches a document served this way. The
/// round trip this gives up is in-process and costs microseconds.
///
/// `tauri.conf.json` cannot carry this. Tauri attaches the `csp` it configures
/// to the assets it serves from `tauri://localhost`, and every document here
/// comes from the plugin's protocol instead.
///
/// Malformed, it fails the build rather than the first response.
const POLICY: HeaderValue = HeaderValue::from_static(
    "default-src 'none'; \
     style-src 'self'; \
     form-action 'self'; \
     base-uri 'none'; \
     frame-ancestors 'none'",
);

/// Attaches [`POLICY`] to every response the application renders, and says so
/// when it renders none.
///
/// The log line is the half a desktop application otherwise loses. topcoat's
/// terminal conversion turns any error it does not recognise into the literal
/// body `internal server error` and drops the cause; on a server there is a log
/// to go and read, and in a window there is nothing.
///
/// The policy does not ride a framework error, because it cannot: `next.run`
/// borrows `cx` for the rest of this function, so a layer cannot render or
/// respond after the fact, and the error is still an `Err` at this point. Those
/// bodies are `text/plain` with nothing in them anybody chose, so the gap is
/// narrow - but it is a gap, not a claim this layer gets to make.
#[layer("/")]
async fn policed(cx: &mut CxBuilder, body: Body, next: Next<'_>) -> Result<Response> {
    let mut response = next.run(cx, body).await.inspect_err(|error| {
        tracing::error!(%error, "the request failed");
    })?;

    response
        .headers_mut()
        .insert(header::CONTENT_SECURITY_POLICY, POLICY);

    Ok(response)
}

/// Wraps every page in a complete HTML document.
///
/// A layout sees a page's error before it becomes a response, which is the one
/// place a failed query can still be rendered inside the shell rather than
/// replacing it with plain text. The message is shown rather than swallowed
/// because there is no untrusted client here, and the person reading it is the
/// only one who can act on it.
#[layout("/")]
async fn root(slot: Result) -> Result {
    let content = match slot {
        Err(error) if error.downcast_ref::<toasty::Error>().is_some() => view! {
            // Declared before the slot renders, so it is the status that wins.
            (StatusCode::INTERNAL_SERVER_ERROR)

            <h1>"Toasty Todos"</h1>
            <p class="note">
                "The database refused this one: "
                (error.to_string())
            </p>
        },
        content => content,
    }?;

    view! {
        <!DOCTYPE html>
        <html>
            <head>
                <title>"Toasty Todos"</title>
                <link rel="stylesheet" href="/app.css">

                // No `topcoat::dev::script()`. It reloads the page once a new
                // build is serving, and a rebuild restarts this process - so
                // the window is already new and holds nothing stale to discard.
            </head>
            <body>(content)</body>
        </html>
    }
}

/// The stylesheet, served rather than inlined.
///
/// An inline `<style>` would need `'unsafe-inline'` in [`POLICY`], or a hash of
/// the block that every edit to it invalidates. A file needs neither.
///
/// `include_str!` and not `asset!`: an asset comes out of a bundle the
/// `topcoat` CLI builds, and this example is meant to run on `cargo run` with
/// nothing else installed. Content hashing and immutable caching buy nothing
/// when the response is a buffer handed across one process.
#[route(GET "/app.css")]
async fn stylesheet(cx: &Cx) -> Result<Response> {
    Css(include_str!("app.css")).into_response(cx)
}

#[page("/")]
async fn home(cx: &Cx) -> Result {
    view! {
        <h1>"Toasty Todos"</h1>

        // Submit a new todo to POST /todos.
        //
        // `autofocus` puts the caret back after every add, because each one is
        // a full document navigation and the window is otherwise left with
        // nothing focused.
        <form method="post" action="/todos">
            <input
                type="text"
                name="title"
                placeholder="What needs doing?"
                required=""
                autofocus=""
            >
            <button type="submit">"Add"</button>
        </form>

        // Load all todos and display them in creation order.
        let todos = store(cx).all().await?;

        if todos.is_empty() {
            <p>"All done!"</p>
        } else {
            <ul class="todos">
                for todo in todos {
                    <li class="todo">
                        toggle_checkbox(todo: &todo)

                        <span id=(title_id(&todo))>
                            if todo.done {
                                <s>(&todo.title)</s>
                            } else {
                                (&todo.title)
                            }
                        </span>

                        delete_button(todo: &todo)
                    </li>
                }
            </ul>
        }
    }
}

// --- Components -------------------------------------------------------------

/// The id a row's title carries, so the toggle beside it can name its label.
///
/// One home for a string both ends have to agree on, and a tuple rather than a
/// `format!` because an attribute value can be built from one.
fn title_id(todo: &Todo) -> (&'static str, u64) {
    ("todo-", todo.id)
}

/// A checkbox that submits, drawn without a line of script.
///
/// Upstream is `<input type="checkbox" onchange="this.form.submit()">`. An
/// inline handler attribute needs `script-src 'unsafe-inline'`, so under
/// [`POLICY`] it never runs and the checkbox does nothing. A submit button is
/// the only markup that changes state on one click with no script at all, so
/// the control becomes one and CSS draws the tick.
///
/// `aria-checked=(todo.done)` would be a bug: an expression attribute whose
/// value is `false` is removed entirely, and `aria-checked` is enumerated
/// rather than boolean, so absent and `"false"` are different answers.
/// Upstream's `checked=(todo.done)` was right because `checked` really is a
/// boolean attribute.
#[component]
async fn toggle_checkbox(todo: &Todo) -> Result {
    view! {
        <form method="post" action=(("/todos/", todo.id, "/toggle"))>
            <button
                type="submit"
                class="toggle"
                role="checkbox"
                aria-checked=(if todo.done { "true" } else { "false" })
                aria-labelledby=(title_id(todo))
            ></button>
        </form>
    }
}

/// Submits the todo id to its delete endpoint.
#[component]
async fn delete_button(todo: &Todo) -> Result {
    view! {
        <form method="post" action=(("/todos/", todo.id, "/delete"))>
            <button type="submit" class="delete">"delete"</button>
        </form>
    }
}

// --- Routes -----------------------------------------------------------------

#[derive(Deserialize)]
struct NewTodo {
    title: String,
}

/// Parses the dynamic todo id, answering `400` when it is not a number.
///
/// topcoat 0.5 spells this as an attribute on a tuple struct; the function-like
/// `path_param!` upstream uses is on topcoat's `main` and not released. The
/// name has to stay `TodoId`, because the macro snake-cases it into the
/// `{todo_id}` segment.
#[path_param(error = bad_request)]
struct TodoId(u64);

#[route(POST "/todos")]
async fn create(cx: &Cx, Form(new): Form<NewTodo>) -> Result<SeeOther> {
    // A blank title is not an error, it is a form submitted by accident.
    if let Some(title) = Title::parse(&new.title) {
        store(cx).add(&title).await?;
    }

    // Post/Redirect/Get. No webview follows a `Location` from a custom
    // protocol, so the plugin follows this one in the process and the window is
    // handed the list it names.
    Ok(see_other("/"))
}

#[route(POST "/todos/{todo_id}/toggle")]
async fn toggle(cx: &Cx) -> Result<SeeOther> {
    match store(cx).toggle(*path_param::<TodoId>(cx)?).await {
        // Another window deleted it. The list is about to say so, which is a
        // better answer than a 500 about a todo that is already gone.
        Err(error) if error.is_record_not_found() => {
            tracing::debug!(%error, "toggled a todo that was already gone");
        }
        result => result?,
    }

    Ok(see_other("/"))
}

#[route(POST "/todos/{todo_id}/delete")]
async fn delete(cx: &Cx) -> Result<SeeOther> {
    store(cx).remove(*path_param::<TodoId>(cx)?).await?;

    Ok(see_other("/"))
}

/// The application driven the way the webview drives it.
///
/// Origin rewriting, redirect following and the transport's refusals included,
/// with no window in sight. Each test gets its own in-memory database, so they
/// share nothing and none of them touches the user's real one.
#[cfg(test)]
mod tests {
    use example_harness::{get, response, submit};
    use scraper::{Html, Selector};
    use tauri_plugin_topcoat::{Platform, Session};

    use super::*;

    async fn window() -> Session {
        let store = Store::in_memory().await.expect("an in-memory database");

        plugin(store)
            .session(Platform::Scheme)
            .expect("the plugin is configured correctly")
    }

    /// The one attribute a selector picks out of the page.
    ///
    /// Parsed and not searched for, because a test that reads the page wrongly
    /// is a test that passes wrongly.
    fn attribute(page: &str, selector: &str, name: &str) -> String {
        let html = Html::parse_document(page);
        let selector = Selector::parse(selector).expect("a valid selector");
        html.select(&selector)
            .next()
            .and_then(|element| element.attr(name))
            .unwrap_or_else(|| panic!("no {selector:?} with {name} in: {page}"))
            .to_owned()
    }

    /// Reads a row's action out of the page rather than guessing the id.
    ///
    /// The rendered markup is the only place the two ends agree, so a test that
    /// wrote `/todos/1/toggle` itself would keep passing after the page stopped
    /// pointing there.
    fn action(page: &str, verb: &str) -> String {
        attribute(page, &format!(r#"form[action$="/{verb}"]"#), "action")
    }

    async fn with_one_todo() -> (Session, String) {
        let window = window().await;
        let (status, page) = submit(&window, "/todos", "title=buy+milk").await;
        assert_eq!(
            status,
            StatusCode::OK,
            "adding did not land back on the list"
        );
        (window, page)
    }

    /// The load-bearing one.
    ///
    /// Proves Post/Redirect/Get works over a transport whose webview follows no
    /// redirect, that the write reached SQLite, and that the render after it saw
    /// the write - in one assertion.
    #[tokio::test]
    async fn adding_a_todo_lands_back_on_the_list() {
        let (_, page) = with_one_todo().await;

        assert!(
            page.contains("buy milk"),
            "the list did not come back: {page}"
        );
    }

    #[tokio::test]
    async fn a_blank_title_adds_nothing() {
        let window = window().await;

        let (status, page) = submit(&window, "/todos", "title=+++").await;

        assert_eq!(status, StatusCode::OK);
        assert!(
            page.contains("All done!"),
            "a blank todo was stored: {page}"
        );
    }

    #[tokio::test]
    async fn toggling_strikes_it_through_and_back() {
        let (window, page) = with_one_todo().await;
        // Not `toggle`: `#[route]` puts a constant of that name in this scope,
        // and a `let` would match against it rather than bind.
        let toggling = action(&page, "toggle");

        let (status, page) = submit(&window, &toggling, "").await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("<s>buy milk</s>"), "{page}");

        let (_, page) = submit(&window, &toggling, "").await;
        assert!(!page.contains("<s>"), "it stayed struck through: {page}");
    }

    #[tokio::test]
    async fn deleting_removes_it() {
        let (window, page) = with_one_todo().await;

        let (status, page) = submit(&window, &action(&page, "delete"), "").await;

        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("All done!"), "it survived: {page}");
    }

    /// Proves the `error = bad_request` on the path parameter is wired.
    #[tokio::test]
    async fn a_todo_id_that_is_not_a_number_is_a_bad_request() {
        let window = window().await;

        let (status, _) = submit(&window, "/todos/nope/toggle", "").await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// Two windows on one list is an ordinary desktop situation.
    #[tokio::test]
    async fn toggling_a_todo_that_is_gone_lands_back_on_the_list() {
        let window = window().await;

        let (status, page) = submit(&window, "/todos/404/toggle", "").await;

        assert_eq!(
            status,
            StatusCode::OK,
            "a todo that is already gone was a 500"
        );
        assert!(page.contains("All done!"), "{page}");
    }

    #[tokio::test]
    async fn the_policy_rides_the_page_and_the_stylesheet() {
        let window = window().await;

        for path in ["/", "/app.css"] {
            let served = response(&window, path).await;

            assert_eq!(
                served.headers().get(header::CONTENT_SECURITY_POLICY),
                Some(&POLICY),
                "{path} was served without the policy"
            );
        }
    }

    /// `style-src 'self'` is only honest if the linked stylesheet is served.
    ///
    /// The path is read out of the rendered page rather than written here
    /// again, so a link that stops naming its route fails this.
    #[tokio::test]
    async fn the_stylesheet_the_page_links_is_served() {
        let window = window().await;
        let (_, page) = get(&window, "/").await;
        let href = attribute(&page, r#"link[rel="stylesheet"]"#, "href");

        let (status, body) = get(&window, &href).await;

        assert_eq!(status, StatusCode::OK, "{href} is linked but not served");
        assert!(body.contains(".toggle"), "{body}");
    }

    /// The failed-query page, which is the one thing a layout can render that a
    /// route cannot.
    ///
    /// Clobbering the file under an open connection is the only way to make a
    /// query fail through the public API, and it is worth the trouble: without
    /// this, the branch in [`root`] that shows the database's own words is
    /// dead code that renders for the first time on a user's machine.
    #[tokio::test]
    async fn a_failed_query_is_still_a_page() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("todos.db");
        let store = Store::open(&path).await.expect("a new database");
        let window = plugin(store)
            .session(Platform::Scheme)
            .expect("the plugin is configured correctly");

        std::fs::write(&path, b"not a database anymore").expect("the file is clobbered");

        let (status, page) = get(&window, "/").await;

        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "the status the view declares did not win"
        );
        assert!(
            page.contains("The database refused this one"),
            "the cause was swallowed: {page}"
        );
        assert_eq!(
            attribute(&page, r#"link[rel="stylesheet"]"#, "href"),
            "/app.css",
            "the failure replaced the document instead of filling it: {page}"
        );
    }

    /// The test that makes [`POLICY`] enforceable rather than aspirational.
    ///
    /// The failure it guards is a page that renders perfectly in every other
    /// test here and comes up unstyled or inert in the window, which is an
    /// expensive class of bug to go looking for. This turns it red instead.
    #[tokio::test]
    async fn nothing_the_policy_forbids_is_in_the_markup() {
        let (_, page) = with_one_todo().await;
        let html = Html::parse_document(&page);
        let every = Selector::parse("*").expect("a valid selector");
        // Or a page that parsed to nothing would satisfy every rule below.
        assert!(html.select(&every).count() > 5, "{page}");

        for element in html.select(&every) {
            let element = element.value();
            let name = element.name();
            assert!(name != "style", "an inline style block: {page}");
            assert!(name != "script", "a script: {page}");

            // Every `on*`, not a list of the four this page might have grown.
            for (attribute, _) in element.attrs() {
                let attribute = attribute.to_ascii_lowercase();
                assert!(attribute != "style", "an inline style attribute: {page}");
                assert!(
                    !attribute.starts_with("on"),
                    "an inline handler `{attribute}`: {page}"
                );
            }
        }
    }
}
