---
job: release
checks: [version_bumped, changelog_complete, tests_pass]
---
# Release
1. Verify merged PRs and passing release checks.
2. Bump the version consistently across package metadata.
3. Add a changelog entry for every merged PR.
4. Create the version tag and verify the published artifact.
