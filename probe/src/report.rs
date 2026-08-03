//! What the probe asks the webview, and what it has learned so far.
//!
//! Every type here crosses into the page, so every type here derives
//! [`specta::Type`] and is exported to TypeScript. Nothing in `ui/` describes
//! this shape a second time.

use std::{
    fmt::Write as _,
    sync::{Mutex, MutexGuard, PoisonError},
};

use serde::{Deserialize, Serialize};
use specta::Type;

/// Declares every probe.
///
/// One entry per question: the variant, the id the page names it by, the label
/// a human reads, the answer we expect, and the answer that stands when nothing
/// ever reports it.
///
/// One table, so an id cannot drift between where an answer is recorded and
/// where it is printed, and the report cannot be missing a row: it is an array
/// indexed by this enum. The declared answer is the meaning of silence - a
/// form post that never arrives is a `no`, an inline script that never runs is
/// a `yes` - so nothing downstream has to infer what an empty row meant.
///
/// The expected answer is a prediction - what these webviews have been
/// measured doing. `README.md` says what to make of a row that disagrees.
///
/// The id is also the serde name, which is what `specta` projects into
/// TypeScript. The page does not get to spell a probe id: it receives the union
/// of these and the compiler holds it to answering every one.
macro_rules! probes {
    (
        $($variant:ident, $id:literal, $label:literal,
          $expected:ident, ($verdict:ident, $silence:expr);)*
    ) => {
        /// One question the probe asks the webview.
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, Type,
        )]
        pub enum Probe {
            $(
                #[serde(rename = $id)]
                $variant,
            )*
        }

        impl Probe {
            /// Every probe, in the order the report prints them.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            /// How many there are, so the report can be that long exactly.
            pub const COUNT: usize = Self::ALL.len();

            /// The string this probe is named by: its serde name, a member of
            /// the exported TypeScript union, and a path segment.
            pub fn id(self) -> &'static str {
                match self {
                    $(Self::$variant => $id,)*
                }
            }

            /// How the row reads to whoever is looking at the report.
            pub fn label(self) -> &'static str {
                match self {
                    $(Self::$variant => $label,)*
                }
            }

            /// What this probe is expected to answer on a webview we have
            /// already measured.
            pub fn expected(self) -> Verdict {
                match self {
                    $(Self::$variant => Verdict::$expected,)*
                }
            }

            /// The answer that stands if nothing ever reports this probe.
            fn silence(self) -> Answer {
                match self {
                    $(Self::$variant => Answer::new(Verdict::$verdict, $silence),)*
                }
            }
        }
    };
}

/// Most probes are answered by the page itself, so silence from it means the
/// answer never arrived, not that the answer was no.
const PAGE_SILENT: &str = "the page never reported this";

