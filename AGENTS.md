## Agent skills

### Issue tracker

Issues and PRDs for this repo live in GitHub Issues. See `docs/agents/issue-tracker.md`.

### Triage labels

This repo uses the default mattpocock/skills triage labels. See `docs/agents/triage-labels.md`.

### Release notes

`CHANGELOG.md` at the root. A change a user would notice — behaviour, numbers on a
card, anything that will read as a regression without the reason beside it — gets a
note under `Unreleased` as it lands, not at tag time. At tag time `Unreleased`
becomes `## <version>`, and the release workflow reads that section as the release's
notes — `cargo dist` does not, it only builds the artifacts. A tag whose version has
no section fails the release job rather than shipping notes without it.

### Domain docs

This repo uses a single-context domain docs layout: root `CONTEXT.md` plus `docs/adr/`. See `docs/agents/domain.md`.
