# Handoff — ADR 0013 stop-hook spike: findings

> Archived verbatim into the repo per #54 (the original lived at
> `/tmp/handoff-adr-0013-spike-findings.md`, which is volatile). The packaged,
> production version of the hook lives in `hooks/claude-code/`; the copy below
> is a historical record of what the spike actually ran, kept unchanged.

Spike ran 2026-07-27 against real headless Claude Code sessions (`claude -p`,
CLI 2.1.220) in a scratch project with a `Stop` hook wired via project
`.claude/settings.json`. Spike code was throwaway (lived in
`$CLAUDE_JOB_DIR/tmp/spikeproj`, auto-cleaned); the hook script is embedded
below as the primary source. Spike journal files were deleted after validation.

## Verdicts on the four questions

**1. Can the hook reliably get the agent to emit one ADR-shaped record? YES.**
Pattern that works: the Stop hook checks the project JSONL for an
`who:"agent"` entry with the current `session_id`; if absent it returns
`{"decision":"block","reason":"<instruction + shape template>"}`. The agent
then appends the line itself (its own Write/Bash tools) and the next Stop pass
sees the entry and allows the stop. 3/3 completed sessions produced exactly one
valid one-line JSON record matching
`{v, project, session, at, who, handoff, done:[], next, resume:{instruction}}`.
Loop guard: if `stop_hook_active` is true and the entry still isn't there,
allow the stop and log to a `spike-missed.log` — this fired once (see
permissions finding) and never looped.

**2. Is done/next prose a report, not a reconstruction? YES.**
- Feature session: `done` listed the actual edits plus "Verified with python3
  temps.py 100" — evidence of specifics only the doer knows. `handoff:
  needs-review`, sensible `next`.
- Q&A session: `handoff: on-track`, `done` summarized the answer given,
  `resume.instruction: "Trivial Q&A only; nothing to pick up."` — no invented
  follow-up work. The instruction "if trivial Q&A, still write the entry with
  on-track / nothing pending" was followed exactly.
- Interrupted session (`kill -9` mid-run): **no entry written, file
  untouched** — matches the ADR's stale-next trade-off; nothing corrupts.

**3. JSONL append mechanics? YES.** Appends never clobbered (line count grew
1-per-session, earlier lines byte-identical), file created mode `0600`
(verified `-rw-------`), every line valid single-line JSON, slug stable across
sessions (spike slug: lowercased path with non-alnum → `-`).

**4. Does feeding the journal tail back produce a conversation? YES — and
more than expected.** A hand-written `who:"user"` correction ("temps.py is NOT
done — I also need Kelvin") was included in the hook's block reason (last 5
lines of the journal). The next session's agent, given only the vague prompt
"continue as you see fit", **did the Kelvin work** and wrote an entry whose
`done` opens with "Added Kelvin conversions … per the user journal note" —
explicitly answering the correction. Latest-wins resolution then reads
correctly: needs-review supersedes the user's needs-you.

## Surprises / constraints for `/to-spec`

1. **Permissions are the packaging problem.** The journal path
   (`~/.local/share/riku/journal/…`) is outside the project cwd, so default
   headless permissions (and `acceptEdits`) DENY the write — the first spike
   session failed exactly this way (agent reported it couldn't write; the
   missed-log fired). The packaged hook must ship with an `allow` rule for
   `Write`/`Bash` on that path, and docs must say so. Related: untrusted
   workspaces ignore project-level `permissions.allow` entirely ("workspace has
   not been trusted" warning) — yet writes still succeeded in that state under
   plain `claude -p` defaults, so behavior differs by mode; the spec should pin
   the recommended settings placement (user-level settings, not project-level)
   for the permission rule.
2. **The tail injection can trigger real work at stop time, not just an
   answering entry.** In the Q4 run the agent implemented the Kelvin request
   after the stop was blocked. Arguably a feature (correction gets acted on),
   but the spec should decide whether the hook instruction says "answer the
   correction in your entry" vs "act on it" — the spike's wording left it open
   and the agent chose to act.
3. **Agent chattiness is controllable.** "After appending the line, stop — do
   not summarize the entry to the user" worked; final visible output was just
   "Journal entry appended."
4. **`v`, `at` are agent-supplied** — the agent filled ISO-8601 UTC correctly
   every time, but nothing enforces it. The reader crate should treat `at` as
   untrusted/possibly absent.
5. **Hook must create the journal dir** (`makedirs`) — first session ever runs
   before Riku has created anything.

## The spike hook (primary source, verbatim)

```python
#!/usr/bin/env python3
"""PROTOTYPE — ADR 0013 stop-hook spike. Throwaway; delete after findings.

Stop hook: if this session has no journal entry yet, block the stop and hand
the agent an instruction to append one record (ADR 0013 shape) to the
per-project JSONL. Includes the journal tail so the agent can answer user
correction entries (the conversation model).
"""
import json, os, re, sys

