#!/usr/bin/env python3
"""
Verify that all test suites referenced in the multi-tenant isolation compliance
checklist exist and contain tests.

This script:
1. Parses the compliance checklist markdown
2. Extracts all test file references
3. Verifies each test file exists in the codebase
4. Checks that test files contain actual test functions
5. Reports any missing or empty test files

Exit codes:
- 0: All test files exist and have tests
- 1: Missing or empty test files
"""

import os
import re
import sys
from pathlib import Path
from typing import Set

def extract_test_files(checklist_path: Path) -> Set[str]:
    """Extract all test file references from the compliance checklist.
    Returns set of test file names (without .rs extension)."""
    test_files = set()

    with open(checklist_path, 'r') as f:
        content = f.read()

    # Pattern: cargo test --test test_file_name
    command_pattern = r'cargo test --test ([a-z_0-9_]+)'
    for match in re.finditer(command_pattern, content):
        test_file = match.group(1)
        test_files.add(test_file)

    # Also extract from backtick references: tests/file.rs::
    ref_pattern = r'tests/([a-z_0-9_]+)\.rs::'
    for match in re.finditer(ref_pattern, content):
        test_file = match.group(1)
        test_files.add(test_file)

    return test_files

def verify_test_file(repo_root: Path, test_file: str) -> bool:
    """Verify a test file exists and contains at least one test."""
    test_file_path = repo_root / 'tests' / f'{test_file}.rs'

    if not test_file_path.exists():
        return False

    with open(test_file_path, 'r') as f:
        content = f.read()

    # Look for #[test] or #[tokio::test]
    test_pattern = r'#\[(?:tokio::)?test\]'
    return bool(re.search(test_pattern, content))

def main():
    """Main entry point."""
    repo_root = Path(__file__).parent.parent
    checklist_path = repo_root / 'docs' / 'multi-tenant-isolation-compliance-checklist.md'

    if not checklist_path.exists():
        print(f"Error: Compliance checklist not found: {checklist_path}", file=sys.stderr)
        sys.exit(1)

    # Extract test file references
    test_files = extract_test_files(checklist_path)

    if not test_files:
        print("Warning: No test file references found in compliance checklist", file=sys.stderr)
        sys.exit(0)

    print(f"Verifying {len(test_files)} test suites referenced in compliance checklist...")

    # Verify test files exist and have tests
    missing = []
    empty = []

    for test_file in sorted(test_files):
        test_file_path = repo_root / 'tests' / f'{test_file}.rs'

        if not test_file_path.exists():
            missing.append(test_file)
        elif not verify_test_file(repo_root, test_file):
            empty.append(test_file)

    if missing:
        print("\n❌ Missing test files:", file=sys.stderr)
        for test_file in missing:
            print(f"  - tests/{test_file}.rs", file=sys.stderr)

    if empty:
        print("\n⚠️  Test files with no tests:", file=sys.stderr)
        for test_file in empty:
            print(f"  - tests/{test_file}.rs", file=sys.stderr)

    if missing or empty:
        print(f"\nTotal issues: {len(missing) + len(empty)}", file=sys.stderr)
        print("\nRun: cargo test --test <name> -- --nocapture", file=sys.stderr)
        sys.exit(1)

    print("✅ All test files exist and contain tests")
    print(f"   Verified {len(test_files)} test suites")
    sys.exit(0)

if __name__ == '__main__':
    main()
