#!/usr/bin/env python3
"""Scan tracked files for obvious secret patterns.

The scanner intentionally targets common high-signal credential formats while
remaining configurable via .secrets-allowlist.txt.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(subprocess.check_output(["git", "rev-parse", "--show-toplevel"], text=True).strip())
ALLOWLIST_PATHS = [ROOT / ".secrets-allowlist.txt", ROOT / ".github" / "secrets-allowlist.txt"]
PATTERNS = [
    re.compile(r"AKIA[0-9A-Z]{16}"),
    re.compile(r"AIza[0-9A-Za-z_-]{35}"),
    re.compile(r"ghp_[A-Za-z0-9]{36}"),
    re.compile(r"xox[baprs]-[A-Za-z0-9-]+"),
    re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
    re.compile(r"(?i)(api[_-]?key|secret|token|password)\s*[:=]\s*['\"]?[A-Za-z0-9/._+=:-]{8,}"),
    re.compile(r"(?i)aws_access_key_id\s*[:=]\s*ASIA[0-9A-Z]{12,}"),
]


def load_allowlist() -> list[re.Pattern[str]]:
    patterns: list[re.Pattern[str]] = []
    for allowlist_path in ALLOWLIST_PATHS:
        if not allowlist_path.exists():
            continue
        for raw_line in allowlist_path.read_text(encoding="utf-8").splitlines():
            candidate = raw_line.strip()
            if not candidate or candidate.startswith("#"):
                continue
            patterns.append(re.compile(candidate))
    return patterns


def should_skip(rel_path: str, allowlist: list[re.Pattern[str]]) -> bool:
    rel = rel_path.replace(os.sep, "/")
    return any(pattern.search(rel) for pattern in allowlist)


def collect_files() -> list[str]:
    output = subprocess.check_output(["git", "ls-files"], cwd=str(ROOT), text=True)
    files = [line.strip() for line in output.splitlines() if line.strip()]
    return [path for path in files if not path.startswith(".git/")]


def main() -> int:
    allowlist = load_allowlist()
    matches: list[str] = []
    for rel_path in collect_files():
        if should_skip(rel_path, allowlist):
            continue
        full_path = ROOT / rel_path
        if not full_path.is_file():
            continue
        try:
            for lineno, line in enumerate(full_path.read_text(encoding="utf-8", errors="ignore").splitlines(), start=1):
                if any(pattern.search(line) for pattern in PATTERNS):
                    matches.append(f"{rel_path}:{lineno}:{line.strip()}")
        except UnicodeDecodeError:
            continue

    if matches:
        print("Potential secrets detected:\n")
        for match in matches[:50]:
            print(match)
        print(f"\n{len(matches)} potential secret matches found. Update the allowlist or rotate the credential.")
        return 1

    print("Secret scan passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
