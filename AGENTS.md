# Working with fpcheck

This repository builds `fpcheck`, a Rust-style feedback layer for Free Pascal.
It is developed with Rust (`cargo`); Free Pascal is the *target* of the tool, not
a build dependency of it.

- After every edit run `fpcheck check --message-format=llm` and fix all errors before moving on.
- Fix root causes first; later errors often disappear.
- Run `fpcheck explain <code>` when a diagnostic is unclear.
- Never add `{@allow}` or `{@unsafe}` to silence a diagnostic without a real reason string.
- Apply `fpcheck fix` for MachineApplicable suggestions, then re-check.
- Before declaring a task done: `fpcheck test` must pass with no leaks.
- Prefer `TOption`/`TResult`/`TBox` from rustlike.pas over nil, error codes, and bare Create/Free.

## Building and testing this repository

```sh
cargo build          # builds target/debug/fpcheck
cargo test           # 270 unit + integration tests, no Free Pascal required
```

`fpcheck check` invokes `fpc`. On a machine without Free Pascal, use
`fpcheck check --no-fpc` to run the analyzer alone; the fpc-facing pipeline is
covered by tests with a fake compiler instead.

## Ground rules for changes

- `SPEC.md` is the specification. Read the relevant section before changing behaviour.
- Diagnostic codes are **stable forever**; never reuse or renumber one (spec section 6).
- Default levels live with the lint in `src/analyzer/mod.rs`, not in the catalog.
  Message text lives in `catalog/*.toml` as data.
- A false negative is better than a false positive (spec section 7.4). If a lint
  cannot decide, it stays silent.
