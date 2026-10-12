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
cargo test           # 297 unit + integration tests
cargo fmt            # formatting is enforced; keep `cargo fmt --check` clean
```

The suite passes without Free Pascal installed: the 12 tests in
`tests/real_fpc.rs` skip when no compiler is available. To run them, point
`FPCHECK_FPC` at one:

```sh
FPCHECK_FPC=/path/to/fpc cargo test
```

`fpcheck check` invokes `fpc`; `FPCHECK_FPC` also overrides `[fpc] binary` for
the tool itself. On a machine without Free Pascal, `fpcheck check --no-fpc` runs
the analyzer alone.

`catalog/fpc.toml` is **generated** from the compiler's own `msg/errore.msg` by
`tools/gen_fpc_catalog.py` — never hand-edit it. The templates are the
compiler's wording, so they cannot drift; add clearer prose to the `CURATED`
overlay in that script and regenerate.

## Ground rules for changes

- `SPEC.md` is the specification. Read the relevant section before changing behaviour.
- Diagnostic codes are **stable forever**; never reuse or renumber one (spec section 6).
  Lint levels live with the lint in `src/analyzer/mod.rs`; compiler codes are the
  compiler's own message numbers.
- Message text lives in `catalog/*.toml` as data.
- A false negative is better than a false positive (spec section 7.4). If a lint
  cannot decide, it stays silent.
- Behaviour that depends on the compiler (flags, message wording, ids) must be
  verified against a real compiler, not assumed: add it to `tests/real_fpc.rs`.
