//! Work Items for one project: the `WORK.md` and GitHub Issues sources.
//!
//! A project has a single source of Work Items — `WORK.md` wins if it exists,
//! otherwise GitHub Issues (see CONTEXT.md). [`read_work_map`] resolves which one
//! applies for a directory and returns the [`WorkMap`] the board renders. The two
//! decoders — [`parse_work_md`] and [`parse_github_issues`] — are pure so they can
//! be unit-tested without a filesystem or the `gh` CLI.
//!
//! Work Links (which Agent Session is carrying an item) are *not* built here: they
//! join Work Items to live sessions on branch, which only the board — holding the
//! session store — can do. This module owns the plan; the board overlays the work.

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};
use tracing::warn;

/// Where a project's Work Items came from. Serialized to the UI, which turns it
/// into the source badge (`WORK.md` vs `GitHub Issues`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkSourceKind {
    /// A `WORK.md` checklist in the project root.
    WorkMd,
    /// GitHub Issues, read via the `gh` CLI.
    Github,
}

/// A Work Item's kanban column. `Doing` has no native `WORK.md`/GitHub checkbox,
/// so a `WORK.md` in-progress marker (`[~]`, `[-]`, `[/]`) or a GitHub
/// `in-progress`/`doing` label produces it; everything else is `Todo` (open) or
/// `Done` (checked / closed).
///
/// This is what the *source* said. A live Work Link can raise a `Todo` item to
/// `Doing` on the board — see `status_with_work_link` in `crates/board/src/http.rs`,
/// which owns that overlay because only the board holds the sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkStatus {
    Todo,
    Doing,
    Done,
}

/// One unit of project work, source-agnostic. `id` is the stable handle shown on
/// the card (`W-14` for a Work Map, `#42` for a GitHub Issue).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItem {
    pub id: String,
    pub title: String,
    pub status: WorkStatus,
    /// A short effort estimate like `~2d`, when the source records one.
    pub effort: Option<String>,
    /// Ids of items that must finish first — the blocked-by hint on To-do cards.
    pub blocked_by: Vec<String>,
}

/// A project's Work Items plus which source they came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkMap {
    pub source: WorkSourceKind,
    pub items: Vec<WorkItem>,
}

/// Resolve and read the Work Items for `project_dir`. `WORK.md` wins if present
/// (never both, per CONTEXT.md); otherwise GitHub Issues via `gh`. A missing
/// `WORK.md` and an unavailable `gh` (not a repo, not installed, not authed)
/// degrade to an empty GitHub map rather than an error, so the view still renders.
pub fn read_work_map(project_dir: &Path) -> WorkMap {
    let work_md = project_dir.join("WORK.md");
    match std::fs::read_to_string(&work_md) {
        Ok(contents) => WorkMap {
            source: WorkSourceKind::WorkMd,
            items: parse_work_md(&contents),
        },
        Err(_) => WorkMap {
            source: WorkSourceKind::Github,
            items: github_issues(project_dir).unwrap_or_default(),
        },
    }
}

/// Parse a `WORK.md` checklist into Work Items. Each item is one Markdown task
/// line; non-list lines (headings, prose, blanks) are ignored, so the file can
/// carry a title and notes around the list.
///
/// Line grammar (after the `- [x]` / `* [ ]` checkbox):
/// ```text
/// - [ ] W-14 Auto-update banner (~2d) (blocked by: W-12, W-13)
/// ```
/// The first token is the id; the rest is the title. `(~…)` gives the effort and
/// `(blocked by: …)` the dependencies — both are lifted out wherever they sit, so
/// ordinary parenthetical prose in a title (`(semantic colors)`) is left intact.
/// Checkbox marks: `x`/`X` → Done, `~`/`-`/`/` → Doing, blank → Todo.
pub fn parse_work_md(contents: &str) -> Vec<WorkItem> {
    contents.lines().filter_map(parse_work_line).collect()
}

