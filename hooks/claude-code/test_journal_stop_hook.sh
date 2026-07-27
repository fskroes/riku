#!/bin/sh
# Exercises every branch of riku_journal_stop_hook.py against an isolated
# XDG_DATA_HOME. Run from anywhere: sh hooks/claude-code/test_journal_stop_hook.sh
set -eu

HOOK="$(cd "$(dirname "$0")" && pwd)/riku_journal_stop_hook.py"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
export XDG_DATA_HOME="$TMP/xdg"

SLUG=users-you-dev-proj
JP="$XDG_DATA_HOME/riku/journal/$SLUG.jsonl"
fail() { echo "FAIL: $1" >&2; exit 1; }

# 1. First stop, no entry: block with template; journal file pre-created 0600.
echo '{"session_id":"s1","cwd":"/Users/you/dev/proj","stop_hook_active":false}' \
  | python3 "$HOOK" > "$TMP/out1"
python3 - "$TMP/out1" "$SLUG" <<'EOF'
import json, sys
d = json.load(open(sys.argv[1]))
assert d["decision"] == "block", "expected block"
r = d["reason"]
assert '"v":1' in r and '"session":"s1"' in r and sys.argv[2] in r, "template incomplete"
assert "do not start new work at stop time" in r, "no-new-work wording missing"
EOF
[ -f "$JP" ] || fail "journal file not pre-created"
perms=$(ls -l "$JP" | cut -c2-10)
[ "$perms" = "rw-------" ] || fail "journal file not 0600 (got $perms)"

# 2. Agent entry exists for the session: allow (no output).
printf '%s\n' '{"v":1,"project":"'"$SLUG"'","session":"s1","at":"2026-07-27T10:00:00Z","who":"agent","handoff":"on-track","done":["x"],"next":"y","resume":{"instruction":"z"}}' >> "$JP"
out=$(echo '{"session_id":"s1","cwd":"/Users/you/dev/proj","stop_hook_active":false}' | python3 "$HOOK")
[ -z "$out" ] || fail "expected allow (empty output) when entry exists"

# 3. stop_hook_active with no entry: allow + miss logged.
out=$(echo '{"session_id":"s2","cwd":"/Users/you/dev/proj","stop_hook_active":true}' | python3 "$HOOK")
[ -z "$out" ] || fail "expected allow (empty output) on stop_hook_active"
grep -q "MISSED session=s2" "$XDG_DATA_HOME/riku/journal/journal-missed.log" \
  || fail "miss not logged"

# 4. User correction in tail: block reason carries the tail + answer-don't-act wording.
printf '%s\n' '{"v":1,"project":"'"$SLUG"'","session":"s1","at":"2026-07-27T11:00:00Z","who":"user","handoff":"needs-you","done":[],"next":"also need Kelvin","resume":{"instruction":""}}' >> "$JP"
echo '{"session_id":"s3","cwd":"/Users/you/dev/proj","stop_hook_active":false}' \
  | python3 "$HOOK" > "$TMP/out4"
python3 - "$TMP/out4" <<'EOF'
import json, sys
r = json.load(open(sys.argv[1]))["reason"]
assert "also need Kelvin" in r, "journal tail missing from reason"
assert "do NOT begin new work now" in r, "tail correction wording missing"
EOF

# 5. Nothing clobbered: both earlier lines intact.
[ "$(grep -c '"session":"s1"' "$JP")" = "2" ] || fail "earlier lines lost"

# 6. Past the 1 MiB cap: the journal rotates to one generation before the next
# append is invited, and the tail still carries the conversation across it.
python3 - "$JP" <<'EOF'
import sys
line = '{"v":1,"project":"users-you-dev-proj","session":"old","at":"2026-07-27T09:00:00Z","who":"agent","handoff":"on-track","done":["bulk"],"next":"x","resume":{"instruction":"y"}}\n'
with open(sys.argv[1], "a") as f:
    while f.tell() < 1 << 20:
        f.write(line)
EOF
echo '{"session_id":"s4","cwd":"/Users/you/dev/proj","stop_hook_active":false}' \
  | python3 "$HOOK" > "$TMP/out6"
[ -f "$JP.1" ] || fail "journal not rotated past the cap"
[ "$(wc -c < "$JP")" -eq 0 ] || fail "live journal not started fresh after rotation"
perms=$(ls -l "$JP" | cut -c2-10)
[ "$perms" = "rw-------" ] || fail "rotated-into journal not 0600 (got $perms)"
grep -q '\\"session\\":\\"old\\"' "$TMP/out6" \
  || fail "tail lost across the rotation: the conversation must not restart with the file"

# A second rotation replaces the generation rather than growing a third file.
python3 - "$JP" <<'EOF'
import sys
with open(sys.argv[1], "w") as f:
    f.write("x" * (1 << 20))
EOF
echo '{"session_id":"s5","cwd":"/Users/you/dev/proj","stop_hook_active":false}' \
  | python3 "$HOOK" > /dev/null
[ "$(ls "$XDG_DATA_HOME/riku/journal" | grep -c "^$SLUG")" = "2" ] \
  || fail "expected exactly one live journal and one rotated generation"

echo "OK: all journal stop hook branches pass"
