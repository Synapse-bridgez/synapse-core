#!/usr/bin/env bash
# Merges all open PRs into their base branch (develop/feature/*)
# Conflicts are resolved by favoring the incoming PR branch (ours = PR, theirs = base)
# Token is loaded from ~/work/scripts/.env

set -uo pipefail

source ~/work/scripts/.env

REPO="Synapse-bridgez/synapse-core"
API="https://api.github.com/repos/${REPO}"
AUTH="Authorization: Bearer ${GITHUB_TOKEN}"

prs=$(curl -s -H "$AUTH" -H "Accept: application/vnd.github+json" \
  "${API}/pulls?state=open&per_page=100")

pr_count=$(echo "$prs" | jq 'length')
echo "Found ${pr_count} open PR(s)"
echo ""

echo "$prs" | jq -c '.[]' | while read -r pr; do
  number=$(echo "$pr" | jq -r '.number')
  title=$(echo "$pr" | jq -r '.title')
  author=$(echo "$pr" | jq -r '.user.login')
  body=$(echo "$pr" | jq -r '.body // ""')
  head=$(echo "$pr" | jq -r '.head.ref')
  base="develop"

  closes=$(echo "$body" | grep -ioP '(closes|fixes|resolves)\s+#\K[0-9]+' | head -1)
  closes_str=${closes:+"#${closes}"}
  closes_str=${closes_str:-"(none)"}

  echo "PR #${number}: ${title}"
  echo "  Submitted by : ${author}"
  echo "  Closes issue : ${closes_str}"
  echo "  Branch       : ${head} → ${base}"

  # Try GitHub API merge — update PR base to develop first, then merge
  curl -s -X PATCH -H "$AUTH" -H "Accept: application/vnd.github+json" -H "Content-Type: application/json" \
    "${API}/pulls/${number}" -d "{\"base\":\"develop\"}" >/dev/null

  response=$(curl -s -w "\n%{http_code}" \
    -X PUT -H "$AUTH" -H "Accept: application/vnd.github+json" -H "Content-Type: application/json" \
    "${API}/pulls/${number}/merge" \
    -d "{\"merge_method\":\"merge\",\"commit_title\":\"Merge PR #${number}: ${title}\"}")

  http_code=$(echo "$response" | tail -1)
  msg=$(echo "$response" | head -n -1 | jq -r '.message // "merged"')

  if [[ "$http_code" == "200" ]]; then
    echo "  Status       : ✅ Merged via API"
  else
    echo "  Status       : ⚠️  API failed (${msg}), attempting local merge..."

    # Fetch latest and merge locally, favoring PR branch on conflicts
    git fetch origin "${head}" "${base}" 2>/dev/null
    git checkout "${base}" 2>/dev/null || git checkout -b "${base}" "origin/${base}"
    git reset --hard "origin/${base}" 2>/dev/null

    if git merge "origin/${head}" -X theirs --no-edit \
      -m "Merge PR #${number}: ${title} [conflict resolved]" 2>/dev/null; then
      git push origin "${base}"
      echo "  Status       : ✅ Merged locally (conflicts resolved, ${head} favored)"
    else
      git merge --abort 2>/dev/null || true
      echo "  Status       : ❌ Could not merge — manual intervention needed"
    fi
  fi
  echo ""
done