fn parse_work_line(line: &str) -> Option<WorkItem> {
    let trimmed = line.trim();
    // A task line starts with a `- [` / `* [` bullet and a checkbox.
    let rest = trimmed
        .strip_prefix("- [")
        .or_else(|| trimmed.strip_prefix("* ["))?;
    let (mark, after) = rest.split_once(']')?;
    let status = match mark.trim() {
        "x" | "X" => WorkStatus::Done,
        "~" | "-" | "/" => WorkStatus::Doing,
        "" => WorkStatus::Todo,
        _ => WorkStatus::Todo,
    };

    // Lift the `(~effort)` and `(blocked by: …)` annotations out of the text.
    let (after, blocked_inner) = extract_paren(after, |low| low.starts_with("blocked by"));
    let (after, effort_inner) = extract_paren(&after, |low| low.starts_with('~'));

    let after = after.trim();
    let mut parts = after.splitn(2, char::is_whitespace);
    let id = parts.next().unwrap_or("").trim().to_string();
    if id.is_empty() {
        return None;
    }
    let title = parts.next().unwrap_or("").trim().to_string();

    Some(WorkItem {
        id,
        title,
        status,
        effort: effort_inner.map(|s| s.trim().to_string()),
        blocked_by: blocked_inner
            .as_deref()
            .map(parse_blocked_ids)
            .unwrap_or_default(),
    })
}

/// Find the first `(…)` whose lowercased, trimmed inner text satisfies `want`,
/// return `(text_without_that_paren, Some(inner))`. Nested parens are not handled
/// (Work Item metadata never nests); an unclosed `(` ends the search.
fn extract_paren(s: &str, want: impl Fn(&str) -> bool) -> (String, Option<String>) {
    let mut i = 0;
    while let Some(open_rel) = s[i..].find('(') {
        let open = i + open_rel;
        let Some(close_rel) = s[open + 1..].find(')') else {
            break;
        };
        let close = open + 1 + close_rel;
        let inner = &s[open + 1..close];
        if want(inner.to_ascii_lowercase().trim()) {
            let mut out = s[..open].trim_end().to_string();
            let tail = &s[close + 1..];
            if !out.is_empty() && !tail.trim().is_empty() {
                out.push(' ');
            }
            out.push_str(tail.trim_start());
            return (out.trim().to_string(), Some(inner.to_string()));
        }
        i = close + 1;
    }
    (s.to_string(), None)
}

/// Ids inside a `blocked by: W-12, W-13` annotation. Splits on commas/whitespace
/// after the `blocked by` prefix and drops the optional `:`.
fn parse_blocked_ids(inner: &str) -> Vec<String> {
    let low = inner.to_ascii_lowercase();
    let after = low.strip_prefix("blocked by").unwrap_or(&low);
    // Map the trimmed-prefix length back onto the original-case string.
    let start = inner.len() - after.len();
    inner[start..]
        .trim_start_matches([':', ' '])
        .split([',', ' '])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Read GitHub Issues for the repo at `dir` via `gh`. Returns `None` when `gh`
/// cannot run or the directory is not a GitHub repo — the caller degrades to an
/// empty map. Kept separate from [`parse_github_issues`] so the JSON decoding is
/// testable without the network.
fn github_issues(dir: &Path) -> Option<Vec<WorkItem>> {
    let output = Command::new("gh")
        .args([
            "issue",
            "list",
            "--state",
            "all",
            "--limit",
            "200",
            "--json",
            "number,title,state,labels,body",
        ])
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        warn!(dir = %dir.display(), "gh issue list failed; no GitHub Work Items");
        return None;
    }
    let json = String::from_utf8(output.stdout).ok()?;
    Some(parse_github_issues(&json))
}

/// Decode the JSON array from `gh issue list --json number,title,state,labels,body`
/// into Work Items. An `in-progress`/`doing` label maps to Doing, a closed issue to
/// Done, everything else to Todo. Dependencies are read from a `Blocked by: #12`
/// line in the body (the fallback convention in docs/agents/issue-tracker.md).
pub fn parse_github_issues(json: &str) -> Vec<WorkItem> {
    let issues: Vec<GhIssue> = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "could not parse gh issue list output");
            return Vec::new();
        }
    };
    issues
        .into_iter()
        .map(|issue| {
            let doing = issue.labels.iter().any(|l| {
                matches!(
                    l.name.to_ascii_lowercase().as_str(),
                    "in-progress" | "doing"
                )
            });
            let status = if doing {
                WorkStatus::Doing
            } else if issue.state.eq_ignore_ascii_case("closed") {
                WorkStatus::Done
            } else {
                WorkStatus::Todo
            };
            WorkItem {
                id: format!("#{}", issue.number),
                title: issue.title,
                status,
                effort: None,
                blocked_by: parse_blocked_ids_from_body(&issue.body),
            }
        })
        .collect()
}

