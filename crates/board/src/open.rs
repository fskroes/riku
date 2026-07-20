//! The OS half of the deep link: turning a [`DeepLink`] into an open terminal.
//!
//! [`collector::DeepLink`] says *what* resumes a session and *where*; this module
//! is *how* on this machine. The board is local (binds `127.0.0.1`), so "open"
//! genuinely runs on the human's own box — it launches a new macOS Terminal
//! window that `cd`s into the session's workspace and runs the resume command, so
//! the human lands back inside the exact conversation to answer or review it.
//!
//! Two seams keep this honest and testable: the launch is behind the [`Launcher`]
//! trait (tests inject a recorder instead of spawning Terminal), and the shell /
//! AppleScript strings are built by pure functions with their own unit tests.

use std::process::Command;

use collector::DeepLink;

/// Opens a resolved [`DeepLink`] on this machine. Behind a trait so the HTTP layer
/// depends on the capability, not the macOS mechanism, and tests can substitute a
/// recorder.
pub trait Launcher: Send + Sync {
    /// Bring the human back into the session. `Err(message)` is surfaced to the
    /// board UI verbatim, so it should read as a human-facing reason.
    fn open(&self, link: &DeepLink) -> Result<(), String>;
}

/// Whether a session id is safe to place into a resume command. Real ids are
/// uuids / Codex rollout ids (`[A-Za-z0-9_-]`); anything else is rejected before
/// it reaches a shell or AppleScript string — belt-and-suspenders on top of the
/// quoting below, and a clear 4xx rather than a mangled launch.
pub fn is_safe_session_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The default launcher: a new macOS Terminal window running the resume command.
pub struct TerminalLauncher;

impl Launcher for TerminalLauncher {
    fn open(&self, link: &DeepLink) -> Result<(), String> {
        let script = terminal_applescript(link);
        let output = Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
            .map_err(|e| format!("could not launch Terminal (osascript: {e})"))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        Err(if detail.is_empty() {
            "Terminal refused to open the session".to_string()
        } else {
            format!("Terminal could not open the session: {detail}")
        })
    }
}

/// The shell command run inside the new terminal: `cd <dir> && <program> <args…>`.
/// Every path/argument is single-quoted, so spaces or shell metacharacters in a
/// workspace path or session id are inert.
fn terminal_command(link: &DeepLink) -> String {
    let mut cmd = format!("cd {}", shell_single_quote(&link.dir.to_string_lossy()));
    cmd.push_str(" && ");
    cmd.push_str(link.program);
    for arg in &link.args {
        cmd.push(' ');
        cmd.push_str(&shell_single_quote(arg));
    }
    cmd
}

/// The full AppleScript handed to `osascript`: activate Terminal and run the
/// command in a fresh window. Passed as one `-e` argument (no shell in between),
/// so only AppleScript-string escaping is needed here — the inner command is
/// already shell-quoted by [`terminal_command`].
fn terminal_applescript(link: &DeepLink) -> String {
    let cmd = applescript_string(&terminal_command(link));
    format!("tell application \"Terminal\"\nactivate\ndo script \"{cmd}\"\nend tell")
}

/// Single-quote a string for POSIX `sh`: wrap in `'…'`, closing/reopening around
/// any embedded single quote (`'` → `'\''`).
fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Escape a string for embedding inside an AppleScript double-quoted literal:
/// backslash first, then the double quote.
fn applescript_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use collector::Tool;
    use std::path::Path;

    fn link(dir: &str) -> DeepLink {
        DeepLink::resume(Tool::Claude, "sess-1", Some(dir), Path::new("/t.jsonl")).unwrap()
    }

    #[test]
    fn command_cds_then_resumes() {
        // Every argument is single-quoted (harmless for a literal flag, essential
        // for the untrusted-looking ones).
        let cmd = terminal_command(&link("/Users/x/repos/foo"));
        assert_eq!(cmd, "cd '/Users/x/repos/foo' && claude '--resume' 'sess-1'");
    }

    #[test]
    fn a_workspace_path_with_spaces_and_quotes_is_inert() {
        // A single quote in the path cannot break out of the quoting.
        let cmd = terminal_command(&link("/Users/x/My Repos/it's mine"));
        assert_eq!(cmd, "cd '/Users/x/My Repos/it'\\''s mine' && claude '--resume' 'sess-1'");
    }

    #[test]
    fn applescript_escapes_backslash_and_quote() {
        assert_eq!(applescript_string(r#"a "b" \c"#), r#"a \"b\" \\c"#);
        // The full script embeds the shell command inside a double-quoted literal.
        let script = terminal_applescript(&link("/tmp/foo"));
        assert!(script.contains(r#"do script "cd '/tmp/foo' && claude '--resume' 'sess-1'""#));
        assert!(script.starts_with("tell application \"Terminal\""));
    }

    #[test]
    fn rejects_unsafe_session_ids() {
        assert!(is_safe_session_id("2f1c9a3e-uuid_1"));
        assert!(!is_safe_session_id(""));
        assert!(!is_safe_session_id("a'b"));
        assert!(!is_safe_session_id("a b"));
        assert!(!is_safe_session_id("$(rm -rf)"));
    }
}
