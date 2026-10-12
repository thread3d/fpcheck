# fpcheck

A Rust-style feedback layer for Free Pascal: one command that returns precise,
structured, actionable diagnostics for an edit → check → fix loop, aimed
primarily at LLM coding agents.

**Status:** specification v0.1 — no implementation yet.

The full specification lives in [`SPEC.md`](SPEC.md): goal and non-goals,
architecture, CLI, FPC runner and strict profiles, the diagnostic model and its
renderers, the `P0xxx`/`Fxxxx` diagnostic catalog, `{@...}` annotations,
`rustlike.pas`, the test/runtime harness, configuration, milestones, and the
remaining open decisions.
