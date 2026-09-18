#!/usr/bin/env python3
"""Merge spec/terms/fragments/*.toml into the four flat registries.

A contract owns its fragment; the flat registries are derived from them exactly
as spec/build/ is derived from the contract files, and the same equality rule
applies — a registry differing from a fresh merge is stale.

    python3 tools/spec/merge-registry.py           # write
    python3 tools/spec/merge-registry.py --check   # fail on any difference

An operation is keyed `<contract>.<operation>`, so two contracts needing one verb
each register it under their own key. A term, error or named bound is keyed on its
spelling alone and is claimed by the first fragment in contract order; a second
claim is a collision and is reported, because one spelling naming two things is
what the registry exists to refuse. A fragment may declare `owner` pointing at
another contract, which is how a term lands with its obligor when the obligor's
fragment is not the one being edited.
"""
import json
import os
import sys
import tomllib
from collections import defaultdict

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
FRAGMENTS = os.path.join(ROOT, "spec", "terms", "fragments")
TERMS = os.path.join(ROOT, "spec", "terms")

# Contract order decides which fragment wins a contested spelling, and it is the
# order contract.toml declares.
ORDER = [
    "corpus", "topology", "store", "sync", "read", "memory", "run", "pipeline",
    "connector", "secret", "derive", "authority", "enforcement", "visibility",
    "disclosure", "accountability", "control", "console", "formal", "build",
    # `wire` is not a contract. It carries protocol vocabulary no party owes, and
    # it sorts last so a contract's own spelling always wins over it.
    "wire",
]

HEADERS = {
    "operation": (
        "# Operations — the second segment of a clause id, keyed by `<contract>.<operation>`.\n"
        "# Exactly one file per contract claims each operation, and a contract spans several\n"
        "# files, so the key decides the file without forcing a synonym on a second contract\n"
        "# that needs the same verb.\n"
        "#\n"
        "# Generated from spec/terms/fragments/ — edit a fragment, then run\n"
        "# `python3 tools/spec/merge-registry.py`."
    ),
    "term": (
        "# Subjects — the fourth segment of a clause id, and every backticked token appearing\n"
        "# in two or more contract files. `resolves = \"code\"` marks a token the status\n"
        "# computation looks for in a definition position; `\"concept\"` is excluded from that\n"
        "# test. Owner `wire` names protocol vocabulary no contract owes, which no clause is\n"
        "# addressed on and the usage-to-owner join passes over.\n"
        "#\n"
        "# Generated from spec/terms/fragments/."
    ),
    "error": (
        "# Error identifiers — every `refusal` clause names one.\n"
        "#\n"
        "# Generated from spec/terms/fragments/."
    ),
    "limit": (
        "# Named bounds — a numeral in a `limit` clause resolves to exactly one entry, and each\n"
        "# entry is asserted by exactly one clause. Two independent bounds carrying the same\n"
        "# number are two entries; one shared ceiling is one entry reached by reference.\n"
        "#\n"
        "# Generated from spec/terms/fragments/."
    ),
}

FIELDS = {
    "operation": ["gloss", "file", "owner", "aliases"],
    "term": ["gloss", "owner", "resolves", "aliases"],
    "error": ["gloss", "owner"],
    "limit": ["value", "unit", "owner", "gloss"],
}


def fragment_order(name: str) -> int:
    stem = name[:-5]
    return ORDER.index(stem) if stem in ORDER else len(ORDER)


def merge():
    ops: dict[str, dict] = {}
    flat: dict[str, dict[str, dict]] = {t: {} for t in ("term", "error", "limit")}
    claimed: dict[tuple[str, str], str] = {}
    collisions: list[tuple[str, str, str, str]] = []
    failures: list[tuple[str, str]] = []

    for name in sorted(os.listdir(FRAGMENTS), key=fragment_order):
        if not name.endswith(".toml"):
            continue
        contract = name[:-5]
        try:
            with open(os.path.join(FRAGMENTS, name), "rb") as fh:
                doc = tomllib.load(fh)
        except Exception as exc:  # a fragment that does not parse is a hard stop
            failures.append((name, str(exc)))
            continue

        for key, value in (doc.get("operation") or {}).items():
            if not isinstance(value, dict):
                value = {"gloss": str(value)}
            value["owner"] = contract
            ops[f"{contract}.{key}"] = value

        for table in flat:
            for key, value in (doc.get(table) or {}).items():
                if not isinstance(value, dict):
                    value = {"gloss": str(value)}
                if key in flat[table]:
                    collisions.append((table, key, claimed[(table, key)], contract))
                    continue
                value.setdefault("owner", contract)
                flat[table][key] = value
                claimed[(table, key)] = contract

    return ops, flat, collisions, failures


def render(table: str, entries: dict[str, dict]) -> str:
    lines = [HEADERS[table], "", f"[{table}]"]
    for key in sorted(entries):
        value = entries[key]
        parts = [
            f"{field} = " + json.dumps(value[field])
            for field in FIELDS[table]
            if field in value and value[field] not in (None, "", [])
        ]
        plain = all(ch.isalnum() or ch in "-_" for ch in key)
        rendered = key if plain and table != "operation" else json.dumps(key)
        lines.append(f"{rendered} = {{ " + ", ".join(parts) + " }")
    return "\n".join(lines) + "\n"


def main() -> int:
    check = "--check" in sys.argv
    ops, flat, collisions, failures = merge()

    for name, exc in failures:
        print(f"fragment {name} does not parse: {exc}", file=sys.stderr)
    for table, key, first, second in collisions:
        print(
            f"[{table}] `{key}` claimed by both `{first}` and `{second}` — "
            f"one spelling names one thing; rename the second",
            file=sys.stderr,
        )

    outputs = {"operation": ops, **flat}
    stale = []
    for table, entries in outputs.items():
        path = os.path.join(TERMS, f"{table}.toml")
        text = render(table, entries)
        current = open(path).read() if os.path.exists(path) else ""
        if check:
            if current != text:
                stale.append(table)
        elif current != text:
            open(path, "w").write(text)

    counts = "  ".join(f"{t} {len(e)}" for t, e in outputs.items())
    if check:
        for table in stale:
            print(f"spec/terms/{table}.toml is stale", file=sys.stderr)
        print(counts, file=sys.stderr)
        return 1 if (stale or collisions or failures) else 0

    print(counts, file=sys.stderr)
    return 1 if (collisions or failures) else 0


if __name__ == "__main__":
    raise SystemExit(main())
