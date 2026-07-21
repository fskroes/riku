# README patterns in established developer tools

Research date: 2026-07-21. Sources below are the projects' own GitHub README
files; this is a comparison of useful conventions, not a template to copy.

## What comparable projects do

| Project | Opening sequence | Where the detail goes | Relevant lesson |
| --- | --- | --- | --- |
| [GitHub CLI](https://github.com/cli/cli/blob/trunk/README.md) | Name, one-sentence purpose, screenshot, supported environments | Links to its manual and contribution guide; installation is grouped by OS | A reader can identify the tool, see it, and choose a platform-specific install route before encountering implementation detail. |
| [Vite](https://github.com/vitejs/vite/blob/main/README.md) | Logo/badges, name/tagline, six concrete capabilities, short explanation | Links to the full documentation and keeps contribution/license at the end | The root README is deliberately a concise product entry point rather than the entire manual. |
| [ripgrep](https://github.com/BurntSushi/ripgrep/blob/master/README.md) | One-paragraph definition, platform availability, badges/license | A quick-links block precedes examples, rationale, installation, build, and test details | It supports both evaluators and builders: links and an example first; deeper operational detail follows. |
| [bat](https://github.com/sharkdp/bat/blob/master/README.md) | Visual identity, a one-line “cat clone” definition, navigation links, then feature screenshots | Examples are grouped under “How to use”; integration and customization are separate | Screenshots are evidence of the benefit, and runnable examples are organized by user task instead of internal architecture. |

## Patterns worth adopting

1. **Lead with the user outcome and a visual.** All four make the purpose
   understandable immediately; GitHub CLI and bat put a screenshot near the
   definition, while Vite follows its tagline with concrete capabilities. Riku
   already has a strong description and board screenshot, so retain both but make
   the first screen more scannable: one sentence, one short “what it watches /
   what it helps you do” line, then the screenshot.

2. **Put the supported install path before source/development instructions.**
   GitHub CLI separates end-user installation from source building, and ripgrep
   names the binary and gives a copyable package-manager command before its build
   section. For Riku, the Apple-Silicon Homebrew command should be the first
   getting-started path, followed by the source build as an explicitly developer
   fallback. Keep the Intel limitation next to that command.

3. **Give a fast, verifiable first use.** The CLI projects pair an install route
   with a recognizable command; bat additionally supplies short, task-shaped
   examples. Riku’s quick start should end with `riku` and the local URL, ideally
   with a plain statement of what the reader should see (their local sessions,
   arranged by attention). This is more useful than explaining its architecture
   before the reader has opened it.

4. **Use the README as a map, not the canonical manual.** Vite links to full
   docs rather than duplicating them. GitHub CLI links to its manual/contribution
   guide. Riku should keep high-level operating concepts in the README, but move
   exhaustive flag precedence, wire/API details, transcript heuristics, and
   deployment configuration behind clear links to focused docs/ADR pages. This
   makes the first read shorter without hiding implementation truth.

5. **Organize examples around reader intent.** bat’s “How to use” and integration
   sections show work people actually want to do; ripgrep’s quick links enable
   jumping directly to a need. For Riku, a compact “Common setups” section would
   work better than a long continuous narrative: **solo local board**,
   **source/development**, and **team/multi-machine** (linking to the relay
   deployment guide).

6. **Keep technical trust signals, but locate them after the core pitch.**
   ripgrep includes its build/test path; GitHub CLI includes provenance details;
   Vite ends with contribution and license. Riku should similarly expose source
   build, tests, contributing, license, and security/reporting contact after the
   end-user setup. These are important to GitHub visitors, but should not compete
   with the first-use flow.

## Recommended Riku outline

```text
Name + one-sentence outcome
Screenshot
Three capabilities / constraints (local-first, read-only, Claude Code + Codex)
Install on macOS (Apple Silicon) → brew commands → `riku` → localhost URL
What you will see / first-use confirmation
Common setups
  - Solo (default)
  - Team / multi-machine (link to relay deployment)
How it works (brief; link to architecture/ADRs)
Build from source and develop
Contributing, license, security
```

## Deliberate non-recommendations

- Do not add badges merely because the examples have them. Add only badges that
  answer a real adoption question for Riku (release/install availability, license,
  CI) and keep them maintained.
- Do not claim cross-platform support until artifacts and the support path exist.
  GitHub CLI states supported platforms directly; Riku should continue to state
  the current Apple-Silicon boundary beside installation.
- Do not hide the local-only/one-way-control boundary. It is a product promise,
  not an implementation footnote, and belongs in the opening capabilities.
