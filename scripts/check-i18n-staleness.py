#!/usr/bin/env python3
"""Flag translations of the key onboarding docs that lag behind their English source.

Each translation under docs/i18n/<lang>/ declares the source it was made from
and the source commit it reflects in its front matter (see docs/i18n/README.md).
A translation is stale when its source changed between source_commit and HEAD.

Usage:
    python3 scripts/check-i18n-staleness.py [--repo PATH] [--strict]

Without --strict the script reports and exits 0; stale and invalid files are
emitted as GitHub warning annotations when running under GitHub Actions.
"""
import argparse
import os
import pathlib
import subprocess
import sys

I18N_DIR = "docs/i18n"
KEY_SOURCES = {"README.md", "CONTRIBUTING.md", "docs/QUICK_START.md"}


def parse_front_matter(text):
    """Return the front matter as a dict, or None if it is missing or unterminated."""
    lines = text.splitlines()
    if not lines or lines[0].strip() != "---":
        return None
    meta = {}
    for line in lines[1:]:
        if line.strip() == "---":
            return meta
        key, sep, value = line.partition(":")
        if sep:
            meta[key.strip()] = value.strip()
    return None


def git(repo, *args):
    return subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True)


def check_translation(repo, path):
    """Return (status, detail) where status is 'current', 'stale' or 'invalid'."""
    meta = parse_front_matter(path.read_text(encoding="utf-8"))
    if meta is None:
        return "invalid", "missing or unterminated front matter"

    source = meta.get("source", "")
    commit = meta.get("source_commit", "")
    if source not in KEY_SOURCES:
        return "invalid", f"source '{source}' is not one of {sorted(KEY_SOURCES)}"

    expected_name = pathlib.PurePosixPath(source).name
    if path.name != expected_name or path.parent.parent != repo / I18N_DIR:
        return "invalid", f"expected file at {I18N_DIR}/<lang>/{expected_name}"

    if not commit:
        return "invalid", "missing source_commit"
    if git(repo, "cat-file", "-e", f"{commit}^{{commit}}").returncode != 0:
        return "invalid", f"source_commit {commit} not found in history"

    diff = git(repo, "diff", "--quiet", commit, "HEAD", "--", source)
    if diff.returncode == 0:
        return "current", ""
    if diff.returncode == 1:
        latest = git(repo, "log", "-1", "--format=%h", "HEAD", "--", source).stdout.strip()
        return "stale", f"{source} changed since {commit[:12]} (latest: {latest})"
    return "invalid", f"git diff failed: {diff.stderr.strip()}"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--repo", default=".", help="repository root (default: current directory)")
    parser.add_argument("--strict", action="store_true", help="exit 1 if any translation is stale or invalid")
    args = parser.parse_args()

    repo = pathlib.Path(args.repo).resolve()
    key_names = {pathlib.PurePosixPath(s).name for s in KEY_SOURCES}
    translations = sorted(p for p in (repo / I18N_DIR).glob("*/*.md") if p.name in key_names)
    in_actions = os.environ.get("GITHUB_ACTIONS") == "true"

    problems = 0
    for path in translations:
        rel = path.relative_to(repo).as_posix()
        status, detail = check_translation(repo, path)
        print(f"{status:8} {rel}" + (f"  ({detail})" if detail else ""))
        if status != "current":
            problems += 1
            if in_actions:
                print(f"::warning file={rel}::{status}: {detail}")

    if not translations:
        print(f"no translations found under {I18N_DIR}/")

    return 1 if problems and args.strict else 0


if __name__ == "__main__":
    sys.exit(main())
