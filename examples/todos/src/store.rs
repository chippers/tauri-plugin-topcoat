//! Every todo, and every query this application makes.
//!
//! Nothing here knows it is behind a router or inside a window. The isolation
//! is the import list above, which a reviewer can check by reading it, and what
//! it buys is a data layer whose tests need neither.

// `toasty::Model` generates a dozen types - `TodoFields`, `TodoQuery`,
// `TodoCreate`, the upsert builders - none of which derive `Debug`. They are
// emitted as siblings of the struct while carrying its span, so an `allow` on
// the struct does not reach them and this has to be the file.
#![allow(missing_debug_implementations)]

use std::{
    fmt,
    path::{Path, PathBuf},
};

use thiserror::Error;
use toasty::Db;
use toasty_driver_sqlite::Sqlite;

/// A todo, as it is stored.
#[derive(Debug, toasty::Model)]
pub struct Todo {
    /// Assigned by SQLite, and the order the list is drawn in.
    #[key]
    #[auto]
    pub id: u64,

    /// What there is to do.
    pub title: String,

    /// Whether it has been done.
    pub done: bool,
}

/// A title with something in it.
///
/// The rule that a blank todo is not a todo, written as a type instead of an
/// `if` in a handler. Parsed where the form arrives, so nothing below can be
/// handed an empty one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Title(String);

impl Title {
    /// Trims `raw`, and is [`None`] when nothing is left of it.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Title> {
        let trimmed = raw.trim();
        (!trimmed.is_empty()).then(|| Title(trimmed.to_owned()))
    }

    /// The title, trimmed.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The database, opened once and shared by every request.
///
/// Holds the [`Db`] rather than handing it out, so the clone every statement
/// needs happens in one place. Cloning is cheap - it shares the underlying
/// pool - but once per query beats once per handler.
pub struct Store(Db);

impl Store {
    /// Opens the database at `path`, creating the file, its directory and its
    /// schema as needed.
    ///
    /// # Errors
    ///
    /// [`OpenError`]. Every variant names the file; [`OpenError::Schema`] also
    /// says what to do about it, because a stale schema is the one of the three
    /// a person can act on. A desktop application has nobody to page.
    pub async fn open(path: &Path) -> Result<Store, OpenError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| OpenError::Directory {
                path: parent.to_owned(),
                source,
            })?;
        }

        // `Sqlite::open` rather than `connect("sqlite:{path}")`: the URL form
        // parses what it is given and keeps `url.path()`, so a `#` or a `?` in
        // a directory name - legal on every platform this targets - silently
        // truncates the path and opens a different database.
        let db = Db::builder()
            .models(toasty::models!(crate::*))
            .build(Sqlite::open(path))
            .await
            .map_err(|source| OpenError::Connect {
                path: path.to_owned(),
                source,
            })?;

        ready(db, path).await
    }

    /// An empty database that lives only as long as the value returned.
    ///
    /// # Errors
    ///
    /// The same as [`Store::open`], less the cases that need a filesystem.
    #[cfg(test)]
    pub async fn in_memory() -> Result<Store, OpenError> {
        let db = Db::builder()
            .models(toasty::models!(crate::*))
            .build(Sqlite::in_memory())
            .await
            .map_err(|source| OpenError::Connect {
                path: PathBuf::from(":memory:"),
                source,
            })?;

        ready(db, Path::new(":memory:")).await
    }

    /// Every todo, oldest first.
    ///
    /// # Errors
    ///
    /// Whatever the database says.
    pub async fn all(&self) -> Result<Vec<Todo>, toasty::Error> {
        let mut db = self.0.clone();
        Todo::all()
            .order_by(Todo::fields().id().asc())
            .exec(&mut db)
            .await
    }

    /// Adds one.
    ///
    /// # Errors
    ///
    /// Whatever the database says.
    pub async fn add(&self, title: &Title) -> Result<(), toasty::Error> {
        let mut db = self.0.clone();
        let title = title.as_str();
        toasty::create!(Todo { title, done: false })
            .exec(&mut db)
            .await?;

        Ok(())
    }

    /// Inverts one todo's `done`.
    ///
    /// # Errors
    ///
    /// A [`toasty::Error`] whose `is_record_not_found` holds when `id` names no
    /// todo, which is what a second window that already deleted it produces.
    pub async fn toggle(&self, id: u64) -> Result<(), toasty::Error> {
        let mut db = self.0.clone();
        let mut todo = Todo::get_by_id(&mut db, id).await?;
        let done = !todo.done;
        toasty::update!(todo { done }).exec(&mut db).await?;

        Ok(())
    }

    /// Removes one. Removing a todo that is already gone succeeds.
    ///
    /// # Errors
    ///
    /// Whatever the database says.
    pub async fn remove(&self, id: u64) -> Result<(), toasty::Error> {
        let mut db = self.0.clone();
        Todo::delete_by_id(&mut db, id).await
    }
}

/// Shows the database's identity and not its contents.
///
/// [`Db`] has no `Debug` of its own, and the workspace warns on a type without
/// one.
impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Store")
            .field(&self.0.driver().url())
            .finish()
    }
}

