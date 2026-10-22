#!/usr/bin/env python3
"""Tally maintainer votes on an RFC pull request and post the status as a PR comment.

Vote convention (see "Voting and consensus" in docs/rfcs/README.md):
- A maintainer's latest APPROVED review is an approval.
- A maintainer's latest CHANGES_REQUESTED review is a blocking objection.
- COMMENTED reviews do not vote. A DISMISSED review no longer votes.
- Only current members of the maintainers team vote. The RFC author never votes.
  Votes from anyone else are listed as excluded, so a maintainer who leaves
  the project mid-vote stops counting immediately and visibly.

Outcomes:
- needs-revision: at least one objection is outstanding.
- accepted: at least ACCEPT_THRESHOLD approvals and no outstanding objections.
- open: anything else.

Rejected, Withdrawn and Deferred are terminal and are set by a maintainer in the
RFC header. This script reports them but never computes them.

Environment: GITHUB_TOKEN, GITHUB_REPOSITORY, PR_NUMBER and optionally
RFC_MAINTAINERS_TEAM (default: synapse-core/maintainers).
"""

import json
import os
import re
import sys
import urllib.request

API = "https://api.github.com"
COMMENT_MARKER = "<!-- rfc-vote-status -->"
RFC_DIR = "docs/rfcs"
NON_RFC_FILES = {"README.md", "000-template.md"}
ACCEPT_THRESHOLD = 1
TERMINAL_STATUSES = {"Rejected", "Withdrawn", "Deferred"}
STATUS_PATTERN = re.compile(r"^- \*\*Status:\*\*\s*(.+?)\s*$", re.MULTILINE)


def request(method, url, token, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        url,
        data=data,
        method=method,
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
        },
    )
    with urllib.request.urlopen(req) as resp:
        return json.load(resp)


def list_all(url, token):
    separator = "&" if "?" in url else "?"
    items, page = [], 1
    while True:
        batch = request("GET", f"{url}{separator}per_page=100&page={page}", token)
        items.extend(batch)
        if len(batch) < 100:
            return items
        page += 1


def tally(reviews, maintainers, author):
    latest = {}
    for review in sorted(reviews, key=lambda r: r.get("submitted_at") or ""):
        login = review["user"]["login"]
        if review["state"] == "DISMISSED":
            latest.pop(login, None)
        elif review["state"] in ("APPROVED", "CHANGES_REQUESTED"):
            latest[login] = review["state"]

    approvals, objections, excluded = [], [], []
    for login, state in sorted(latest.items()):
        if login == author or login not in maintainers:
            excluded.append(login)
        elif state == "APPROVED":
            approvals.append(login)
        else:
            objections.append(login)

    if objections:
        outcome = "needs-revision"
    elif len(approvals) >= ACCEPT_THRESHOLD:
        outcome = "accepted"
    else:
        outcome = "open"

    return {
        "outcome": outcome,
        "approvals": approvals,
        "objections": objections,
        "excluded": excluded,
        "approvals_needed": max(0, ACCEPT_THRESHOLD - len(approvals)),
    }


def rfc_status(path):
    with open(path, encoding="utf-8") as f:
        match = STATUS_PATTERN.search(f.read())
    return match.group(1) if match else "Unknown"


def render(name, status, result):
    if status in TERMINAL_STATUSES:
        outcome_line = f"**Terminal status:** {status} (set by a maintainer in the RFC header)"
    else:
        outcome_line = f"**Consensus:** `{result['outcome']}`"

    def names(logins):
        return ", ".join(f"@{login}" for login in logins) or "none"

    outstanding = []
    if result["objections"]:
        outstanding.append(f"resolve objections from {names(result['objections'])}")
    if result["approvals_needed"]:
        outstanding.append(
            f"{result['approvals_needed']} more maintainer approval(s) needed "
            f"(threshold: {ACCEPT_THRESHOLD})"
        )

    lines = [
        COMMENT_MARKER,
        f"### RFC vote status: `{name}`",
        "",
        f"- **RFC status:** {status}",
        f"- {outcome_line}",
        f"- **Approvals:** {len(result['approvals'])} ({names(result['approvals'])})",
        f"- **Objections:** {len(result['objections'])} ({names(result['objections'])})",
        f"- **Outstanding:** {'; '.join(outstanding) or 'nothing'}",
        f"- **Excluded from tally:** {names(result['excluded'])}",
        "",
        "_Maintainers vote with an approving or changes-requested review. "
        "Only current maintainers count; the author never does. "
        "See Voting and consensus in docs/rfcs/README.md._",
    ]
    return "\n".join(lines)


def main():
    token = os.environ["GITHUB_TOKEN"]
    repo = os.environ["GITHUB_REPOSITORY"]
    pr_number = os.environ["PR_NUMBER"]
    team = os.environ.get("RFC_MAINTAINERS_TEAM", "synapse-core/maintainers")
    org, team_slug = team.split("/", 1)

    files = list_all(f"{API}/repos/{repo}/pulls/{pr_number}/files", token)
    rfc_files = [
        f["filename"]
        for f in files
        if f["filename"].startswith(f"{RFC_DIR}/")
        and os.path.basename(f["filename"]) not in NON_RFC_FILES
        and f["status"] != "removed"
    ]
    if not rfc_files:
        print("No RFC files changed in this pull request; nothing to report.")
        return

    pr = request("GET", f"{API}/repos/{repo}/pulls/{pr_number}", token)
    author = pr["user"]["login"]
    maintainers = {
        m["login"]
        for m in list_all(f"{API}/orgs/{org}/teams/{team_slug}/members", token)
    }
    reviews = list_all(f"{API}/repos/{repo}/pulls/{pr_number}/reviews", token)

    sections = []
    for path in rfc_files:
        status = rfc_status(path)
        result = tally(reviews, maintainers, author)
        sections.append(render(os.path.basename(path), status, result))
    body = "\n\n".join(sections)

    comments = list_all(f"{API}/repos/{repo}/issues/{pr_number}/comments", token)
    existing = next((c for c in comments if COMMENT_MARKER in c["body"]), None)
    if existing:
        request("PATCH", f"{API}/repos/{repo}/issues/comments/{existing['id']}", token, {"body": body})
    else:
        request("POST", f"{API}/repos/{repo}/issues/{pr_number}/comments", token, {"body": body})
    print(body)


if __name__ == "__main__":
    sys.exit(main())
