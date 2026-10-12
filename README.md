# fpcheck

A Rust-style feedback layer for Free Pascal: one command that returns precise,
structured, actionable diagnostics for an edit → check → fix loop, aimed
primarily at LLM coding agents.

**Status:** working implementation of milestone M0 plus the cheap, precise part
of M1, against [SPEC.md](SPEC.md) (specification v0.1). Verified against a real
**Free Pascal 3.2.2** compiler.

```console
$ fpcheck check --message-format=llm
error[P0702] src/demo.pas:11:3: `with` is banned
  9 |
 10 | begin
>11 |   with TStringList.Create do
    |   ^^^^
 12 |     Add('x');
  note: `with` hides which object a name belongs to
  help: bind the target to a local variable and qualify field accesses explicitly

error[F5000] src/demo.pas:11:8: undeclared identifier `TStringList`
 10 | begin
>11 |   with TStringList.Create do
    |        ^^^^^^^^^^^
 12 |     Add('x');
  help: check the spelling and the identifier's case
  help: add the unit that declares it to the `uses` clause
  label: not found in this scope

fpcheck: 4 errors, 2 warnings. Run 'fpcheck explain <code>' for details.
```

## Quick start

```sh
cargo build --release
./target/release/fpcheck init --agent     # writes fpcheck.toml and AGENTS.md
./target/release/fpcheck check            # analyzer + fpc
./target/release/fpcheck check --no-fpc   # analyzer only, no Free Pascal needed
./target/release/fpcheck explain F5000
./target/release/fpcheck fix              # apply MachineApplicable suggestions
./target/release/fpcheck mcp              # Model Context Protocol server on stdio
```

`check` invokes `fpc`; without a Free Pascal installation it fails with exit
code `2` (pointing at `--no-fpc`) rather than pretending the code is fine.
Exit codes: `0` clean, `1` diagnostics at error level, `2` tool or config
failure.

Set **`FPCHECK_FPC`** to point at a compiler that is not on `PATH`; it overrides
`[fpc] binary` in `fpcheck.toml`:

```sh
FPCHECK_FPC=/opt/fpc/bin/fpc fpcheck check
```

## What is implemented

**The feedback loop (M0, complete)**

- Cargo-like CLI: `check`, `build`, `fix`, `fmt`, `explain`, `init`, with
  `--message-format=human|json|llm|sarif`, `--max-diagnostics`,
  `--profile=dev|release`, `--deny/--warn/--allow`, `--deny-warnings`,
  `--report-allows`, `-p`.
- Strict `dev`/`release` profiles built from `fpcheck.toml` (range, overflow,
  I/O, stack and method checks, assertions, heap traces, debug info, `-O-`,
  message numbers, full paths).
- FPC runner that captures stdout and stderr **into one stream** so message
  order — and therefore cascade suppression — is preserved.
- Message parser for `file(line,col) Severity: text`, line-only locations, bare
  `Fatal:` lines, banners and progress lines, and numeric message ids.
- Normalizer: **point → span expansion** using the lexer's token spans, catalog
  lookup **by the compiler's own message number**, **cascade suppression**,
  duplicate removal, project-relative paths, level remapping, and
  root-cause-first ordering.
- Four renderers over one diagnostic model: rustc-style `human`, JSON Lines with
  a versioned summary, compact `llm` (the main target, with unified diffs of
  suggestions), and SARIF 2.1.0.
- Message catalog as **data**: `catalog/lints.toml` (46 analyzer codes) and
  `catalog/fpc.toml` (**955 compiler messages** with their real numbers),
  embedded in the binary and overridable per project.
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

**MCP server (spec section 12.1)**

`fpcheck mcp` speaks newline-delimited JSON-RPC 2.0 on stdio, for agents whose
harness can call MCP servers but not (or not conveniently) run shell commands.
It is a **thin adapter, not a second implementation**: every request path calls
the same core functions the CLI calls, so the two front ends cannot drift.

| Tool | Core it calls | Notes |
|---|---|---|
| `check` | `cli::run_check` | Text content is byte-identical to `fpcheck check --message-format=llm`; `structuredContent` (from revision `2025-06-18` on) carries every diagnostic, uncapped, plus the versioned summary the JSON renderer uses. |
| `fix` | `cli::run_fix` | Applies `MachineApplicable` suggestions; `dry_run` reports without writing. |
| `explain` | `explain::render` | The tool-shaped form of the resource below. |

Resources serve the reference data: `fpcheck://catalog/lints`,
`fpcheck://catalog/fpc`, and `fpcheck://explain/{code}` (also exposed as a
resource template, so any of the ~955 compiler codes is reachable without
listing all of them). They read the project catalog the analyzer uses and call
the same `explain::render`, so a resource can never say something
`fpcheck explain` would not.

Two transport guarantees are tested: **stdout carries protocol messages only**
(every line is exercised as JSON in `tests/mcp.rs`), and a *tool execution*
failure — a missing compiler, an unknown code — comes back as a normal result
with `isError: true` rather than a transport error, so the agent can read the
message and adapt.

`fpcheck mcp` is additive: `fpcheck check` remains the canonical loop. The
server holds no incremental cache yet (milestone M4), so it resolves config and
the catalog per call; the adapter is structured to host that cache when it
lands.

## Verification against the real compiler

The compiler is the authority for its own flags and messages, so both were
checked against **FPC 3.2.2 (x86_64-darwin)** rather than assumed:

- **Every profile flag is accepted.** The `dev` and `release` flag sets were run
  through the real compiler; an unknown flag makes compilation fail, so the
  clean-compile tests (`tests/real_fpc.rs`) are also a flag test.
- **Real message numbers.** FPC's `-vq` switch means "show message numbers", and
  the profile now requests it, so FPC prints `Error: (5000) Identifier not
  found "x"`. That number *is* the stable code (`F5000`), which makes
  `explain F5000`, `--allow=F5000` and per-code configuration work on real
  output.
- **Templates come from the compiler itself.** `catalog/fpc.toml` is generated
  from FPC's own English catalogue, `msg/errore.msg`, by
  [`tools/gen_fpc_catalog.py`](tools/gen_fpc_catalog.py) — 955 diagnostic
  messages, 20 with curated prose. A hand-written catalogue cannot drift into
  being subtly wrong; this one cannot drift at all.
- **Templates are checked against real output.** `tests/real_fpc.rs` compiles a
  corpus of genuinely broken programs and requires every emitted compiler
  message to be matched by its catalogue entry. During development, 23 of 23
  triggered errors matched their entry **exactly**.
- Codes are `F` + the number as printed, so `5000` is `F5000` and `10022` is
  `F10022`.

Three bugs were found this way and fixed, each of which made the tool
*silently worse* on real projects:

1. **Unresolvable paths.** FPC strips directories from diagnostics (reporting a
   bare `demo.pas` for `src/demo.pas`), so the renderer could not find the file
   and every compiler diagnostic lost its source excerpt. The profile now
   requests `-vb` (full paths) and the normalizer strips the project root again,
   keeping output readable *and* resolvable.
2. **Cascades FPC marks itself.** `<erroneous type>` is FPC's own marker for a
   value whose type it could not determine *because* of an earlier error; such
   messages are now suppressed (but never when nothing has been reported yet).
3. **Summaries reported as diagnostics.** `There were N errors compiling
   module, stopping` was being emitted as an error — with a *basename* even
   under `-vb`, so it also sorted first as an unresolvable path. It is a
   summary of errors already reported, and is now dropped like `Compilation
   aborted`.

## What is not implemented

- `fpcheck test` (FPCUnit runner, heaptrc leak parsing, runtime-error mapping,
  M2) and `fpcheck lsp` (M4) are explicit stubs that exit `2` and say so.
- Ownership, nullability and aliasing lints (P04xx, P05xx, P06xx) and the
  annotation semantics they need (M3) are not implemented.
- `rustlike.pas` is not written yet. Now that a compiler is available it can be
  built and tested, so this is no longer blocked — just not done.
- Incremental caching, `infer --suggest`, fuzzing and the SARIF polish of M4 are
  not implemented. `fmt` wraps `ptop`. In particular `fpcheck mcp` is not yet
  backed by the incremental cache, so each tool call resolves config and the
  catalog afresh.
- **Known cascade limitation.** Suppression is textual: a follow-up error is
  dropped when it repeats the failed symbol. In the example above, `Add` is
  reported even though it is only unresolved because `with TStringList.Create`
  failed — that link needs statement structure, not text. `with` is banned by
  P0702 anyway, so this is rare in code that follows the rules, and
  `--max-diagnostics` plus root-cause ordering bound the cost.

## Repository layout

| Path | Contents |
|---|---|
| `SPEC.md` | the specification this implements (normative) |
| `src/diagnostic.rs` | the single diagnostic model |
| `src/lexer.rs` | lossless Pascal lexer with full token spans |
| `src/source.rs` | line index, span extraction, snippets |
| `src/fpc/` | `profile` (flags), `runner` (invoke), `parser` (messages), `normalize` (spans, cascades) |
| `src/analyzer/` | the lint registry and one module per lint family |
| `src/render/` | `human`, `json`, `llm`, `sarif` |
| `src/mcp/` | MCP stdio server: `protocol` (JSON-RPC), `tools`, `resources` |
| `src/catalog.rs` | embedded catalog loader and message-template engine |
| `src/config.rs` | `fpcheck.toml` parsing, profiles, per-unit strictness globs |
| `src/fix.rs` | applying `MachineApplicable` suggestions |
| `catalog/` | diagnostic text as data (`fpc.toml` is generated) |
| `docs/` | long-form explanations, one per analyzer code |
| `tools/gen_fpc_catalog.py` | regenerates `catalog/fpc.toml` from an installed FPC |
| `tests/cli.rs` | end-to-end tests of the binary (no compiler needed) |
| `tests/mcp.rs` | end-to-end tests of the MCP stdio server (no compiler needed) |
| `tests/real_fpc.rs` | tests that drive a real compiler (skipped without one) |

## Development

```sh
cargo test        # 345 tests
cargo build
```

The 12 tests in `tests/real_fpc.rs` **skip** when no `fpc` is on `PATH` (or in
`FPCHECK_FPC`), so the suite passes on a machine without Free Pascal; set
`FPCHECK_FPC` to run them:

```sh
FPCHECK_FPC=/path/to/fpc cargo test
```

To regenerate the compiler catalogue against a different FPC version:

```sh
FPCHECK_FPC=/path/to/fpc python3 tools/gen_fpc_catalog.py
```

If the default `CARGO_HOME` is outside a writable workspace, point it inside:
`CARGO_HOME=$PWD/.cargo-home cargo test`.

Diagnostic codes are stable forever and are declared in exactly one place
(`src/analyzer/mod.rs` for lints, the compiler itself for `F` codes); message
text is data in `catalog/`. See [AGENTS.md](AGENTS.md) for the conventions.