/// The half [`Store::open`] and `in_memory` share.
///
/// It asks the database whether the schema is there, rather than running
/// `push_schema` unconditionally or checking for the file.
///
/// `push_schema` emits `CREATE TABLE` with no `IF NOT EXISTS`, so calling it at
/// every start - which is what upstream does, and gets away with because its
/// database is in memory - fails on the second launch of a persistent one.
///
/// `if !path.exists()` is a proxy for the question rather than the question.
/// It also has a window: the driver creates the file and `push_schema` fills
/// it, so a crash between the two leaves a database `exists` calls ready
/// forever. Asking the database instead has no such state.
async fn ready(db: Db, path: &Path) -> Result<Store, OpenError> {
    if let Err(missing) = probe(&db).await {
        // A file this build has never opened has no tables at all. Anything
        // else failing the probe is a file we must not write to, so the error
        // reported is the probe's: `push_schema` would only add "table already
        // exists", which is the consequence and not the cause.
        db.push_schema().await.map_err(|_| OpenError::Schema {
            path: path.to_owned(),
            source: missing,
        })?;
    }

    Ok(Store(db))
}

/// Reads one row, naming every column this build expects.
async fn probe(db: &Db) -> Result<(), toasty::Error> {
    let mut db = db.clone();
    Todo::all().limit(1).exec(&mut db).await.map(drop)
}

/// Why the database could not be opened.
///
/// Every message leads with its own sentence and leaves the cause to
/// [`source`](std::error::Error::source). toasty's `Display` walks a context
/// chain and repeats itself - a missing directory prints "unable to open
/// database file" four times - so pasting it mid-sentence buries the sentence.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OpenError {
    /// The directory that would hold the database could not be created.
    #[error("could not create {}, the directory the todo database lives in", .path.display())]
    Directory {
        /// The directory.
        path: PathBuf,
        /// What the filesystem said.
        source: std::io::Error,
    },

    /// The database could not be opened or created.
    #[error("could not open the todo database at {}", .path.display())]
    Connect {
        /// The file.
        path: PathBuf,
        /// What toasty said.
        source: toasty::Error,
    },

    /// The file is not a database this build of the application understands.
    #[error(
        "the todo database at {} was written by a different build of this application, and \
         this example wires up no migrations - delete that file to start over with an empty list",
        .path.display()
    )]
    Schema {
        /// The file.
        path: PathBuf,
        /// What the failed probe said.
        source: toasty::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_what_is_left_after_trimming() {
        assert_eq!(
            Title::parse("  buy milk  ").as_ref().map(Title::as_str),
            Some("buy milk")
        );
        assert_eq!(
            Title::parse("buy milk").as_ref().map(Title::as_str),
            Some("buy milk")
        );
    }

    /// Interior space is the title's, not the form's.
    #[test]
    fn a_title_keeps_the_space_inside_it() {
        assert_eq!(
            Title::parse(" buy  milk ").as_ref().map(Title::as_str),
            Some("buy  milk")
        );
    }

    #[test]
    fn a_blank_title_is_not_a_title() {
        assert_eq!(Title::parse(""), None);
        assert_eq!(Title::parse("   "), None);
        assert_eq!(Title::parse("\t\n"), None);
    }

    /// The claim the whole port rests on: it is still there next time.
    ///
    /// Also where the `push_schema` trap gets caught. A rewrite of [`ready`]
    /// that pushes unconditionally fails on the reopen below rather than on
    /// somebody's second launch.
    #[tokio::test]
    async fn a_todo_survives_reopening_the_file() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("nested").join("todos.db");

        let store = Store::open(&path).await.expect("a new database");
        store
            .add(&Title::parse("buy milk").expect("a title"))
            .await
            .expect("the todo is added");
        drop(store);

        let store = Store::open(&path).await.expect("the same database again");
        let todos = store.all().await.expect("the list");

        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].title, "buy milk");
    }

    /// A first launch that died between creating the file and filling it.
    #[tokio::test]
    async fn an_empty_file_is_not_mistaken_for_a_database() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("todos.db");
        std::fs::write(&path, []).expect("an empty file");

        let store = Store::open(&path)
            .await
            .expect("the schema is pushed anyway");

        assert!(store.all().await.expect("the list").is_empty());
    }

    #[tokio::test]
    async fn a_file_that_is_not_a_database_names_itself() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("todos.db");
        std::fs::write(&path, b"not a database").expect("a file");

        let error = Store::open(&path).await.expect_err("it is refused");

        assert!(matches!(error, OpenError::Schema { .. }), "{error:?}");
        assert!(error.to_string().contains("todos.db"), "{error}");
    }

    #[tokio::test]
    async fn toggling_inverts_one_todo() {
        let store = Store::in_memory().await.expect("a database");
        store
            .add(&Title::parse("buy milk").expect("a title"))
            .await
            .expect("the todo is added");
        let id = store.all().await.expect("the list")[0].id;

        store.toggle(id).await.expect("it toggles");
        assert!(store.all().await.expect("the list")[0].done);

        store.toggle(id).await.expect("it toggles back");
        assert!(!store.all().await.expect("the list")[0].done);
    }

    /// What the other window did while this one was drawing.
    #[tokio::test]
    async fn touching_a_todo_that_is_gone_says_so() {
        let store = Store::in_memory().await.expect("a database");

        let error = store.toggle(404).await.expect_err("there is no such todo");
        assert!(error.is_record_not_found(), "{error}");

        store.remove(404).await.expect("removing nothing succeeds");
    }
}