/// `#12`-style ids from a `Blocked by: #12, #13` line anywhere in an issue body.
fn parse_blocked_ids_from_body(body: &str) -> Vec<String> {
    for line in body.lines() {
        let low = line.trim().to_ascii_lowercase();
        if let Some(rest) = low.strip_prefix("blocked by") {
            let start = line.len() - rest.len();
            return line[start..]
                .split(|c: char| !c.is_ascii_digit() && c != '#')
                .map(str::trim)
                .filter(|t| t.starts_with('#') && t.len() > 1)
                .map(str::to_string)
                .collect();
        }
    }
    Vec::new()
}

#[derive(Deserialize)]
struct GhIssue {
    number: u64,
    title: String,
    state: String,
    #[serde(default)]
    labels: Vec<GhLabel>,
    #[serde(default)]
    body: String,
}

#[derive(Deserialize)]
struct GhLabel {
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_checkbox_marks_into_statuses() {
        let md = "\
# Work Map

- [x] W-01 Done thing
- [~] W-02 In-progress thing
- [ ] W-03 Todo thing
";
        let items = parse_work_md(md);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].status, WorkStatus::Done);
        assert_eq!(items[1].status, WorkStatus::Doing);
        assert_eq!(items[2].status, WorkStatus::Todo);
        assert_eq!(items[2].id, "W-03");
        assert_eq!(items[2].title, "Todo thing");
    }

    #[test]
    fn lifts_effort_and_blocked_by_but_keeps_prose_parens() {
        let items = parse_work_md(
            "- [ ] W-08 Token refresh (semantic colors) (~2d) (blocked by: W-12, W-13)\n",
        );
        let it = &items[0];
        assert_eq!(it.id, "W-08");
        assert_eq!(it.title, "Token refresh (semantic colors)");
        assert_eq!(it.effort.as_deref(), Some("~2d"));
        assert_eq!(it.blocked_by, vec!["W-12", "W-13"]);
    }

    #[test]
    fn blocked_by_without_colon_and_single_id() {
        let items = parse_work_md("- [ ] W-14 Auto-update banner (blocked by W-12)\n");
        assert_eq!(items[0].blocked_by, vec!["W-12"]);
        assert_eq!(items[0].title, "Auto-update banner");
        assert_eq!(items[0].effort, None);
    }

    #[test]
    fn ignores_non_task_lines() {
        let items = parse_work_md("# Heading\n\nSome prose.\n- not a checkbox\n- [ ] W-01 Real\n");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "W-01");
    }

    #[test]
    fn item_id_only_no_title() {
        let items = parse_work_md("- [x] W-99\n");
        assert_eq!(items[0].id, "W-99");
        assert_eq!(items[0].title, "");
    }

    #[test]
    fn parses_github_issues_json() {
        let json = r#"[
            {"number":2,"title":"Walking skeleton","state":"CLOSED","labels":[],"body":""},
            {"number":5,"title":"Codex source","state":"OPEN","labels":[{"name":"in-progress"}],"body":"Blocked by: #2"},
            {"number":8,"title":"Relay","state":"OPEN","labels":[{"name":"enhancement"}],"body":"nothing here"}
        ]"#;
        let items = parse_github_issues(json);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].id, "#2");
        assert_eq!(items[0].status, WorkStatus::Done);
        assert_eq!(items[1].status, WorkStatus::Doing);
        assert_eq!(items[1].blocked_by, vec!["#2"]);
        assert_eq!(items[2].status, WorkStatus::Todo);
        assert!(items[2].blocked_by.is_empty());
    }

    #[test]
    fn malformed_github_json_is_empty_not_a_panic() {
        assert!(parse_github_issues("not json").is_empty());
    }
}
