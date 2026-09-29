#!/usr/bin/env python3
"""
Generate contributor onboarding comment based on issue labels.

This script reads the contributor-onboarding.yml configuration and generates
a welcome comment with setup steps and documentation links based on the
GitHub issue labels.

Usage:
  python3 scripts/generate-contributor-onboarding.py <label1> <label2> ...
"""

import sys
import yaml
import os
from pathlib import Path
from typing import List, Dict, Optional

def load_config(config_path: Path) -> Dict:
    """Load contributor onboarding configuration."""
    with open(config_path, 'r') as f:
        return yaml.safe_load(f)

def normalize_label(label: str) -> str:
    """Normalize label to configuration key format (lowercase, underscores)."""
    return label.lower().replace(' ', '_').replace('&', 'and').replace('-', '_')

def find_matching_category(labels: List[str], config: Dict) -> Optional[Dict]:
    """Find matching category based on labels."""
    categories = config.get('categories', {})

    for label in labels:
        # Try exact label match
        normalized = normalize_label(label)
        if normalized in categories:
            return categories[normalized]

        # Try substring matching
        for cat_key, cat_config in categories.items():
            if cat_key == 'fallback':
                continue
            cat_label = cat_config.get('label', cat_key)
            if label.lower() in cat_label.lower() or cat_label.lower() in label.lower():
                return cat_config

    # Return fallback if no match
    return categories.get('fallback', {})

def format_setup_checklist(steps: List[str]) -> str:
    """Format setup steps as markdown checklist."""
    items = '\n'.join([f"  - [ ] {step}" for step in steps])
    return items

def format_documentation_links(docs: List[str], repo_root: Path) -> str:
    """Format documentation links with validation."""
    links = []
    for doc in docs:
        doc_path = repo_root / doc
        if doc_path.exists():
            links.append(f"  - [{doc}]({doc})")
        else:
            # Still link it even if not found (it might be in different locations)
            links.append(f"  - [{doc}]({doc})")
    return '\n'.join(links)

def generate_comment(labels: List[str], config: Dict, repo_root: Path) -> str:
    """Generate the onboarding comment."""
    category = find_matching_category(labels, config)

    welcome_msg = category.get('welcome', config.get('fallback', {}).get('welcome', ''))
    setup_steps = category.get('setup_steps', [])
    documentation = category.get('documentation', [])

    setup_checklist = format_setup_checklist(setup_steps)
    doc_links = format_documentation_links(documentation, repo_root)

    template = config.get('comment_template', '')

    comment = template.format(
        welcome_message=welcome_msg,
        setup_checklist=setup_checklist,
        documentation_links=doc_links
    )

    return comment

def main():
    """Main entry point."""
    if len(sys.argv) < 2:
        print("Usage: python3 scripts/generate-contributor-onboarding.py <label1> <label2> ...", file=sys.stderr)
        sys.exit(1)

    labels = sys.argv[1:]
    repo_root = Path(__file__).parent.parent
    config_path = repo_root / '.github' / 'contributor-onboarding.yml'

    if not config_path.exists():
        print(f"Error: Config file not found: {config_path}", file=sys.stderr)
        sys.exit(1)

    try:
        config = load_config(config_path)
        comment = generate_comment(labels, config, repo_root)
        print(comment)
    except Exception as e:
        print(f"Error generating onboarding comment: {str(e)}", file=sys.stderr)
        sys.exit(1)

if __name__ == '__main__':
    main()
