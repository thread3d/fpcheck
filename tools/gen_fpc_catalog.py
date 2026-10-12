#!/usr/bin/env python3
"""Regenerate catalog/fpc.toml from an installed Free Pascal compiler.

The authoritative source is FPC's own English message catalogue, ``errore.msg``,
which lives next to the compiler binaries and maps every message number to its
exact template text::

    sym_e_id_not_found=05000_E_Identifier not found "$1"

That file is what the compiler itself prints from, so the templates here cannot
drift from reality the way hand-written guesses can. This script was validated
against FPC 3.2.2 on x86_64-darwin: every error triggered in
``tests/real_fpc.rs`` matched its catalogue entry exactly.

Usage::

    python3 tools/gen_fpc_catalog.py [path/to/errore.msg] [-o catalog/fpc.toml]

With no argument the path is discovered from ``fpc -iV`` and ``FPCHECK_FPC``.
The curated overlay below is the *only* hand-written part: it gives clearer
headlines and actionable ``help`` lines to the messages agents hit most often.
Everything else uses FPC's own wording verbatim.
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

# ``name=NNNNN_TYPE_Text`` where TYPE may be several letters (e.g. ``TL``) and a
# leading ``-`` marks a message that is off by default.
ENTRY = re.compile(r"^([A-Za-z0-9_]+)=(\d{5})_(-?[A-Za-z]+)_(.*)$")

#: Message classes worth turning into diagnostics. F=fatal, E=error, W=warning,
#: N=note, H=hint. Others (T=tried, U=used, D=debug, C=conditional, I=info,
#: X=executable, O=output, L=linenumber) are compiler chatter, not diagnostics.
DIAGNOSTIC_CLASSES = set("FEWNH")

#: Clearer prose for the messages an agent actually hits. Keyed by message
#: number. Placeholders are ``{p1}``, ``{p2}`` ... matching FPC's ``$1``, ``$2``.
CURATED: dict[int, dict[str, object]] = {
    1018: dict(
        headline="compilation aborted",
        note="reported after the real errors; the root causes come first",
    ),
    2000: dict(
        headline="unexpected end of file",
        label="the file ends inside a block",
        help=["close the open `begin`, `try`, `record` or `class` before the end of the file"],
    ),
    2003: dict(
        headline="expected `{p1}` but found `{p2}`",
        label="unexpected token",
        help=["check the statement for a missing or extra token"],
        note="the token name is FPC's, not the literal source text",
    ),
    2013: dict(
        headline="cannot open include file `{p1}`",
        label="include file not found",
        help=["add its directory to `[fpc] units` in fpcheck.toml, or fix the `{{$I ...}}` path"],
    ),
    3026: dict(
        headline="wrong number of parameters in the call to `{p1}`",
        label="argument count does not match the declaration",
        help=["check the routine's signature, including default parameters"],
    ),
    3203: dict(
        headline="illegal expression",
        label="this is not a valid expression",
        help=["check for a missing operand, a stray operator, or a missing semicolon above"],
    ),
    3205: dict(
        headline="illegal qualifier",
        label="cannot qualify this expression",
        help=["the value on the left is not a record, class, or unit that has this member"],
    ),
    4001: dict(
        headline="incompatible types: got `{p1}` expected `{p2}`",
        label="type mismatch",
        help=["convert explicitly, e.g. `Integer(x)`", "or change the declared type if the value is right"],
    ),
    4110: dict(
        headline="constant expression is out of range: {p1}",
        label="range check failed at compile time",
        help=["narrow the constant, or widen the target type"],
        note="caught before runtime because both sides are constant",
    ),
    5000: dict(
        headline="undeclared identifier `{p1}`",
        label="not found in this scope",
        help=[
            "check the spelling and the identifier's case",
            "add the unit that declares it to the `uses` clause",
            "if it is a field, qualify it with the instance that owns it",
        ],
    ),
    5002: dict(
        headline="duplicate identifier `{p1}`",
        label="an identifier with this name already exists here",
        help=["rename one of them, or remove the redundant declaration"],
    ),
    5024: dict(
        headline="parameter `{p1}` is never used",
        label="unused parameter",
        help=["remove it, or prefix the name with `_` to mark it intentionally unused"],
    ),
    5025: dict(
        headline="local variable `{p1}` is never used",
        label="unused local variable",
        help=["remove it, or prefix the name with `_` to mark it intentionally unused"],
    ),
    5028: dict(
        headline="local {p1} `{p2}` is never used",
        label="unused local declaration",
        help=["remove it, or prefix the name with `_` to mark it intentionally unused"],
    ),
    5033: dict(
        headline="function result may not be set on every path",
        label="the function can return without assigning `Result`",
        help=["assign `Result` on every path, or raise instead of falling through"],
    ),
    5037: dict(
        headline="`{p1}` may be used before it is initialized",
        label="read before definite assignment",
        help=["assign it before this point on every path"],
    ),
    10022: dict(
        headline="cannot find unit `{p1}`",
        label="unit not on the search path",
        help=["add its directory to `[fpc] units` in fpcheck.toml"],
        note="required by `{p2}`",
    ),
    10026: dict(
        headline="there were {p1} errors compiling the module",
        note="a summary of the errors already reported",
    ),
    11030: dict(
        headline="reading compiler config file {p1}",
        note="compiler chatter, not a problem in your code",
    ),
    11031: dict(
        headline="finished reading compiler config file {p1}",
        note="compiler chatter, not a problem in your code",
    ),
}


def find_errore_msg() -> Path:
    """Locate ``errore.msg`` next to the compiler named by FPCHECK_FPC or PATH."""
    binary = os.environ.get("FPCHECK_FPC") or shutil.which("fpc")
    if not binary:
        sys.exit("no errore.msg given and no fpc found; pass the path explicitly")
    try:
        version = subprocess.run(
            [binary, "-iV"], capture_output=True, text=True, check=True
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError) as exc:
        sys.exit(f"could not run {binary}: {exc}")
    # <prefix>/bin/fpc -> <prefix>/lib/fpc/<version>/msg/errore.msg
    prefix = Path(binary).resolve().parent.parent
    candidate = prefix / "lib" / "fpc" / version / "msg" / "errore.msg"
    if not candidate.is_file():
        sys.exit(f"expected message catalogue at {candidate}")
    return candidate


def parse_catalogue(path: Path) -> list[tuple[int, str, str, str]]:
    entries = []
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = line.rstrip("\n")
        if not line.strip() or line[0] in "#%":
            continue  # comment, or TeX documentation for the manual
        match = ENTRY.match(line)
        if not match:
            continue
        name, number, kinds, text = match.groups()
        message_class = kinds.lstrip("-")[0]
        entries.append((int(number), message_class, text, name))
    return entries


def convert_placeholders(text: str) -> str:
    """Turn FPC's ``$1`` into ``{p1}`` and escape literal braces for our loader.

    Order matters: literal braces are doubled *before* placeholders are inserted,
    otherwise ``{p1}`` would itself be escaped and render as literal text.
    """
    out = []
    for part in re.split(r"(\$\d+)", text):
        if re.fullmatch(r"\$\d+", part):
            out.append("{p" + part[1:] + "}")
        else:
            out.append(part.replace("{", "{{").replace("}", "}}"))
    return "".join(out)


def toml_string(value: str) -> str:
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def emit(entries: list[tuple[int, str, str, str]], source: Path, version: str) -> str:
    lines = [
        "# FPC compiler message catalogue (spec sections 4.2 and 6.4).",
        "#",
        f"# GENERATED by tools/gen_fpc_catalog.py from FPC {version}'s msg/errore.msg.",
        "# Do not edit by hand: add a clearer headline in the script's CURATED overlay",
        "# and regenerate. The templates are the compiler's own wording, so they cannot",
        "# drift from what FPC prints.",
        "#",
        "# Codes are the compiler's real message numbers (`F` + number). The compiler is",
        "# invoked with `-vq`, so every diagnostic arrives with its number and the",
        "# normalizer looks the entry up by code; template matching is only a fallback",
        "# for output produced without `-vq`.",
        "#",
        "# `{p1}`, `{p2}` ... are the compiler's `$1`, `$2` arguments. Literal braces are",
        "# doubled (`{{` renders as `{`).",
        "",
    ]
    curated_count = 0
    for number, message_class, text, name in entries:
        code = f"F{number}"
        template = convert_placeholders(text)
        overlay = CURATED.get(number, {})
        # Overlay strings are already written in our template syntax (`{p1}` for a
        # placeholder, `{{` for a literal brace), so they must not be converted
        # again - only TOML-escaped.
        headline = str(overlay["headline"]) if "headline" in overlay else template
        if overlay:
            curated_count += 1

        lines.append("[[message]]")
        lines.append(f"template = {toml_string(template)}")
        lines.append(f"code = {toml_string(code)}")
        lines.append(f"headline = {toml_string(headline)}")
        if "label" in overlay:
            lines.append(f"label = {toml_string(str(overlay['label']))}")
        if "help" in overlay:
            helps = ", ".join(toml_string(str(h)) for h in overlay["help"])  # type: ignore[union-attr]
            lines.append(f"help = [{helps}]")
        if "note" in overlay:
            lines.append(f"note = {toml_string(str(overlay['note']))}")
        lines.append("")

    header_note = (
        f"# {len(entries)} messages, {curated_count} with curated prose"
        f" (source: {source})"
    )
    lines.insert(1, header_note)
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("catalogue", nargs="?", type=Path, help="path to errore.msg")
    parser.add_argument("-o", "--output", type=Path, default=Path("catalog/fpc.toml"))
    args = parser.parse_args()

    source = args.catalogue or find_errore_msg()
    if not source.is_file():
        sys.exit(f"no such file: {source}")

    try:
        version = source.parent.parent.name
    except IndexError:
        version = "unknown"

    all_entries = parse_catalogue(source)
    diagnostics = [e for e in all_entries if e[1] in DIAGNOSTIC_CLASSES]
    diagnostics.sort(key=lambda e: e[0])

    if not diagnostics:
        sys.exit(f"no diagnostic messages found in {source}")
    missing = sorted(set(CURATED) - {e[0] for e in diagnostics})
    if missing:
        sys.exit(f"curated ids not present in the catalogue: {missing}")

    args.output.write_text(emit(diagnostics, source, version), encoding="utf-8")
    print(
        f"wrote {args.output}: {len(diagnostics)} messages "
        f"({len(CURATED)} curated) from FPC {version}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
