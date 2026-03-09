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

1. Read current version from `Cargo.toml` workspace `[workspace.package] version` field. Parse as SemVer 2.0.

2. Get commits since last tag:
   ```bash
   git log $(git describe --tags --abbrev=0 2>/dev/null || git rev-list --max-parents=0 HEAD)..HEAD --oneline --no-decorate
   ```

3. **Investigate changes in detail.** For each commit, examine the actual diff to understand the real impact — commit prefixes alone can be misleading. Run:
   ```bash
   git diff $(git describe --tags --abbrev=0 2>/dev/null || git rev-list --max-parents=0 HEAD)..HEAD --stat
   ```
   Then read the full diff for any commits that touch public APIs, MCP tool signatures, config formats, or data schemas. Look for:
   - **Breaking**: removed/renamed MCP tools, changed tool parameter names/types, removed config vars, changed data formats, removed public API functions
   - **Minor**: new MCP tools, new config vars, new CLI subcommands, new skills/agents, significant new behavior
   - **Patch**: bug fixes, doc changes, internal refactors, CI/build changes, typos

4. If bump type was provided as argument, use it. Otherwise auto-detect using both commit prefixes AND the diff investigation:
   - **major**: breaking changes found in diffs, OR any commit contains `BREAKING CHANGE` in body, OR type ends with `!`
   - **minor**: new features/capabilities found in diffs, OR any `feat:` commit
   - **patch**: only fixes, refactors, docs, CI, or trivial changes
   - If no conventional commits found, decide from diff analysis; default to `patch` if unclear

5. Apply bump: `major` → X+1.0.0, `minor` → X.Y+1.0, `patch` → X.Y.Z+1

6. **Present analysis and ask for confirmation.** Use `AskUserQuestion` to show:
   - Current version → proposed version (bump type)
   - Commit list with short descriptions
   - Key changes found in diff investigation (files changed, what was added/removed/modified)
   - Justification for the chosen bump type
   - Options: proposed bump type (recommended), alternative bump types, or abort

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
