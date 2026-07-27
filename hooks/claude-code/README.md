# Riku agent journal — Claude Code Stop hook

Wires a Claude Code session to append one journal entry to Riku's per-project
journal when the session stops (ADR 0013,
`docs/adr/0013-agent-written-project-journal.md`). The mechanics were validated
against real sessions; findings and the original spike source live in
`docs/spikes/adr-0013-stop-hook-spike-findings.md`.

## What it does

At `Stop`, the hook checks
`$XDG_DATA_HOME/riku/journal/<project>.jsonl` (fallback
`~/.local/share/riku/journal/`) for an agent entry with the current session id:

- **No entry yet** → blocks the stop once, handing the agent the record
  template, the journal path, and the last ~5 journal lines. If those lines
  include a `who:"user"` correction, the agent is told to answer it *in its
  entry* — and explicitly not to start new work at stop time.
- **Entry exists** → allows the stop. Exactly one entry per session.
- **Blocked once already and still no entry** (`stop_hook_active`) → allows the
  stop and appends a line to `journal-missed.log` in the journal directory, so
  a misbehaving agent can never be trapped in a stop loop.

The journal file is pre-created by the hook with mode `0600`; the agent only
ever appends. `<project>` is a slug of the workspace path: lowercased,
non-alphanumerics collapsed to `-` (e.g. `/Users/you/dev/riku` →
`users-you-dev-riku`). Trivial Q&A sessions still get an entry: `on-track`,
with next/instruction saying nothing is pending.

## Install

Requires Python 3 on `PATH` and Claude Code with hooks support.

**1. Copy the hook script somewhere stable**, e.g.:

```sh
mkdir -p ~/.claude/hooks
cp hooks/claude-code/riku_journal_stop_hook.py ~/.claude/hooks/
```

**2. Wire the Stop hook** in `~/.claude/settings.json` (user-level enables it
for every project; use a project's `.claude/settings.json` instead to scope it):

```json
{
  "hooks": {
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "python3 \"$HOME/.claude/hooks/riku_journal_stop_hook.py\""
          }
        ]
      }
    ]
  }
}
```

**3. Add the permission rule — user-level settings, required.** The journal
path is *outside the project cwd*, so default permission modes deny the
agent's append. The allow rule must go in **`~/.claude/settings.json`**
(user-level): untrusted workspaces ignore project-level `permissions.allow`
rules entirely, so a project-level rule silently fails there.

```json
{
  "permissions": {
    "allow": [
      "Edit(~/.local/share/riku/journal/**)",
      "Bash(echo:*)",
      "Bash(printf:*)"
    ]
  }
}
```

If you set `XDG_DATA_HOME`, point the `Edit` rule at
`$XDG_DATA_HOME/riku/journal/**` instead.

The spike found agents append either through file tools or through a shell
`>> file` redirect, so both need allowing (`Bash` rules match on the command,
not the path — hence the broader `echo`/`printf` rules; narrow or drop them if
your agent reliably uses file tools). The `Write` tool is deliberately *not*
in the list: Write replaces file contents, and the journal must only ever be
appended to. The hook pre-creates the file, so the agent never needs
whole-file writes.

**4. Verify.** Run any session to completion (even a one-question session),
then:

```sh
tail -n 1 ~/.local/share/riku/journal/<project-slug>.jsonl
```

You should see one line of JSON with `"who":"agent"` and your session id. If
instead `journal-missed.log` gained a line, the append was denied — recheck
step 3 (rule present, and at user level).

## Record shape (v1)

```
{v:1, project, session, at, who:"agent",
 handoff:"needs-you"|"needs-review"|"on-track",
 done:[string], next:string, resume:{instruction:string}}
```

`handoff` is the agent's parting Handoff Status (needs-you / needs-review /
on-track — deliberately distinct from Riku's live Attention vocabulary).
`resume` carries only an instruction; Riku builds the runnable resume command
itself from `session`.

## Validation

The end-to-end behavior (real headless sessions producing exactly one valid
record each, trivial Q&A yielding on-track, corrections answered) was
validated by the spike — see the findings doc. The packaged script diverges
from the spike copy (XDG_DATA_HOME support, hook-side 0600 pre-creation,
timestamped miss log, the answer-don't-act tail wording); those branches are
covered by a runnable check:

```sh
sh hooks/claude-code/test_journal_stop_hook.sh
```

It exercises, against an isolated `XDG_DATA_HOME`: first-stop block with the
full template, allow once an agent entry exists, the `stop_hook_active` miss
log, 0600 pre-creation, tail injection with the correction wording, and that
appends never clobber earlier lines.
