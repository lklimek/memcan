---
name: release
description: Bump version (SemVer 2.0), update Cargo.lock, generate changelog (Keep a Changelog), commit, push, and create GitHub release. Args: major|minor|patch or auto-detect from commits.
user-invocable: true
---

# Release

Bump version, commit, push, and create a GitHub release.

## Arguments

Optional first argument: `major`, `minor`, or `patch`. If omitted, auto-detect from git history (see Step 1).

## Steps

### 1. Determine New Version

1. Read current version from `Cargo.toml` workspace `[workspace.package] version` field.
2. Parse as SemVer 2.0 (MAJOR.MINOR.PATCH).
3. Get commits since last tag:
   ```bash
   git log $(git describe --tags --abbrev=0 2>/dev/null || git rev-list --max-parents=0 HEAD)..HEAD --oneline --no-decorate
   ```
4. If no bump type argument provided, auto-detect from commit prefixes:
   - **major**: any commit contains `BREAKING CHANGE` in body, or type ends with `!` (e.g., `feat!:`, `fix!:`)
   - **minor**: any `feat:` or `feat(scope):` commit
   - **patch**: only `fix:`, `perf:`, `refactor:`, `chore:`, `docs:`, `test:`, `build:`, `ci:`, `style:`
   - If no conventional commits found, default to `patch`
5. Apply bump:
   - `major` → MAJOR+1.0.0
   - `minor` → MAJOR.MINOR+1.0
   - `patch` → MAJOR.MINOR.PATCH+1
6. Print: `Bumping version: {old} → {new} ({bump_type}, {reason})`

### 2. Update Version

Update version in exactly two places (keep in sync):
1. `Cargo.toml` — `[workspace.package] version = "{new}"`
2. `.claude-plugin/plugin.json` — `"version": "{new}"`

Then run `cargo update --workspace` to sync `Cargo.lock`.

### 3. Generate Changelog

Format per [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

Parse commits from Step 1 and map conventional commit types to changelog sections:

| Commit prefix | Changelog section |
|---|---|
| `feat` | Added |
| `fix` | Fixed |
| `perf` | Changed |
| `refactor` | Changed |
| `docs` | Changed |
| `chore`, `ci`, `build`, `test`, `style` | Other |
| `BREAKING CHANGE` or `!` suffix | separate "BREAKING" note at top |

Write to a temp file. Format:

```markdown
## [X.Y.Z] - YYYY-MM-DD

### BREAKING
- description (hash)

### Added
- description (hash)

### Fixed
- description (hash)

### Changed
- description (hash)
```

Omit empty sections. Strip the type prefix and optional scope from descriptions (e.g., `feat(cli): add foo` → `add foo`). If a commit has no conventional prefix, put it in Changed.

### 4. Commit and Push

```bash
git add Cargo.toml Cargo.lock .claude-plugin/plugin.json
git commit -m "chore: release v{new}"
git push origin main
```

Verify push succeeds before proceeding.

### 5. Create GitHub Release

```bash
gh release create v{new} --target main --title "v{new}" --notes-file {changelog_temp_file}
```

Print the release URL when done.

### 6. Summary

Print:
- Version: {old} → {new}
- Release URL
- Triggered workflows: Release (binaries), Publish (crates.io), Docker (image)

## Constraints

- NEVER skip the Cargo.lock update — `cargo publish --locked` will fail otherwise.
- NEVER create the release before pushing — the release must reference a commit that exists on the remote.
- If any step fails, stop and report the error. Do not continue with partial state.
