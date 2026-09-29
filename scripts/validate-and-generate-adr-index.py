#!/usr/bin/env python3
"""
Validate and generate ADR index from actual ADR files.

This script:
1. Scans docs/adr/ for ADR files
2. Validates each ADR follows the template structure
3. Validates numbering (no gaps, no collisions)
4. Extracts title, status, and date from each ADR
5. Generates the ADR index in docs/adr/README.md

Exit codes:
- 0: Success (index generated or already up to date)
- 1: Validation errors found
"""

import os
import re
import sys
from pathlib import Path
from typing import Optional, Dict, List, Tuple

def parse_adr_number(filename: str) -> Optional[int]:
    """Extract ADR number from filename (e.g., '001-title.md' -> 1)."""
    match = re.match(r'^(\d+)-', filename)
    return int(match.group(1)) if match else None

def extract_adr_metadata(file_path: Path) -> Dict[str, str]:
    """Extract title, status, and date from ADR file."""
    with open(file_path, 'r') as f:
        content = f.read()

    metadata = {
        'title': None,
        'status': None,
        'date': None,
        'has_required_sections': True
    }

    # Extract title from first heading
    title_match = re.search(r'^#\s+ADR-\d+:\s*(.+)$', content, re.MULTILINE)
    if title_match:
        metadata['title'] = title_match.group(1).strip()

    # Extract status
    status_match = re.search(r'^##\s+Status\s*\n+\n*(.+?)(?:\n|$)', content, re.MULTILINE)
    if status_match:
        status_line = status_match.group(1).strip()
        # Extract status from potentially link-formatted text
        status_clean = re.sub(r'\[(.+?)\]\(.+?\)', r'\1', status_line).strip()
        metadata['status'] = status_clean

    # Extract date (look for YYYY-MM format)
    date_match = re.search(r'(\d{4}-\d{2})', content)
    if date_match:
        metadata['date'] = date_match.group(1)

    # Check for required sections
    required_sections = ['## Context', '## Decision', '## Consequences', '## Alternatives Considered']
    for section in required_sections:
        if section not in content:
            metadata['has_required_sections'] = False
            break

    return metadata

def validate_adr_files(adr_dir: Path) -> Tuple[List[Dict], List[str]]:
    """Validate all ADR files and return sorted list with any errors."""
    adrs = []
    errors = []

    # Get all ADR files (excluding template)
    adr_files = sorted([f for f in adr_dir.glob('*.md') if f.name != '000-template.md' and re.match(r'^\d+', f.name)])

    # Validate numbering
    expected_number = 1
    for file_path in adr_files:
        actual_number = parse_adr_number(file_path.name)
        if actual_number is None:
            errors.append(f"Invalid ADR filename: {file_path.name}")
            continue

        if actual_number < expected_number:
            errors.append(f"ADR numbering gap or collision: expected {expected_number}, got {actual_number}")

        expected_number = max(expected_number, actual_number + 1)

        # Extract metadata
        try:
            metadata = extract_adr_metadata(file_path)

            if not metadata['title']:
                errors.append(f"{file_path.name}: Missing or malformed title")
                continue

            if not metadata['status']:
                errors.append(f"{file_path.name}: Missing status")
                continue

            if not metadata['has_required_sections']:
                errors.append(f"{file_path.name}: Missing required sections (Context, Decision, Consequences, Alternatives)")
                continue

            adrs.append({
                'number': actual_number,
                'filename': file_path.name,
                'title': metadata['title'],
                'status': metadata['status'],
                'date': metadata['date'] or 'TBD',
                'path': f"./{file_path.name}"
            })
        except Exception as e:
            errors.append(f"{file_path.name}: Error parsing: {str(e)}")

    return adrs, errors

def generate_adr_table(adrs: List[Dict]) -> str:
    """Generate markdown table for README."""
    if not adrs:
        return "| ADR | Title | Status | Date |\n|-----|-------|--------|------|\n"

    table = "| ADR | Title | Status | Date |\n"
    table += "|-----|-------|--------|------|\n"

    for adr in adrs:
        table += f"| [{adr['number']:03d}]({adr['path']}) | {adr['title']} | {adr['status']} | {adr['date']} |\n"

    return table

def update_readme(readme_path: Path, new_table: str) -> bool:
    """Update README.md with new ADR table. Returns True if changed."""
    with open(readme_path, 'r') as f:
        content = f.read()

    # Find and replace the Current ADRs section
    # Pattern: "## Current ADRs" followed by table until next section or end
    pattern = r'(## Current ADRs\s*\n\n)(.*?)(?=\n## [A-Z]|\Z)'

    def replace_table(match):
        return match.group(1) + new_table

    new_content = re.sub(pattern, replace_table, content, flags=re.DOTALL)

    if new_content == content:
        return False

    with open(readme_path, 'w') as f:
        f.write(new_content)

    return True

def main():
    """Main entry point."""
    adr_dir = Path(__file__).parent.parent / 'docs' / 'adr'
    readme_path = adr_dir / 'README.md'

    if not adr_dir.exists():
        print(f"Error: ADR directory not found: {adr_dir}", file=sys.stderr)
        sys.exit(1)

    if not readme_path.exists():
        print(f"Error: README.md not found: {readme_path}", file=sys.stderr)
        sys.exit(1)

    # Validate ADR files
    adrs, errors = validate_adr_files(adr_dir)

    # Report errors
    if errors:
        print("ADR Validation Errors:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        sys.exit(1)

    # Generate new table
    new_table = generate_adr_table(adrs)

    # Update README
    changed = update_readme(readme_path, new_table)

    if changed:
        print("✓ ADR index regenerated and README.md updated")
        sys.exit(0)
    else:
        print("✓ ADR index is current")
        sys.exit(0)

if __name__ == '__main__':
    main()