data = json.load(sys.stdin)
session_id = data.get("session_id", "unknown")
cwd = data.get("cwd") or os.getcwd()
stop_hook_active = data.get("stop_hook_active", False)

# stable slug of the workspace path (spike version)
slug = re.sub(r"[^a-zA-Z0-9]+", "-", cwd.strip("/")).strip("-").lower()

journal_dir = os.path.expanduser("~/.local/share/riku/journal")
journal_path = os.path.join(journal_dir, f"{slug}.jsonl")

# Does this session already have an agent entry? Then allow the stop.
tail_lines = []
if os.path.exists(journal_path):
    with open(journal_path) as f:
        lines = [l for l in f.read().splitlines() if l.strip()]
    tail_lines = lines[-5:]
    for l in lines:
        try:
            rec = json.loads(l)
        except json.JSONDecodeError:
            continue
        if rec.get("session") == session_id and rec.get("who") == "agent":
            sys.exit(0)

if stop_hook_active:
    # Already blocked once and the agent still didn't write — give up, log it.
    with open(os.path.join(journal_dir, "spike-missed.log"), "a") as f:
        f.write(f"MISSED {session_id}\n")
    sys.exit(0)

os.makedirs(journal_dir, exist_ok=True)

tail_block = ""
if tail_lines:
    tail_block = (
        "\nRecent journal entries for this project (the journal is a conversation; "
        "if the latest entries include a who:\"user\" correction, read it and let your "
        "entry answer it):\n" + "\n".join(tail_lines) + "\n"
    )

reason = f"""Before you finish, append one journal entry recording this session's handoff.
{tail_block}
Append exactly ONE line of valid JSON (no pretty-printing, newline-terminated) to the file {journal_path} — APPEND, never overwrite. If the file does not exist, create it with mode 0600. Use this exact shape:

{{"v":1,"project":"{slug}","session":"{session_id}","at":"<current UTC time, ISO 8601>","who":"agent","handoff":"<needs-you|needs-review|on-track>","done":["<short sentence per completed piece of work this session>"],"next":"<the single next best step, one sentence>","resume":{{"instruction":"<what to tell a fresh session to pick this up>"}}}}

handoff is your parting assessment: needs-you (the user must decide/provide something), needs-review (work is done and awaits their review), on-track (nothing needed from them). Write done/next as a report of what YOU actually did and what genuinely remains — not a restatement of the task. If this was a trivial Q&A with nothing to hand off, still write the entry with handoff on-track, done describing the answer given, and next/instruction saying nothing is pending. After appending the line, stop — do not summarize the entry to the user."""

print(json.dumps({"decision": "block", "reason": reason}))
sys.exit(0)
```

Settings wiring used (project `.claude/settings.json`):

```json
{"hooks": {"Stop": [{"hooks": [{"type": "command",
  "command": "python3 \"$CLAUDE_PROJECT_DIR/.claude/hooks/journal_stop_hook.py\""}]}]}}
```

## Next

Per the shaping handoff (`/tmp/handoff-adr-0013-stop-hook-spike.md`): fresh
thread, **`/to-spec` → `/to-tickets` → `/implement`**, with both handoff files
referenced in one unbroken context window. The spec must resolve surprise #1
(permission packaging) and #2 (answer vs act on corrections).
