# fpcheck: a Rust-style feedback layer for Free Pascal

Working name: `fpcheck`. Status: specification v0.1.

## 1. Goal

Give Free Pascal (FPC) development the feedback loop Rust gives an LLM (or a human): a single command that returns **precise, structured, actionable diagnostics**, catches whole bug classes **before runtime**, and makes "it checks clean" mean something.

The audience is primarily an LLM coding agent in an edit → check → fix loop. Every design choice is judged by one question: *does this give the agent a signal it can act on without running the program?*

### Non-goals

- Not a sound borrow checker. FPC has no lifetimes; the tool is a best-effort static analyzer with annotations. Prefer **few, precise, high-confidence errors** over many noisy ones.
- Not a new language or a transpiler. Input and output are ordinary FPC source that `fpc` compiles unchanged. All extra information lives in comments.
- Not a replacement for the FPC compiler. `fpc` stays the authority on whether the code compiles.

## 2. Architecture

```
            ┌──────────────────────────────────────────────┐
 .pas/.pp → │ driver (CLI: check / build / test / fix /…)  │
            └───────┬───────────────┬──────────────────────┘
                    │               │
         ┌──────────▼───┐   ┌───────▼─────────────────────┐
         │ FPC runner   │   │ analyzer                    │
         │ strict flags │   │ parse → resolve → CFG →     │
         │ + msg parser │   │ dataflow passes → lints     │
         └──────────┬───┘   └───────┬─────────────────────┘
                    │               │
              ┌─────▼───────────────▼─────┐
              │ diagnostic engine         │
              │ dedupe · cascade-suppress │
              │ span-expand · render      │
              └─────┬───────────┬─────────┘
                    │           │
              human (rustc-ish)  json / llm / sarif
```

### 2.1 Components

1. **Driver**: Cargo-like CLI (section 3), config loading, incremental caching.
2. **FPC runner**: invokes `fpc` with strict profile flags (section 4), captures output, parses it into structured diagnostics.
3. **Analyzer**: its own front end plus dataflow passes (section 5 onward). Runs even when `fpc` fails, so the agent gets analyzer findings and compiler errors in one round trip.
4. **Diagnostic engine**: one internal diagnostic model, many renderers.
5. **Runtime harness**: test runner plus heap-trace and leak parsing (section 9).
6. **Support unit** `rustlike.pas`: generic `TOption<T>`, `TResult<T,E>`, owning smart pointers (section 8).

### 2.2 Front end

Recommended: **`fcl-passrc`** (`pscanner`, `pparser`, `pastree`, `pasresolver`), the parser and resolver that ship with FPC (the pas2js foundation). It handles FPC dialects better than any third-party grammar and gives resolved identifiers and types.

Requirements for the front end, whichever is chosen:

- **Error-recovering parse**: a syntax error must not stop analysis of the rest of the file.
- **Full token spans** (start line/col, end line/col) on every AST node.
- Handles `{$IFDEF}` by evaluating with the configured defines; unreachable branches are parsed in a lenient mode, not dropped.
- Dialects: `objfpc` first; `delphi` and `fpc` modes second. Dialect is read from `{$mode}` in the file or from config.
- Unit search paths and defines taken from config (section 10) so `uses` resolves exactly as `fpc` would.

Fallback if `fcl-passrc` proves too limited on real code: tree-sitter-pascal for syntactic lints, with `fpc` itself as the type oracle.

Implementation language: Free Pascal (dogfooding, direct use of `fcl-passrc`). A Rust or C++ implementation is acceptable if it shells out to a small FPC helper for resolution.

## 3. CLI

| Command | Purpose |
|---|---|
| `fpcheck check` | Fast path: analyzer + `fpc` in check-only mode (no assemble/link, if the installed FPC supports it). No binary produced. |
| `fpcheck build [--release]` | `check`, then compile and link with the selected profile. |
| `fpcheck test` | Build and run FPCUnit tests with structured output (section 9). |
| `fpcheck fix` | Apply all `MachineApplicable` suggestions. |
| `fpcheck fmt [--check]` | Wrap FPC's `ptop` with a project config. |
| `fpcheck explain P0201` | Long-form explanation of a diagnostic code, with a wrong and a right example. |
| `fpcheck lsp` | (Later) Language server over the same engine. |