// The expectations below are what macOS 26 / WKWebView was measured doing, and
// the reasoning for each is in this crate's README. They are the only column
// here that can be wrong on purpose: a Windows or Linux run that disagrees has
// not failed, it has told us the thing the run was for.
probes! {
    PageOrigin, "page-origin", "the origin the page runs on", Info, (Unknown, PAGE_SILENT);
    UserAgent, "user-agent", "user agent", Info, (Unknown, PAGE_SILENT);

    GzipDecoded, "gzip-decoded", "Content-Encoding: gzip is decoded", No, (Unknown, PAGE_SILENT);
    EsModuleLoaded, "es-module-loaded", "an ES module loads over the scheme", Yes, (Unknown, PAGE_SILENT);
    ResourceTiming, "resource-timing", "Resource Timing records what was fetched", No, (Unknown, PAGE_SILENT);

    FetchPostBody, "fetch-post-body", "a fetch POST delivers its body", Yes, (Unknown, PAGE_SILENT);
    FetchPostHeaders, "fetch-post-headers", "headers on a fetch POST", Info, (Unknown, "no fetch POST arrived");
    FormPostBody, "form-post-body", "a form POST delivers its body", Yes, (No, "the form post never arrived");
    FormPostHeaders, "form-post-headers", "headers on a form POST", Info, (Unknown, "no form post arrived");

    SetCookieReturned, "set-cookie-returned", "Set-Cookie comes back on the next request", No, (Unknown, "nothing asked for the cookies back");
    DocumentCookie, "document-cookie", "document.cookie sees them", No, (Unknown, PAGE_SILENT);

    FetchFollows303, "fetch-follows-303", "fetch follows a 303", No, (Unknown, PAGE_SILENT);
    NavigationFollows303, "navigation-follows-303", "a navigation follows a 303", No, (No, "the redirect target was never requested");

    RangeHeaderArrives, "range-header-arrives", "a Range request header arrives", Yes, (Unknown, "nothing sent a Range header");

    CspDocumentRendered, "csp-document-rendered", "the CSP document rendered at all", Yes, (No, "the document never loaded");
    CspEnforced, "csp-enforced", "a Content-Security-Policy we send is enforced", Yes, (Yes, "the blocked inline script did not run");

    IpcInvoke, "ipc-invoke", "invoke reaches a Tauri command", Yes, (Unknown, PAGE_SILENT);
    IpcWindowIdentity, "ipc-window-identity", "...and the command knows which window called", Info, (Unknown, PAGE_SILENT);
    IpcInvokeUnderCsp, "ipc-invoke-under-csp", "...in a window we send a CSP with", Yes, (No, "the window never loaded");
    IpcInvokeInFrame, "ipc-invoke-in-frame", "...in a subframe of one", No, (No, "the frame never loaded");

    LocalStorage, "local-storage", "localStorage works", Yes, (Unknown, PAGE_SILENT);
    SessionStorage, "session-storage", "sessionStorage works", Yes, (Unknown, PAGE_SILENT);
    IndexedDb, "indexed-db", "IndexedDB works", Yes, (Unknown, PAGE_SILENT);

    ForeignDocumentRan, "foreign-document-ran", "a foreign document runs in a frame at all", Yes, (No, "the document never loaded");
    ForeignPostDelivered, "foreign-post-delivered", "a cross-origin POST reaches the server", Yes, (No, "the webview did not deliver it");
    ForeignPostOrigin, "foreign-post-origin", "...and names its origin when it does", No, (Unknown, "no cross-origin request arrived");
    ForeignPostReferer, "foreign-post-referer", "...and what scheme its Referer names", Info, (Unknown, "no cross-origin request arrived");
}

/// What a probe found.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub enum Verdict {
    /// The webview behaved the way a real HTTP client would.
    #[serde(rename = "yes")]
    Yes,
    /// It did not.
    #[serde(rename = "no")]
    No,
    /// Neither: a value worth writing down.
    #[serde(rename = "info")]
    Info,
    /// Nothing established an answer either way.
    #[serde(rename = "unknown")]
    Unknown,
}

impl Verdict {
    /// Every verdict, for measuring how wide the column has to be.
    const ALL: &'static [Self] = &[Self::Yes, Self::No, Self::Info, Self::Unknown];

    /// The word this verdict is written as, wherever it is written.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
            Self::Info => "info",
            Self::Unknown => "unknown",
        }
    }
}

/// A verdict and the evidence for it.
#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct Answer {
    /// What the probe found.
    verdict: Verdict,
    /// Why, in the words of whichever side found out. Empty when the verdict
    /// says everything.
    detail: String,
}

impl Answer {
    fn new(verdict: Verdict, detail: impl Into<String>) -> Self {
        Self {
            verdict,
            detail: detail.into(),
        }
    }

    pub fn yes(detail: impl Into<String>) -> Self {
        Self::new(Verdict::Yes, detail)
    }

    pub fn no(detail: impl Into<String>) -> Self {
        Self::new(Verdict::No, detail)
    }

    pub fn info(detail: impl Into<String>) -> Self {
        Self::new(Verdict::Info, detail)
    }
}

/// One line of the report, as the page receives it.
#[derive(Clone, Debug, Serialize, Type)]
pub struct Row {
    /// Which question this answers.
    id: Probe,
    /// How the row reads to whoever is looking at the report.
    label: String,
    /// What the probe found.
    verdict: Verdict,
    /// What it was expected to find. Equal to `verdict` on a webview this was
    /// written against, and the point of the row when it is not.
    expected: Verdict,
    /// Why.
    detail: String,
}

