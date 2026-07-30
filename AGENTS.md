## Agent skills

### Issue tracker

Issues and PRDs for this repo live in GitHub Issues. See `docs/agents/issue-tracker.md`.

### Triage labels

This repo uses the default mattpocock/skills triage labels. See `docs/agents/triage-labels.md`.

### Release notes

`CHANGELOG.md` at the root. A change a user would notice — behaviour, numbers on a
card, anything that will read as a regression without the reason beside it — gets a
note under `Unreleased` as it lands, not at tag time. `cargo dist` reads the section
matching a tagged version as that release's notes.

### Domain docs

This repo uses a single-context domain docs layout: root `CONTEXT.md` plus `docs/adr/`. See `docs/agents/domain.md`.
