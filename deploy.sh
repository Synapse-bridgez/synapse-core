#!/bin/bash

# Deployment script for Redis Idempotency feature
# This script pushes the feature branch to GitHub

set -e

# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

echo -e "${BLUE}╔════════════════════════════════════════════════════════╗${NC}"
echo -e "${BLUE}║  Redis Idempotency Feature - Deployment Script        ║${NC}"
echo -e "${BLUE}╚════════════════════════════════════════════════════════╝${NC}"
echo ""

# Check we're on the right branch
CURRENT_BRANCH=$(git branch --show-current)
EXPECTED_BRANCH="feature/issue-11-redis-idempotency"

if [ "$CURRENT_BRANCH" != "$EXPECTED_BRANCH" ]; then
    echo -e "${RED}❌ Error: Not on the correct branch${NC}"
    echo "   Current: $CURRENT_BRANCH"
    echo "   Expected: $EXPECTED_BRANCH"
    exit 1
fi

echo -e "${GREEN}✓ On correct branch: $CURRENT_BRANCH${NC}"

# Check for uncommitted changes
if ! git diff-index --quiet HEAD --; then
    echo -e "${RED}❌ Error: You have uncommitted changes${NC}"
    echo "   Please commit or stash your changes first"
    git status --short
    exit 1
fi

echo -e "${GREEN}✓ No uncommitted changes${NC}"

# Show what will be pushed
echo ""
echo -e "${BLUE}📦 Commits to be pushed:${NC}"
git log origin/main..HEAD --oneline --decorate
echo ""

# Count commits
COMMIT_COUNT=$(git rev-list --count origin/main..HEAD)
echo -e "${BLUE}📊 Summary:${NC}"
echo "   Commits: $COMMIT_COUNT"
echo "   Branch: $CURRENT_BRANCH"
echo "   Remote: origin"
echo "   Target PR branch: develop"
echo ""

# Show file changes
echo -e "${BLUE}📝 Files changed:${NC}"
git diff --stat origin/main..HEAD
echo ""

# Confirm push
echo -e "${YELLOW}⚠️  Ready to push to GitHub${NC}"
read -p "Do you want to continue? (y/N): " -n 1 -r
echo ""

if [[ ! $REPLY =~ ^[Yy]$ ]]; then
    echo -e "${RED}❌ Deployment cancelled${NC}"
    exit 1
fi

# Push to GitHub
echo ""
echo -e "${BLUE}🚀 Pushing to GitHub...${NC}"
git push origin "$CURRENT_BRANCH"

if [ $? -eq 0 ]; then
    echo ""
    echo -e "${GREEN}╔════════════════════════════════════════════════════════╗${NC}"
    echo -e "${GREEN}║  ✅ Successfully pushed to GitHub!                     ║${NC}"
    echo -e "${GREEN}╚════════════════════════════════════════════════════════╝${NC}"
    echo ""
    echo -e "${BLUE}📋 Next Steps:${NC}"
    echo ""
    echo "1. Create Pull Request:"
    echo "   https://github.com/Synapse-bridgez/synapse-core/compare/develop...$CURRENT_BRANCH"
    echo ""
    echo "2. Or use GitHub CLI:"
    echo "   gh pr create --base develop --head $CURRENT_BRANCH \\"
    echo "     --title \"feat: implement Redis-based webhook idempotency (issue #11)\" \\"
    echo "     --body-file PR_DESCRIPTION.md"
    echo ""
    echo "3. Add labels: enhancement, redis, idempotency, phase-1"
    echo ""
    echo "4. Request reviewers from the team"
    echo ""
    echo -e "${BLUE}📚 Documentation:${NC}"
    echo "   - PR Description: PR_DESCRIPTION.md"
    echo "   - Implementation: IMPLEMENTATION_NOTES.md"
    echo "   - Testing: test-idempotency.sh"
    echo "   - Full guide: docs/idempotency.md"
    echo ""
    echo -e "${GREEN}🎉 Great work! The feature is ready for review.${NC}"
else
    echo ""
    echo -e "${RED}❌ Push failed!${NC}"
    echo "   Please check your GitHub credentials and network connection"
    exit 1
fi
