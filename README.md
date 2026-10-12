# fpcheck

A Rust-style feedback layer for Free Pascal: one command that returns precise,
structured, actionable diagnostics for an edit → check → fix loop, aimed
primarily at LLM coding agents.

**Status:** working implementation of milestone M0 plus the cheap, precise part
of M1, against [SPEC.md](SPEC.md) (specification v0.1).

```console
$ fpcheck check --message-format=llm
error[P0702] src/lexer.pas:16:3: `with` is banned
 14 | begin
 15 |   {$R-}
>16 |   with Foo do Bar;
    |   ^^^^
 17 |   if x = 0.1 then goto done;
  note: `with` hides which object a name belongs to
  help: bind the target to a local variable and qualify field accesses explicitly

fpcheck: 4 errors, 3 warnings. Run 'fpcheck explain <code>' for details.
```

## Quick start

```sh
cargo build --release
./target/release/fpcheck init --agent     # writes fpcheck.toml and AGENTS.md
./target/release/fpcheck check            # analyzer + fpc
./target/release/fpcheck check --no-fpc   # analyzer only, no Free Pascal needed
./target/release/fpcheck explain P0702
./target/release/fpcheck fix              # apply MachineApplicable suggestions
```

`check` invokes `fpc`; without a Free Pascal installation it fails with exit
code `2` (pointing at `--no-fpc`) rather than pretending the code is fine.
Exit codes: `0` clean, `1` diagnostics at error level, `2` tool or config
failure.

## What is implemented

**The feedback loop (M0, complete)**

- Cargo-like CLI: `check`, `build`, `fix`, `fmt`, `explain`, `init`, with
  `--message-format=human|json|llm|sarif`, `--max-diagnostics`,
  `--profile=dev|release`, `--deny/--warn/--allow`, `--deny-warnings`,
  `--report-allows`, `-p`.
- Strict `dev`/`release` profiles built from `fpcheck.toml` (range, overflow,
  I/O, stack and method checks, assertions, heap traces, debug info, `-O-`).
- FPC runner that captures stdout and stderr **into one stream** so message
  order — and therefore cascade suppression — is preserved.
- Message parser for `file(line,col) Severity: text`, line-only locations, bare
  `Fatal:` lines, banners, progress lines, and optional numeric message ids.
- Normalizer: **point → span expansion** using the lexer's token spans, catalog
  lookup, **cascade suppression** after identifier/unit-not-found, duplicate
  removal, level remapping, and root-cause-first ordering.
- Four renderers over one diagnostic model: rustc-style `human`, JSON Lines with
  a versioned summary, compact `llm` (the main target, with unified diffs of
  suggestions), and SARIF 2.1.0.
- Message catalog as **data**: `catalog/lints.toml` (46 codes) and
  `catalog/fpc.toml` (59 compiler-message templates), embedded in the binary and
  overridable per project.
- `explain CODE` plus 46 long-form `docs/<CODE>.md` write-ups with failing and
  fixed examples.

**Lints (part of M1)**

| Code | Name | Default |
|---|---|---|
| P0001 | allow-without-reason | deny |
| P0304 | checks-disabled | deny (with a machine-applicable fix) |
| P0306 | float-equality | warn |
| P0701 | unsafe-outside-unsafe | deny |
| P0702 | with-statement | deny |
| P0703 | goto | warn |
| P0801 | empty-except | deny |
| P0802 | catch-all-except | warn |
| P0902 | todo-marker | warn |

Suppression is `{@allow(CODE, reason: "...")}`; a missing `reason:` is itself an
error, and `--report-allows` lists every suppression for review.

## What is not implemented

Stated plainly, because "it checks clean" is supposed to mean something:

- **No Free Pascal was available on the machine this was built on.** Every
  flag mapping in `src/fpc/profile.rs` and every compiler-message template is
  therefore *unverified against a real compiler* — the spec itself says to
  verify them (section 4.1). The fpc-facing pipeline is covered by tests against
  a fake compiler that emits realistic output, not by a real compile.
- **No numeric FPC message ids were verified.** Codes for compiler messages are
  derived from the message template (`F1000`–`F9999`, stable because the input
  is canonicalized first). When the compiler does print an id, `F<id>` is used.
- `fpcheck test` (FPCUnit runner, heaptrc leak parsing, runtime-error mapping,
  M2) and `fpcheck lsp` (M4) are explicit stubs that exit `2` and say so.
- Ownership, nullability and aliasing lints (P04xx, P05xx, P06xx) and the
  annotation semantics they need (M3) are not implemented.
- `rustlike.pas` is **deliberately not written**: without a compiler it could
  not be built or tested, and shipping unverified Pascal would undermine the
  point of the tool.
- Incremental caching, `infer --suggest`, fuzzing and the SARIF polish of M4 are
  not implemented. `fmt` wraps `ptop` but is untested here (no `ptop`).

## Repository layout

| Path | Contents |
|---|---|
| `SPEC.md` | the specification this implements |
| `src/diagnostic.rs` | the single diagnostic model |
| `src/lexer.rs` | lossless Pascal lexer with full token spans |
| `src/source.rs` | line index, span extraction, snippets |
| `src/fpc/` | `profile` (flags), `runner` (invoke), `parser` (messages), `normalize` (spans, cascades) |
| `src/analyzer/` | the lint registry and one module per lint family |
| `src/render/` | `human`, `json`, `llm`, `sarif` |
| `src/catalog.rs` | embedded catalog loader and message-template engine |
| `src/config.rs` | `fpcheck.toml` parsing, profiles, per-unit strictness globs |
| `src/fix.rs` | applying `MachineApplicable` suggestions |
| `catalog/`, `docs/` | diagnostic text and long-form explanations |
| `tests/cli.rs` | end-to-end tests of the binary |

## Development

```sh
cargo test        # 270 unit and integration tests; no Free Pascal required
cargo build
```

If the default `CARGO_HOME` is outside a writable workspace, point it inside:
`CARGO_HOME=$PWD/.cargo-home cargo test`.

Diagnostic codes are stable forever and are declared in exactly one place
(`src/analyzer/mod.rs`); message text is data in `catalog/`. See
[AGENTS.md](AGENTS.md) for the conventions.
