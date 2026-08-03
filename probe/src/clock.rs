//! The clock in the page header, and the timings behind it.
//!
//! Every character is rendered here, formatting included; the page knows
//! nothing about how a duration is spelled. Both types cross into it, so both
//! are exported to TypeScript the way [`crate::report`]'s are.

use std::{
    sync::{Mutex, MutexGuard, PoisonError},
    time::Instant,
};

use http::Uri;
use serde::Serialize;
use specta::Type;

use crate::route::{Query, param};

/// What the clock in the page header reads, once an animation frame.
///
/// Public and named, because [`crate::serve`] has to recognise it: it is the
/// one route that is not logged, and sixty lines a second would bury the log
/// that is half the point of running this.
pub const CLOCK: &str = "/clock";

/// The server's clock, as the page reads it once an animation frame.
///
/// Strings and not numbers: the page paints these characters and formats
/// nothing, so a unit only ever changes here.
#[derive(Debug, Serialize, Type)]
pub struct Clock {
    /// `M:SS.mmm`, the part anyone reads.
    pub coarse: String,
    /// The three microsecond digits after it, which are only ever a blur.
    pub fine: String,
    /// The counters beside it: frames drawn, throughput, where.
    pub stats: Vec<Part>,
    /// A round trip taken apart. On the page rather than behind a hover
    /// because it is the only thing here that explains why a number the page
    /// reports disagrees with the devtools network panel.
    pub breakdown: Vec<Part>,
}

/// One labelled reading, in the gauges or in the breakdown.
#[derive(Debug, Serialize, Type)]
pub struct Part {
    /// What it is. Indented with spaces when it is a component of the line
    /// above, which is why the column it lands in is `white-space: pre`.
    pub label: String,
    /// The reading, already in whichever unit reads without a leading zero.
    pub value: String,
    /// Why it is not the same as the line above it. Empty when it is obvious.
    pub note: String,
}

/// What the clock remembers between frames.
#[derive(Debug, Default)]
pub struct Clockwork(Mutex<Tally>);

/// What the clock has been asked for, and what the page timed it at.
///
/// The count is the server's own, because the server did the serving. Both
/// timings have to be the page's, because only the end that sent a request
/// knows when it sent it - and they are kept apart because they disagree by a
/// factor of five and each is right about a different thing. See
/// [`Clockwork::tick`].
#[derive(Debug, Default)]
struct Tally {
    /// When the clock started, which is the first time anything asked it.
    ///
    /// Not when the process began: the header shows how long the transport has
    /// been answering, and a window that opens reading `0:04` because the
    /// linker was slow is showing the wrong span.
    started: Option<Instant>,
    /// How many frames the clock has been drawn for. Not every request: the
    /// burst that measures the transport is explicitly not one of these.
    frames: u64,
    /// What the page measured the transport doing flat out, once.
    burst: Option<Spell>,
    /// And what it costs from inside the loop that draws the clock, which is
    /// the same request and several times the number. Accumulated rather than
    /// sampled: `performance.now()` is clamped, so one reading of a
    /// sub-millisecond span is a quantisation artefact and a mean is not.
    paced: Spell,
}

/// Time the page spent on some number of round trips, split where the response
/// head arrives.
#[derive(Debug, Default)]
struct Spell {
    /// Microseconds until `fetch` resolved: request out, response head back.
    head: u64,
    /// Microseconds after that, reading the body and parsing it.
    body: u64,
    /// How many round trips the two cover.
    of: u64,
}

impl Spell {
    /// What one round trip cost, or `None` before any have been timed.
    fn each(&self) -> Option<(u64, u64)> {
        (self.of > 0).then(|| (self.head / self.of, self.body / self.of))
    }
}

