#!/bin/bash

# Script to set up the feature branch for issue-80

echo "Creating feature branch..."
git checkout -b feature/issue-80-admin-auth-tests

echo "Adding all files..."
git add .

echo "Committing changes..."
git commit -m "Add admin API authentication tests

- Implement JWT-based authentication middleware
- Add admin handlers with role-based access control
- Create comprehensive test suite covering:
  - Valid credentials
  - Invalid credentials
  - Missing credentials
  - Expired tokens
  - Role-based authorization

Resolves issue #80"

echo ""
echo "Branch created and committed successfully!"
echo ""
echo "To push to remote, run:"
echo "  git push origin feature/issue-80-admin-auth-tests"
echo ""
echo "Then create a Pull Request targeting the 'develop' branch."
