# AGENTS.md

## Committing

When finished working, always commit your changes — split into separate,
self-contained batches (one logical change per commit) rather than one large
commit.

## Versioning

Before the final commit, bump `version` in `Cargo.toml` (patch for fixes, minor
for features) and sync `Cargo.lock` with `cargo update -w --offline`. Commit
the bump with the change it belongs to, or as a `chore(release): X.Y.Z` commit
when a batch has several changes.

## Tags

A tag belongs to its output until the output is removed. Emptying a tag (by
`send_to_tag` or by switching away) must never unassign it: the output then
has no tag, and `normalize_outputs` gives it the lowest free tag index, so new
windows land on a tag the user never opened. This regressed once (d640386);
the tests `an_emptied_tag_keeps_its_output` and
`empty_tag_loses_its_output_but_comes_back_there` in `src/state.rs` guard it.

## Pre-commit hook

`.githooks/pre-commit` runs `cargo fmt` and `cargo test` and blocks the
commit on either failing. Enable it once per clone with
`git config core.hooksPath .githooks`.
