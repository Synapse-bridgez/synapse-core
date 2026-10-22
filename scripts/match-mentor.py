#!/usr/bin/env python3
"""Pick a mentor for a Wave issue from the CODEOWNERS of the paths it touches.

CODEOWNERS is the single source of expertise, so there is no second map to
drift out of sync. Paths are pulled from the issue body and matched against
.github/CODEOWNERS (last matching rule wins, as on GitHub). Team owners are
expanded to members through the GitHub API.

Usage:
  match-mentor.py --issue-number N --issue-body-file FILE --assignee LOGIN
                  [--exclude LOGIN,LOGIN] [--codeowners PATH] [--repo-root DIR]

Prints a JSON object: {"mentor", "team", "paths", "reason"}. "mentor" is null
when no eligible mentor exists; "reason" then explains why.

Auth: MENTOR_MATCH_TOKEN (preferred) or GITHUB_TOKEN. Listing team members
needs read:org, which the default GITHUB_TOKEN does not have.
"""

import argparse
import fnmatch
import json
import os
import re
import sys
import urllib.error
import urllib.request
from collections import Counter

API = "https://api.github.com"
PATH_TOKEN_RE = re.compile(r"[\w.\-]+(?:/[\w.\-]*)+")


class MentorLookupError(Exception):
    pass


def parse_codeowners(text):
    rules = []
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        pattern, *owners = line.split()
        if owners:
            rules.append((pattern, owners))
    return rules


def pattern_matches(pattern, path):
    if pattern == "*":
        return True
    anchored = pattern.lstrip("/")
    if pattern.endswith("/"):
        return path.startswith(anchored)
    return fnmatch.fnmatchcase(path, anchored) or path.startswith(anchored + "/")


def owners_for(rules, path):
    owners = []
    for pattern, rule_owners in rules:
        if pattern_matches(pattern, path):
            owners = rule_owners
    return owners


def extract_paths(body, repo_root):
    paths = []
    for token in PATH_TOKEN_RE.findall(body):
        candidate = token.strip("/")
        full = os.path.join(repo_root, candidate)
        if not candidate or candidate in paths or not os.path.exists(full):
            continue
        if os.path.isdir(full):
            candidate += "/"
        paths.append(candidate)
    return paths


def list_team_members(org, slug, token):
    members = []
    page = 1
    while True:
        url = f"{API}/orgs/{org}/teams/{slug}/members?per_page=100&page={page}"
        headers = {"Accept": "application/vnd.github+json"}
        if token:
            headers["Authorization"] = f"Bearer {token}"
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers=headers)) as resp:
                batch = json.load(resp)
        except (urllib.error.HTTPError, urllib.error.URLError) as err:
            raise MentorLookupError(f"could not list members of {org}/{slug}: {err}")
        if not batch:
            return members
        members.extend(member["login"] for member in batch)
        page += 1


def pick_mentor(candidates, issue_number, excluded):
    if not candidates:
        return None
    # Rotate by issue number so different issues spread across the team.
    start = issue_number % len(candidates)
    ordered = candidates[start:] + candidates[:start]
    for login in ordered:
        if login.lower() not in excluded:
            return login
    return None


def match(rules, paths, issue_number, excluded, token):
    if paths:
        owner_counts = Counter()
        for path in paths:
            for owner in owners_for(rules, path):
                owner_counts[owner] += 1
        owner_handles = [owner for owner, _ in sorted(owner_counts.items(), key=lambda item: (-item[1], item[0]))]
    else:
        owner_handles = [owner for pattern, owners in rules if pattern == "*" for owner in owners]

    if not owner_handles:
        return {"mentor": None, "team": None, "paths": paths, "reason": "no CODEOWNERS owner covers this issue"}

    handle = owner_handles[0]
    if "/" not in handle:
        candidates = [handle.lstrip("@")]
        team = None
    else:
        org, slug = handle.lstrip("@").split("/", 1)
        team = handle.lstrip("@")
        try:
            candidates = list_team_members(org, slug, token)
        except MentorLookupError as err:
            return {"mentor": None, "team": team, "paths": paths, "reason": str(err)}

    mentor = pick_mentor(candidates, issue_number, excluded)
    reason = None if mentor else f"no available mentor in {team or handle} after exclusions"
    return {"mentor": mentor, "team": team, "paths": paths, "reason": reason}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--issue-number", type=int, required=True)
    parser.add_argument("--issue-body-file", required=True)
    parser.add_argument("--assignee", required=True)
    parser.add_argument("--exclude", default="", help="comma-separated logins that must not be picked")
    parser.add_argument("--codeowners", default=".github/CODEOWNERS")
    parser.add_argument("--repo-root", default=".")
    args = parser.parse_args()

    with open(os.path.join(args.repo_root, args.codeowners), encoding="utf-8") as f:
        rules = parse_codeowners(f.read())
    with open(args.issue_body_file, encoding="utf-8") as f:
        paths = extract_paths(f.read(), args.repo_root)

    excluded = {login.strip().lower() for login in args.exclude.split(",") if login.strip()}
    excluded.add(args.assignee.lower())
    token = os.environ.get("MENTOR_MATCH_TOKEN") or os.environ.get("GITHUB_TOKEN", "")

    result = match(rules, paths, args.issue_number, excluded, token)
    json.dump(result, sys.stdout, indent=2)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
