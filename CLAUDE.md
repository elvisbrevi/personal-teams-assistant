# Claude Code

[AGENTS.md](AGENTS.md) is the guide for every agent in this repository: read the architecture first, keep its invariants and run its checks before delivering. This file only adds what is specific to Claude Code and does not repeat it.

- GUI work (`desktop/ui/`): follow [the GUI design conventions](docs/ui-design-conventions.md) and look at the result with the preview harness described there (`scripts/ui-preview/`, synthetic data only).
- Design skills installed for this project under `.claude/skills/` (`product-designer`, `information-architect`, `ux-heuristics`, `ui-ux-pro-max`) and the `frontend-design` plugin are advisory. Where they differ from AGENTS.md, the architecture or the conventions — landing-page patterns, external fonts, inline styles under the CSP, all-caps labels — the repository's documents win.
