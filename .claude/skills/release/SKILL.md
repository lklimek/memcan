---
name: release
description: Bump version (SemVer), update Cargo.lock, generate changelog, commit, push, and create GitHub release. Args: major|minor|patch (default: patch).
user-invocable: true
allowed-tools: Read, Edit, Bash(cargo *), Bash(git *), Bash(gh *), Bash(grep *), Bash(cat *), Glob, Grep
---

# Release

Bump version, commit, push, and create a GitHub release.

## Arguments

First argument: bump type -- `major`, `minor`, or `patch` (default: `patch`).

## Steps

### 1. Determine New Version

1. Read current version from `Cargo.toml` workspace `[workspace.package] version` field.
2. Parse as SemVer (MAJOR.MINOR.PATCH).
3. Apply bump based on argument:
   - `major` -> MAJOR+1.0.0
   - `minor` -> MAJOR.MINOR+1.0
   - `patch` -> MAJOR.MINOR.PATCH+1
4. Print: `Bumping version: {old} -> {new}`

### 2. Update Version

Update version in exactly two places (keep in sync):
1. `Cargo.toml` -- `[workspace.package] version = "{new}"`
2. `.claude-plugin/plugin.json` -- `"version": "{new}"`

Then run `cargo update --workspace` to sync `Cargo.lock`.

### 3. Generate Changelog

Generate a changelog from commits since the last tag:

```bash
git log $(git describe --tags --abbrev=0 2>/dev/null || git rev-list --max-parents=0 HEAD)..HEAD --oneline --no-decorate
```

Group commits by type prefix (feat, fix, refactor, chore, perf, docs, test). Format as markdown:

```
## What's Changed

### ✨ Features
- description (hash)

### 🐛 Bug Fixes
- description (hash)

### ⚡ Performance
- description (hash)

### 🔧 Maintenance
- description (hash)
```

Omit empty sections. Strip the type prefix from descriptions (e.g., `feat: add foo` -> `add foo`). Write this to a temp file for use in step 5.

### 4. Commit and Push

```bash
git add Cargo.toml Cargo.lock .claude-plugin/plugin.json
git commit -m "chore: release v{new}"
git push origin main
```

Verify push succeeds before proceeding.

### 5. Create GitHub Release

```bash
gh release create v{new} --repo lklimek/memcan --target main --title "v{new}" --notes-file {changelog_temp_file}
```

Print the release URL when done.

### 6. Summary

Print:
- Version: {old} -> {new}
- Release URL
- Triggered workflows: Release (binaries), Publish (crates.io), Docker (image)

## Constraints

- NEVER skip the Cargo.lock update -- `cargo publish --locked` will fail otherwise.
- NEVER create the release before pushing -- the release must reference a commit that exists on the remote.
- If any step fails, stop and report the error. Do not continue with partial state.