impl Row {
    /// Whether this row says anything the expectations did not already.
    fn news(&self) -> bool {
        self.verdict != self.expected
    }

    /// How the expectation prints: the word, in brackets when that is not what
    /// happened.
    ///
    /// Brackets and not a colour, because the same table goes to a terminal
    /// and to a window, and only one of those has colours to spend. The page
    /// keeps the brackets anyway - a reader who cannot pick the orange out
    /// still has to be able to find the row.
    fn expectation(&self) -> String {
        if self.news() {
            format!("[{}]", self.expected.as_str())
        } else {
            self.expected.as_str().to_owned()
        }
    }
}

/// The whole report, as the page receives it.
///
/// The counts ride along rather than being derived on arrival, so the window
/// and the terminal cannot come to different conclusions about how many of
/// anything there were.
#[derive(Clone, Debug, Serialize, Type)]
pub struct Sheet {
    /// Every probe, in declaration order.
    pub rows: Vec<Row>,
    /// How the verdicts fell out: `13 yes, 8 no, 6 info`.
    pub tally: String,
    /// How many rows did not do what was expected, empty when none did. The
    /// headline of a conformance run, and on a platform already measured it is
    /// meant to stay empty.
    pub news: String,
}

/// Everything the probe has learned, one answer per [`Probe`].
#[derive(Debug)]
pub struct Report {
    /// Indexed by `probe as usize`.
    ///
    /// An array and not a `Vec`, so every probe has exactly one row and
    /// nothing has to check.
    answers: Mutex<[Answer; Probe::COUNT]>,
}

impl Report {
    /// A report in which every probe holds its declared meaning of silence.
    pub fn new() -> Self {
        Self {
            answers: Mutex::new(std::array::from_fn(|index| Probe::ALL[index].silence())),
        }
    }

    /// Records what the server saw for itself.
    pub fn record(&self, probe: Probe, answer: Answer) {
        self.locked()[probe as usize] = answer;
    }

    /// The whole report, in declaration order.
    pub fn rows(&self) -> Vec<Row> {
        let answers = self.locked();
        Probe::ALL
            .iter()
            .zip(answers.iter())
            .map(|(probe, answer)| Row {
                id: *probe,
                label: probe.label().to_owned(),
                verdict: answer.verdict,
                expected: probe.expected(),
                detail: answer.detail.clone(),
            })
            .collect()
    }

    /// The whole report and what it adds up to.
    pub fn sheet(&self) -> Sheet {
        let rows = self.rows();

        let counted: Vec<String> = Verdict::ALL
            .iter()
            .filter_map(|verdict| {
                let count = rows.iter().filter(|row| row.verdict == *verdict).count();
                (count > 0).then(|| format!("{count} {}", verdict.as_str()))
            })
            .collect();

        let news = rows.iter().filter(|row| row.news()).count();
        Sheet {
            tally: counted.join(", "),
            news: if news == 0 {
                String::new()
            } else {
                format!("{news} unexpected")
            },
            rows,
        }
    }

    /// The report as a table, for the terminal the probe was started from.
    pub fn render(&self) -> String {
        let label_width = Probe::ALL
            .iter()
            .map(|probe| probe.label().len())
            .max()
            .unwrap_or(0);
        let verdict_width = Verdict::ALL
            .iter()
            .map(|verdict| verdict.as_str().len())
            .max()
            .unwrap_or(0);

        // Two wider than a verdict, for the brackets a disagreement wears.
        let expected_width = verdict_width + 2;

        let sheet = self.sheet();
        let rows = &sheet.rows;
        let mut table = String::new();
        for (label, verdict, expected, detail) in
            [("probe", "result", "expected".to_owned(), "detail")]
                .into_iter()
                .chain(rows.iter().map(|row| {
                    (
                        row.label.as_str(),
                        row.verdict.as_str(),
                        row.expectation(),
                        row.detail.as_str().trim_end(),
                    )
                }))
        {
            let line = format!(
                "{label:label_width$}  {verdict:verdict_width$}  \
                 {expected:expected_width$}  {detail}"
            );
            let _ = writeln!(table, "{}", line.trim_end());
        }

        // What it adds up to, which on a platform already measured should be
        // the tally and nothing after it.
        let _ = write!(table, "\n{}", sheet.tally);
        if !sheet.news.is_empty() {
            let _ = write!(table, ", {} - in [brackets] above", sheet.news);
        }
        let _ = writeln!(table);

        table
    }