impl Clockwork {
    /// Renders the header, once per animation frame.
    ///
    /// One vocabulary, however the timing was taken. `head` and `body` are
    /// microseconds the page spent, `of` says how many round trips they cover
    /// (one, when absent), and `measuring` says this request belongs to the
    /// burst and is not a frame.
    ///
    /// The page times the request before this one, because nothing can time
    /// itself. The paced and burst figures are kept apart because they
    /// disagree by a factor of five; `README.md` says why.
    pub fn tick(&self, uri: &Uri, query: &Query<'_>) -> Clock {
        let number = |name: &str| param(query, name).parse::<u64>().ok();
        let measuring = query.contains_key("measuring");

        let mut tally = self.locked();
        let elapsed = tally.started.get_or_insert_with(Instant::now).elapsed();
        if !measuring {
            tally.frames += 1;
        }

        if let (Some(head), Some(body)) = (number("head"), number("body")) {
            let of = number("of").unwrap_or(1).max(1);
            if measuring {
                tally.burst = Some(Spell { head, body, of });
            } else {
                tally.paced.head += head;
                tally.paced.body += body;
                tally.paced.of += of;
            }
        }

        Clock {
            coarse: format!(
                "{}:{:02}.{:03}",
                elapsed.as_secs() / 60,
                elapsed.as_secs() % 60,
                elapsed.subsec_millis()
            ),
            fine: format!("{:03}", elapsed.subsec_micros() % 1_000),
            stats: stats(&tally, uri),
            breakdown: breakdown(&tally),
        }
    }

    /// The tally, recovering from a poisoned lock rather than panicking: one
    /// panicked request should not take the header down with it.
    fn locked(&self) -> MutexGuard<'_, Tally> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The gauges beside the clock: what has been served, how fast, and over what.
fn stats(tally: &Tally, uri: &Uri) -> Vec<Part> {
    let mut stats = vec![part("frames", &count(tally.frames), "one a frame, forever")];

    if let Some(burst) = &tally.burst {
        let rate = burst
            .of
            .saturating_mul(1_000_000)
            .saturating_div((burst.head + burst.body).max(1));
        stats.push(part(
            "flat out",
            &format!("{}/s", count(rate)),
            "with nothing in its way",
        ));
    }

    stats.push(part("origin", &origin(uri), "as the server saw it"));
    stats
}

fn part(label: &str, value: &str, note: &str) -> Part {
    Part {
        label: label.to_owned(),
        value: value.to_owned(),
        note: note.to_owned(),
    }
}

/// A round trip taken apart: what it costs with nothing in its way, what it
/// costs paced by frames, and the fact that the gap between the two belongs to
/// the browser rather than to the transport.
fn breakdown(tally: &Tally) -> Vec<Part> {
    let mut parts = Vec::new();
    let timed = |label: &str, micros: u64, note: &str| part(label, &duration(micros), note);

    let flat = tally.burst.as_ref().and_then(Spell::each);
    if let (Some(burst), Some((head, body))) = (&tally.burst, flat) {
        parts.push(timed(
            "flat out",
            head + body,
            &format!("{} back-to-back, nothing else running", count(burst.of)),
        ));
        parts.push(timed(
            "  to the head",
            head,
            "request out, response head back",
        ));
        parts.push(timed("  body", body, "read off the response and parsed"));
    }

    if let Some((head, body)) = tally.paced.each() {
        parts.push(timed(
            "in a frame",
            head + body,
            "the same request, timed from the loop that draws the clock",
        ));
        if let Some((flat_head, flat_body)) = flat {
            parts.push(timed(
                "  the frame's share",
                (head + body).saturating_sub(flat_head + flat_body),
                "style, layout and paint, which finish before a response is delivered",
            ));
        }
    }

    parts
}

/// The origin the request arrived at, as the server saw it.
///
/// Read off the request rather than taken from [`crate::SCHEME`], because the
/// two disagree on the platforms that matter: wry rewrites custom schemes onto
/// `http://probe.localhost` for Windows and Android, and that rewrite is one of
/// the things worth seeing named on screen.
fn origin(uri: &Uri) -> String {
    match (uri.scheme(), uri.authority()) {
        (Some(scheme), Some(authority)) => format!("{scheme}://{authority}"),
        _ => "somewhere".to_owned(),
    }
}

/// A duration in the unit that does not spend its digits on a leading zero.
///
/// The transport lands right on the boundary - comfortably sub-millisecond on
/// the machine this was written on, and not by so much that it will be on every
/// machine - so neither unit is the right one to hard-code.
fn duration(micros: u64) -> String {
    if micros < 1_000 {
        format!("{micros}\u{b5}s")
    } else {
        format!("{:.2}ms", micros as f64 / 1_000.0)
    }
}

/// A count with thousands separators, which this one earns: it passes six
/// figures while you are still reading the table underneath it.
fn count(trips: u64) -> String {
    let digits = trips.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.char_indices() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}
