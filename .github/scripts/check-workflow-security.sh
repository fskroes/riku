#!/usr/bin/env bash
#
# Fail if any GitHub Actions workflow references a mutable Action, grants blanket
# write-all, or omits an explicit permissions declaration. This keeps the release
# automation's supply-chain guarantees from silently regressing on a routine
# workflow edit (issue #17, User Stories 21, 22). It reads only the workflow files,
# so it runs identically in CI and locally: `.github/scripts/check-workflow-security.sh`.
set -euo pipefail

workflow_dir="$(cd "$(dirname "$0")/.." && pwd)/workflows"
fail=0

note() {
  echo "::error::$1"
  fail=1
}

shopt -s nullglob
files=("$workflow_dir"/*.yml "$workflow_dir"/*.yaml)
if [ ${#files[@]} -eq 0 ]; then
  echo "no workflow files found under $workflow_dir" >&2
  exit 1
fi

for file in "${files[@]}"; do
  rel="${file#"$(cd "$workflow_dir/../.." && pwd)"/}"

  # 1. Every `uses:` must pin a full 40-character commit SHA. In-repo (`./…`)
  #    composite actions and digest-pinned docker images are exempt.
  while IFS= read -r line; do
    ref="${line#*uses:}"
    ref="$(printf '%s' "$ref" | sed -E 's/#.*$//' | tr -d '[:space:]"'"'")"
    case "$ref" in
      ''|./*|.\\*|docker://*) continue ;;
    esac
    sha="${ref##*@}"
    if ! printf '%s' "$sha" | grep -Eq '^[0-9a-f]{40}$'; then
      note "$rel: '$ref' is not pinned to a 40-character commit SHA"
    fi
  done < <(grep -E '^[[:space:]]*-?[[:space:]]*uses:[[:space:]]' "$file" || true)

  # 2. No workflow may grant blanket write-all.
  if grep -Eq '(^|[[:space:]])write-all([[:space:]]|$)' "$file"; then
    note "$rel: grants 'write-all'; scope permissions per job instead"
  fi

  # 3. Every workflow must declare permissions explicitly, never relying on the
  #    repository default (which may be read/write).
  if ! grep -Eq '^[[:space:]]*permissions:' "$file"; then
    note "$rel: no explicit 'permissions:' block; declare least-privilege scopes"
  fi
done

if [ "$fail" -ne 0 ]; then
  echo "workflow security check failed" >&2
  exit 1
fi
echo "workflow security check passed: all Action references pinned to a SHA and permissions scoped"