Common flags: `--message-format=human|json|llm|sarif`, `--max-diagnostics N`, `--profile dev|release`, `-p <package>`, `--deny/--warn/--allow <code>`.

Exit codes: `0` clean (warnings allowed unless `--deny-warnings`), `1` diagnostics at error level, `2` tool or config failure (distinct from "your code is wrong").

## 4. FPC runner and strict profiles

### 4.1 Profiles

Flags below are the intended intent; **verify each against the installed FPC version** (`fpc -i`, `fpc -h`) and make the list overridable in config.

`dev` (default):

- `-Cr` range checking, `-Co` overflow checking, `-Ci` I/O checking, `-Ct` stack checking, `-CR` verify method calls
- `-Sa` assertions on
- `-g -gl -gw` debug info and line info; `-gh` heap trace (leak report at exit)
- `-O-` (no optimization, so checks and traces are faithful)
- Warnings, notes and hints all enabled (`-vewnhi` plus message numbers via `-vq` so messages can be keyed by ID)
- Output directories separated (`-FE`, `-FU`) so the source tree stays clean

`release`: keep range/overflow checks configurable (default **on**, Rust-debug-like safety is the point), `-O2`, strip symbols.

### 4.2 Message normalization

FPC prints `file(line,col) Severity: text` with a **point**, not a span, and often cascades follow-up errors. The runner must:

1. Parse every line into `{file, line, col, severity, fpc_id, text}`.
2. **Expand the point to a span** using the front end's token at that position (the offending identifier, statement, or call).
3. Look up `fpc_id` in a **message catalog** (data file, section 6.4) that adds: a clearer headline, a label for the span, a `help:` line, and a suggested edit where one is mechanical.
4. **Suppress cascades**: after "identifier not found" or "unit not found", drop subsequent errors that mention the same symbol or that occur only because the first one failed. Report root causes first.
5. Preserve the raw FPC text in a `fpc_raw` field so nothing is lost.
6. Treat FPC `Fatal:` messages as hard stops for compilation but **not** for the analyzer.

## 5. Diagnostic model

One model, rendered many ways.

```
Diagnostic {
  code:         string        // "P0201" (analyzer) or "F5025" (normalized FPC id)
  level:        error | warning | note | help
  message:      string        // one-line headline, no trailing period
  primary:      Span          // file, line_start, col_start, line_end, col_end
  labels:       [ {span, text} ]      // secondary spans with short captions
  notes:        [ string ]            // explanation, "why"
  help:         [ string ]            // "what to do"
  suggestions:  [ { applicability, message, edits:[{span, replacement}] } ]
  related:      [ Diagnostic ]        // e.g. "value moved here"
  source:       "fpc" | "fpcheck"
  fpc_raw:      string?
  explain:      "fpcheck explain P0201"
}
```

`applicability`: `MachineApplicable` | `MaybeIncorrect` | `HasPlaceholders` | `Unspecified`, same semantics as rustc.

### 5.1 Human renderer (rustc-style)

```
error[P0201]: non-exhaustive `case`: `tkString` not handled
  --> src/lexer.pas:84:5
   |
84 |     case Tok.Kind of
   |     ^^^^^^^^^^^^^ `TTokenKind` has 6 members, 5 handled
   |
   = note: handled: tkIdent, tkNumber, tkSymbol, tkEOF, tkError
   = help: add a `tkString:` branch, or an explicit `else` that you intend
```

Rules: always file:line:col; always show the source line with a caret span; secondary labels on other lines; `= note` for why, `= help` for what to do.

### 5.2 `--message-format=json`

JSON Lines, one `Diagnostic` object per line, plus a final `{"summary": {...}}` line. Stable schema, versioned (`"schema": 1`).

### 5.3 `--message-format=llm` (the main target)

Compact plain text optimized for an agent's context window:

- Ordered **root cause first**; cascades already removed.
- Capped at `--max-diagnostics` (default 15); trailing line says how many were omitted.
- Each diagnostic: code, level, one-line message, `file:line:col`, ±2 lines of source with the offending span marked, `help:` lines, and, when a `MachineApplicable` or `MaybeIncorrect` suggestion exists, a **unified diff** of the fix.
- Final line: `fpcheck: 3 errors, 2 warnings. Run 'fpcheck explain <code>' for details.`
- No ANSI colors, no box drawing, no progress output.

## 6. Diagnostic catalog

Codes are **stable forever**; never reuse. `P` = analyzer lint, `F` = normalized FPC message (`F` + FPC message number).

### 6.1 Default levels

`deny` = error, fails the build. `warn` = warning. `allow` = off unless enabled.

### 6.2 Lints

**Initialization and flow**

| Code | Name | Default | Description |
|---|---|---|---|
| P0101 | uninitialized-use | deny | Local or `out` param read on some path before definite assignment (flow-sensitive, stricter than FPC's hint). |
| P0102 | unused-result | deny | Result of a `{@must_use}` function discarded. Also applies by default to functions returning `Boolean`/error-code types **declared in the same project** whose result is ignored. |
| P0103 | unused-binding | warn | Unused local, parameter, field (private), unit in `uses`, or private routine. |
| P0104 | unreachable-code | warn | Code after `exit`, `raise`, `Halt`, infinite loop. |
| P0105 | shadowed-name | warn | Local shadows a parameter, field, or outer-scope name. |
| P0106 | could-be-const | warn | Value parameter or local never reassigned; mark `const` / use a constant. |
| P0107 | result-never-assigned | deny | Function has a path that returns without assigning `Result`. |

**Exhaustiveness**

| Code | Name | Default | Description |
|---|---|---|---|
| P0201 | non-exhaustive-case | deny | `case` over an enum, boolean, or subrange with no `else` and not all values covered. |
| P0202 | wildcard-else-on-enum | allow | Opt-in: `else` on a `case` over an enum with `{@exhaustive}`; forces a compile-time error when a new member is added. |
| P0203 | variant-tag-mismatch | deny | Access to a variant-record arm inconsistent with the proven value of its tag. |
| P0204 | duplicate-case-label | deny | Overlapping labels or ranges. |

**Numeric safety (Pascal's implicit conversions are the main hole)**

| Code | Name | Default | Description |
|---|---|---|---|
| P0301 | implicit-narrowing | deny | Assignment or argument that narrows (e.g. `Int64`→`Integer`, `Integer`→`Byte`) without explicit conversion. Suggest `Integer(x)` or a checked helper. |
| P0302 | signed-unsigned-mix | deny | Comparison or arithmetic mixing signed and unsigned operands of unequal range. |
| P0303 | constant-out-of-range | deny | Compile-time-evident overflow or range violation. |
| P0304 | checks-disabled | deny | `{$R-}`, `{$Q-}`, `{$I-}` etc. outside an `{@unsafe}` region (section 7). |
| P0305 | implicit-string-conversion | warn | `AnsiString`↔`UnicodeString`↔`WideString`↔`PChar` crossings that can lose data or codepage. |
| P0306 | float-equality | warn | `=` / `<>` on floating-point operands. |

**Nullability**

| Code | Name | Default | Description |
|---|---|---|---|
| P0401 | nil-dereference | deny | Dereference or member access on a value that may be `nil` (flow-sensitive; `if p <> nil` narrows). |
| P0402 | nullable-to-nonnull | deny | `{@nullable}` value passed or assigned to a `{@nonnull}` target. |
| P0403 | redundant-nil-check | warn | Check on a value proven non-nil. |

**Ownership and lifetime (class instances and heap memory)**

| Code | Name | Default | Description |
|---|---|---|---|
| P0501 | leak | deny | `{@owned}` value not freed or transferred on some path (including exception paths). |
| P0502 | double-free | deny | Value freed twice, or freed after ownership was transferred. |
| P0503 | use-after-free | deny | Use of a value after `Free`, `FreeAndNil`, or a move. Related span points at the free/move. |
| P0504 | free-of-borrowed | deny | `Free` called on a `{@borrowed}` value (param, field marked `{@weak}`, result of a borrowing accessor). |
| P0505 | create-without-protection | deny | Constructor result stored in a local, followed by a fallible call, with no enclosing `try..finally`. Suggest the `try..finally` rewrite (machine-applicable). |
| P0506 | dangling-after-free | warn | Field or global freed with `Free` and later reachable; suggest `FreeAndNil`. |
| P0507 | owned-field-not-released | deny | Class has an `{@owned}` field not freed in `Destroy`. |
| P0508 | ownership-escape | deny | Local `{@owned}` value stored into a longer-lived location without `{@sink}`/transfer. |

**Aliasing and invalidation (the practical "borrow checker")**

| Code | Name | Default | Description |
|---|---|---|---|
| P0601 | var-param-aliasing | deny | Same variable (or a global the callee touches) passed to two `var`/`out` params, or to a `var` param and a `const`/value param whose callee mutates through it. |
| P0602 | mutation-during-iteration | deny | Container modified (`Add`, `Delete`, `Clear`, `SetLength`) inside a `for..in` or index loop over itself. |
| P0603 | interior-reference-invalidated | deny | Pointer, `var` alias, or `with` target into a dynamic array, string, or container element held across an operation that can reallocate it (`SetLength`, `Add`, `Insert`, string append). |
| P0604 | reference-to-local-escapes | deny | `@local`, `Addr(local)`, or a `var`/`const` param address stored in or returned to a longer-lived location. |

**Unsafe constructs and banned features**

| Code | Name | Default | Description |
|---|---|---|---|
| P0701 | unsafe-outside-unsafe | deny | Use of an unsafe construct outside an `{@unsafe}` region (list in section 7.2). |
| P0702 | with-statement | deny | `with` is banned: it makes name resolution depend on the type of an expression, which also degrades LLM output. |
| P0703 | goto | warn | `goto` / labels. |
| P0704 | unchecked-hard-cast | deny | Hard typecast between types of different size or between managed and unmanaged types. |

**Error handling**

| Code | Name | Default | Description |
|---|---|---|---|
| P0801 | empty-except | deny | `except` block that swallows exceptions silently. |
| P0802 | catch-all-except | warn | `on E: Exception` / bare `except` without re-raise or logging. |
| P0803 | exception-from-destructor | deny | Path that can raise out of `Destroy` or a `finally` cleanup block. |
| P0804 | ignored-io-result | warn | `IOResult` / `{$I-}` operation without a check. |

**API hygiene**

| Code | Name | Default | Description |
|---|---|---|---|
| P0901 | public-unused-externally | allow | Interface-section symbol never referenced outside its unit. |
| P0902 | todo-marker | warn | `Todo()` / `Unreachable()` calls present (see section 8). |
| P0903 | global-mutable-state | allow | Mutable unit-level variable accessed from more than one thread entry point (phase 4). |

### 6.3 Escape hatch

```pascal
{@allow(P0801, reason: "shutdown path, nothing to recover")}
try ... except end;
```

`reason:` is **mandatory**; omit it and the allow itself is an error (P0001). `fpcheck check --report-allows` lists every suppression so a reviewer (or agent) can audit them. There is no blanket "disable all".

### 6.4 Message catalog (data, not code)

`catalog/*.toml` keyed by code or FPC message id, so improving a message never needs a rebuild:

```toml
[F5025]                       # example key; use real ids from `-vq` output
headline = "local variable `{name}` is never used"
label    = "declared here"
help     = ["remove it, or prefix with `_` to mark intentional"]
```

`fpcheck explain` reads the same catalog plus a `docs/<code>.md` with a failing example, a fixed example, and rationale.

## 7. Annotations

All annotations are **magic comments** (`{@...}`), ignored by `fpc`, parsed by `fpcheck`. They attach to the **next** declaration, parameter, field, or statement. (Real FPC attributes via `prefixedattributes` were considered and rejected: they require RTTI classes and change the compiled program.)

### 7.1 Annotation set

| Annotation | Applies to | Meaning |
|---|---|---|
| `{@owned}` | var, field, function result, parameter | Holder is responsible for freeing. Constructors' results are implicitly owned. |
| `{@borrowed}` | parameter, result, field | Holder must not free or retain beyond the call. **Default for class-typed params.** |
| `{@sink}` | parameter | Callee takes ownership; caller must not use the value afterwards. |
| `{@weak}` | field, var | Non-owning back-reference; never freed here. |
| `{@nullable}` / `{@nonnull}` | var, param, field, result | Nullability contract. |
| `{@must_use}` | function | Result must be consumed. |
| `{@exhaustive}` | `case` | Disallow `else` fallback on enums (P0202). |
| `{@unsafe} ... {@end_unsafe}` or `{@unsafe}` before a routine | region | Permit unsafe constructs; the reason string is required: `{@unsafe(reason: "...")}`. |
| `{@pure}` | function | No global writes, no I/O; enables stronger aliasing reasoning. |
| `{@test}` | procedure | Registers an FPCUnit test case (section 9). |
| `{@allow(Pxxxx, reason: "…")}` | statement/decl | Section 6.3. |
| `{@fpcheck strict}` / `{@fpcheck lenient}` | unit header | Per-unit strictness (section 7.3). |

### 7.2 Constructs requiring `{@unsafe}`

Address-of (`@x`, `Addr`) except for procedural-value assignment; pointer arithmetic and `^` on untyped pointers; untyped `var`/`const` parameters; `absolute`; hard casts that change size or managed-ness; `Move`, `FillChar`, `FillByte` on types containing managed fields; inline `asm`; `Pointer`↔integer casts; `{$R-}`/`{$Q-}`/`{$I-}`; calls to routines declared `{@unsafe}`.

### 7.3 Gradual adoption

Existing code must be usable on day one.

- `lenient` (default for unannotated units): only the non-annotation lints run (P01xx, P02xx, P03xx, P07xx, P08xx). Ownership and nullability lints are **warnings** and only fire where annotations exist.
- `strict`: all lints at their default level; unannotated class-typed params default to `{@borrowed}`, results to `{@nullable}` unless proven otherwise, and unknown calls are treated conservatively.
- `fpcheck infer --suggest` (later): propose annotations for a unit from usage so the agent can add them in bulk.

### 7.4 Analysis scope

Intraprocedural dataflow over a CFG built from the AST (including `try..finally`/`try..except` edges and `exit`/`break`/`continue`/`raise`). Cross-routine reasoning relies **only on annotations**, never on whole-program inference (keeps it fast and predictable). Where the analyzer cannot decide, it stays silent. A false negative is better than a false positive.

## 8. Support unit: `rustlike.pas`

Pure FPC, no analyzer dependency; usable on its own. Requires generics (`{$mode objfpc}` or `delphi`) and, for the ownership types, management operators (FPC 3.2+).

- `TOption<T>`: `Some`, `None`, `IsSome`, `Unwrap`, `Expect(msg)`, `UnwrapOr`, `Map`. `Unwrap` on `None` raises with the call site location.
- `TResult<T,E>`: `Ok`, `Err`, `IsOk`, `Unwrap`, `UnwrapErr`, `Expect`, `MapErr`, `OrElse`. Pascal has no `?` operator; the idiom is `if R.IsErr then Exit(TResult<..>.Err(R.UnwrapErr));`, and the unit provides a snippet in `docs/` so the pattern is consistent.
- `TBox<T: class>`: unique owner built on a managed record: `Initialize`/`Finalize` management operators free the instance; `Copy` is a **move** (source is nulled) and logs a debug-build warning if the source was already empty.
- `TRc<T: class>`: reference-counted shared owner (interface-backed).
- `TWeak<T>`: non-owning handle that reads as `None` after the target is freed (debug builds).
- Helpers: `Todo(msg)`, `Unreachable(msg)`, `Unimplemented(msg)`: raise with `file:line` (via `{$I %FILE%}`/`{$I %LINE%}` captured at call site by a macro-like inline). `Todo` triggers P0902 so none ship.
- `Assert`-like `Ensure(cond, msg)` that is **not** stripped in release.

The analyzer gives `TOption`/`TResult` `{@must_use}` semantics automatically, and treats `TBox` as `{@owned}` with `Copy` = move.

## 9. Test and runtime harness

### 9.1 `fpcheck test`

- Discovers `{@test}` procedures and FPCUnit `TTestCase` descendants; generates the runner program (no hand-written boilerplate).
- Builds with the `dev` profile (checks + `-gh`).
- Runs the binary, consumes FPCUnit's XML output, and converts it to the same diagnostic model:

```
error[T0001]: test `TLexerTests.TestUnterminatedString` failed
  --> tests/lexer_tests.pas:42:5
   = note: expected: <tkError>  actual: <tkString>
   = note: stack: Lexer.NextToken (lexer.pas:130) ← TLexerTests.TestUnterminatedString
```

- Stack traces are symbolized to `file:line` (needs `-gl`/`-gw`) and filtered to project frames first.
- `--message-format=json|llm` as for `check`.

### 9.2 Leak and heap diagnostics

`-gh` prints a leak report at exit. The harness parses it and reports each leak as a diagnostic `R0001 leak: N bytes in M blocks allocated at <symbolized frames>`, with the **allocation site as the primary span**. A test run with leaks **fails** (configurable). This is the runtime counterpart to P0501 and catches what the static pass misses.

### 9.3 Runtime error mapping

Runtime errors (range check, overflow, nil access, invalid pointer, unhandled exception) are caught at the top of the generated runner and rendered as diagnostics with symbolized backtraces, never as a bare "Runtime error 201 at $00401234".

### 9.4 Optional

- `--valgrind`: build with `-gv` and parse valgrind output into the same model.
- `--fuzz` (later): drive `{@fuzz}` procedures with generated input under the `dev` profile.

## 10. Configuration: `fpcheck.toml`

```toml
[package]
name    = "mdiscd"
sources = ["src"]
tests   = ["tests"]
main    = "src/mdiscd.pas"

[fpc]
binary   = "fpc"            # or full path
mode     = "objfpc"
units    = ["lib/**", "/usr/share/fpcsrc/packages/*/src"]
defines  = ["LINUX"]
extra    = []               # appended to the profile flags

[profile.dev]
checks   = ["range", "overflow", "io", "stack", "method"]
heaptrc  = true
debug    = "dwarf"
optimize = 0

[profile.release]
checks   = ["range", "overflow"]
optimize = 2

[lints]                       # override default levels
P0301 = "deny"
P0902 = "deny"
P0901 = "warn"

[lints.per-unit]              # glob → strictness
"src/legacy/**" = "lenient"
"src/**"        = "strict"

[llm]
max_diagnostics = 15
context_lines   = 2
```

`fpcheck init` writes a starter file and detects the FPC version, units path, and targets.

## 11. Incremental checking and performance

- Cache per unit keyed by (source hash, dependencies' interface hashes, flags, tool version). Changing a unit's implementation re-checks only that unit; changing its interface re-checks dependents.
- Targets: `fpcheck check` on a 50k-line project in **under 2 s warm**, under 10 s cold, on a typical workstation.
- Analyzer and `fpc` run **in parallel**; results merged by the diagnostic engine.
- `fpcheck lsp` (phase 4) reuses the same cache for edit-time diagnostics.

## 12. Agent integration

Ship an `AGENTS.md` (or `CLAUDE.md`) snippet with `fpcheck init --agent`:

```
- After every edit run `fpcheck check --message-format=llm` and fix all errors before moving on.
- Fix root causes first; later errors often disappear.
- Run `fpcheck explain <code>` when a diagnostic is unclear.
- Never add `{@allow}` or `{@unsafe}` to silence a diagnostic without a real reason string.
- Apply `fpcheck fix` for MachineApplicable suggestions, then re-check.
- Before declaring a task done: `fpcheck test` must pass with no leaks.
- Prefer `TOption`/`TResult`/`TBox` from rustlike.pas over nil, error codes, and bare Create/Free.
```

A single stable command plus a stable output format is what makes the loop reliable; avoid adding modes the agent must choose between.

### 12.1 MCP server (adapter)

`fpcheck mcp` serves the Model Context Protocol on stdio, for harnesses that can call an MCP server but cannot conveniently run shell commands. It is an **adapter over section 12, not an alternative to it**: the CLI loop remains canonical, and the server must call the same core functions and reuse the same renderers, so the two front ends cannot report different things.

- **Transport.** Newline-delimited JSON-RPC 2.0 on stdin/stdout; stdout carries protocol messages and nothing else. Logs, if any, go to stderr. The server exits `0` when the client closes stdin.
- **Capabilities.** `tools` and `resources`. No `prompts`, `logging`, or `sampling`.
- **Tools.** Exactly three, each a form over an existing core function:
  - `check` → `check`/`build` (analyzer + `fpc`; `no_fpc` selects analyzer-only). Text content is `--message-format=llm`; `structuredContent` (revision `2025-06-18` onward) is the full, uncapped diagnostic set plus the versioned summary of section 5.2. This is the *only* mode choice, and it exists because a machine may have no compiler.
  - `fix` → `fix`, with `dry_run`.
  - `explain` → `explain`.
- **Resources.** `fpcheck://catalog/lints`, `fpcheck://catalog/fpc`, and `fpcheck://explain/{code}` (also a resource template, so the ~955 compiler codes are reachable without being listed). Resources serve the same catalog and the same `explain::render` output as the CLI.
- **Errors.** A protocol-level failure (unknown method or tool, bad arguments, missing resource) is a JSON-RPC error. A *tool execution* failure (no compiler, unknown diagnostic code) is a normal result with `isError: true`, so the agent can read the message and adapt.
- **No fifth format.** The diagnostic model and the four renderers are the only presentations. Tool output wraps them; it does not reinterpret them.

## 13. Testing the tool itself

1. **UI tests** (as in rustc): `tests/ui/*.pas` with expected `*.stderr` golden files in the human format and expected JSON for a subset. Every lint has at least: one failing case, one passing case, one tricky-flow case (exceptions, `exit`, loops, nested try).
2. **False-positive corpus**: run in `lenient` and `strict` over (a) the FPC RTL/FCL sources, (b) a handful of real Pascal projects, (c) the project's own code. Track diagnostics-per-kLOC per lint; a lint above the agreed noise threshold is downgraded to `warn` or `allow` until fixed.
3. **Suggestion tests**: every `MachineApplicable` fix is applied and the result must still compile with `fpc` and re-check clean for that code.
4. **Catalog tests**: every FPC message id seen in the corpus has a catalog entry or is logged as "unmapped" (tracked, not failing).
5. **Differential check**: analyzer-reported errors that `fpc` also reports must agree on location.

## 14. Milestones (ordered by value to an LLM loop)

**M0: Feedback loop (do first; most of the value)**
Driver, strict profiles, FPC message parser, point→span expansion, cascade suppression, human/json/llm renderers, `explain`, catalog for the ~30 most common FPC messages.

**M1: Cheap, precise lints**
P0102 unused-result, P0201 non-exhaustive-case, P0301/P0302 numeric narrowing and mixing, P0304 checks-disabled, P0701/P0702 unsafe and `with`, P0801 empty-except, P0101/P0107 definite assignment, P0103 unused bindings, `fix`, `fmt`.

**M2: Runtime harness**
`test`, FPCUnit integration, symbolized stack traces, heaptrc leak diagnostics, runtime error mapping, `rustlike.pas` (`TOption`, `TResult`, `Todo`).

**M3: Ownership and nullability**
Annotation parser, CFG with exception edges, P05xx ownership lints (start with P0505 and P0501), P04xx nullability, `TBox`/`TRc`.

**M4: Aliasing and polish**
P06xx aliasing/invalidation, incremental cache, `lsp`, `infer --suggest`, thread-safety lint (P0903), SARIF output.

## 15. Open decisions

1. Front end: confirm `fcl-passrc` handles the dialect features actually used (generics, helpers, management operators, inline `var`) before committing; otherwise tree-sitter plus `fpc` as oracle.
2. Annotation spelling: `{@owned}` proposed. Confirm it does not collide with any existing tool's comment syntax in the target code.
3. Whether `P0102` should also flag ignored `Boolean` results by default (noisy in legacy code) or only `{@must_use}` ones.
4. Minimum supported FPC version (3.2.2 assumed; trunk-only features such as inline `const`/`var` and function references are optional).
5. Whether release builds keep range/overflow checks on by default (proposed: yes).