    /// The answers, recovering from a poisoned lock rather than panicking:
    /// losing the whole report because one request panicked would defeat the
    /// point of running this.
    fn locked(&self) -> MutexGuard<'_, [Answer; Probe::COUNT]> {
        self.answers.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for Report {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The id is the serde name, and so also a member of the exported union. A
    /// duplicate would leave one of the two probes unreachable from the page,
    /// which is the one thing the shared table cannot rule out for itself.
    #[test]
    fn test_every_probe_has_its_own_id() {
        let mut ids: Vec<String> = Probe::ALL
            .iter()
            .map(|probe| serde_json::to_string(probe).expect("a probe serializes"))
            .collect();
        ids.sort_unstable();

        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "two probes share an id");
    }

    /// The terminal prints one of these through a `match` and the page
    /// switches on the other through serde, so they have to agree.
    #[test]
    fn test_a_verdict_is_spelled_the_same_way_by_both_routes() {
        for verdict in Verdict::ALL {
            assert_eq!(
                serde_json::to_string(verdict).expect("a verdict serializes"),
                format!("{:?}", verdict.as_str()),
                "{verdict:?} reaches its two readers under different names"
            );
        }
    }

    /// `unknown` means nothing established an answer, so a probe expecting it
    /// is a probe expecting not to run. The column would be measuring the
    /// harness instead of the webview, and would agree with itself for the one
    /// reason that proves nothing.
    #[test]
    fn test_no_probe_expects_to_go_unanswered() {
        for probe in Probe::ALL {
            assert_ne!(
                probe.expected(),
                Verdict::Unknown,
                "{probe:?} expects nothing to answer it"
            );
        }
    }

    /// A disagreement has to be findable without colour, which is all the
    /// terminal has and all a reader who cannot pick the orange out has.
    #[test]
    fn test_the_table_marks_what_was_not_expected_and_nothing_else() {
        let report = Report::new();

        // A fresh report is mostly silent as `unknown` against expectations
        // that are not, so both kinds of row are already here.
        for row in &report.rows() {
            assert_eq!(
                row.expectation().starts_with('['),
                row.news(),
                "{:?} is marked as though it were the other one",
                row.id
            );
        }

        // One probe through both states, read back off the rendered table -
        // the brackets are no use if they stop at the edge of the struct.
        report.record(Probe::GzipDecoded, Answer::no("the body arrived gzipped"));
        assert!(
            !line(&report, Probe::GzipDecoded).contains('['),
            "the answer we expected is flagged as though it were news"
        );

        report.record(Probe::GzipDecoded, Answer::yes(""));
        assert!(
            line(&report, Probe::GzipDecoded).contains("[no]"),
            "a webview that stopped behaving the way it was measured says so \
             nowhere in the table"
        );
    }

    /// The line the rendered table gives one probe.
    fn line(report: &Report, probe: Probe) -> String {
        report
            .render()
            .lines()
            .find(|line| line.starts_with(probe.label()))
            .expect("every probe has a line")
            .to_owned()
    }

    /// The report is indexed by `probe as usize`, so a row has to land on the
    /// probe it belongs to.
    #[test]
    fn test_an_answer_lands_on_its_own_row() {
        let report = Report::new();
        report.record(Probe::GzipDecoded, Answer::yes("decoded"));

        for row in &report.rows() {
            let expected = if row.id == Probe::GzipDecoded {
                Verdict::Yes
            } else {
                row.id.silence().verdict
            };
            assert_eq!(row.verdict, expected, "{:?} moved", row.id);
        }
    }
}
