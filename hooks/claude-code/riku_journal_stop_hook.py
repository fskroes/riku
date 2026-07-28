#!/usr/bin/env python3
"""Riku agent journal — Claude Code Stop hook (ADR 0013).

At session stop, if this session has not yet written its journal entry, block
the stop once and hand the agent an instruction to append one record (ADR 0013
shape) to the per-project JSONL. The instruction embeds the journal tail so the
agent reads and answers user correction entries — the journal is a
conversation, not an agent monologue.

Guarantees:
- One entry per session: the stop is allowed as soon as an agent entry for the
  current session id exists.
- Never traps the agent: if the stop was already blocked once
  (stop_hook_active) and the entry still isn't there, the stop is allowed and
  the miss is logged locally to journal-missed.log.
- The journal file is pre-created with mode 0600; appends never clobber.

Install: see hooks/claude-code/README.md. The journal path is outside the
project cwd, so the permission allow rule MUST live in user-level settings
(~/.claude/settings.json) — untrusted workspaces ignore project-level allow
rules.
"""
import datetime
import json
import os
import re
import sys

data = json.load(sys.stdin)
session_id = data.get("session_id", "unknown")
cwd = data.get("cwd") or os.getcwd()
stop_hook_active = data.get("stop_hook_active", False)

# Stable slug of the workspace path: lowercased, non-alphanumerics collapsed
# to "-". Riku's journal reader derives the same slug; keep them in sync.
slug = re.sub(r"[^a-zA-Z0-9]+", "-", cwd.strip("/")).strip("-").lower()

data_home = os.environ.get("XDG_DATA_HOME") or os.path.expanduser("~/.local/share")
journal_dir = os.path.join(data_home, "riku", "journal")
journal_path = os.path.join(journal_dir, f"{slug}.jsonl")

# An agent entry for this session already exists -> allow the stop.
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

os.makedirs(journal_dir, exist_ok=True)

if stop_hook_active:
    # Already blocked once and the agent still didn't write — give up, log it.
    now = datetime.datetime.now(datetime.timezone.utc).isoformat()
    with open(os.path.join(journal_dir, "journal-missed.log"), "a") as f:
        f.write(f"{now} MISSED session={session_id} project={slug}\n")
    sys.exit(0)

# Keep the journal bounded before inviting another append. Riku caps the live
# file and keeps exactly one rotated generation (JOURNAL_SIZE_CAP in
# crates/sessions/src/journal_store.rs); the cap has to hold here too, because
# the agent appends far more often than Riku does. os.replace is atomic, so a
# reader sees the old file or the new one, never a half-copied journal.
JOURNAL_SIZE_CAP = 1 << 20
if os.path.exists(journal_path) and os.path.getsize(journal_path) >= JOURNAL_SIZE_CAP:
    os.replace(journal_path, journal_path + ".1")
# The tail read above is deliberately kept across a rotation: a user correction
# in those lines still deserves an answer, and the conversation does not restart
# just because the file did.

# Pre-create the journal file with mode 0600 so permissions never depend on
# how the agent creates it; the append below is the agent's job.
fd = os.open(journal_path, os.O_CREAT | os.O_APPEND | os.O_WRONLY, 0o600)
os.close(fd)

tail_block = ""
if tail_lines:
    tail_block = (
        "\nRecent journal entries for this project (the journal is a conversation; "
        'if the latest entries include a who:"user" correction, read it and let your '
        "entry answer it — acknowledge it in done/next as appropriate, but do NOT "
        "begin new work now: the session is stopping):\n"
        + "\n".join(tail_lines)
        + "\n"
    )

reason = f"""Before you finish, append one journal entry recording this session's handoff.
{tail_block}
Append exactly ONE line of valid JSON (no pretty-printing, newline-terminated) to the file {journal_path} — APPEND, never overwrite. The file already exists with the right permissions. Use this exact shape:

{{"v":1,"project":"{slug}","session":"{session_id}","at":"<current UTC time, ISO 8601>","who":"agent","handoff":"<needs-you|needs-review|on-track>","done":["<short sentence per completed piece of work this session>"],"next":"<the single next best step, one sentence>","resume":{{"instruction":"<what to tell a fresh session to pick this up>"}}}}

handoff is your parting assessment: needs-you (the user must decide/provide something), needs-review (work is done and awaits their review), on-track (nothing needed from them). Write done/next as a report of what YOU actually did and what genuinely remains — not a restatement of the task. If a user correction appears in the journal tail above, answer it in your entry; do not start new work at stop time. If this was a trivial Q&A with nothing to hand off, still write the entry with handoff on-track, done describing the answer given, and next/instruction saying nothing is pending. After appending the line, stop — do not summarize the entry to the user."""

print(json.dumps({"decision": "block", "reason": reason}))
sys.exit(0)
