# Changelog

All notable changes to this project are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/). This project uses [Semantic Versioning](https://semver.org/).

## [0.37.0] - 2026-03-17

### Removed

- `lessons-learned` skill — moved to claudius plugin, which now owns classification logic

### Changed

- `remember` skill — simplified to pure executor; classification logic removed (now handled by claudius)
- Updated CLAUDE.md and README.md references to reflect lessons-learned migration

## [0.36.0] - 2026-03-16

### Added

- Export, import, and remote code indexing
- Tool param signatures in skill MCP tables
- `allowed-tools` to lessons-learned skill

### Changed

- Native ARM64 Docker + conditional crates.io publish
- Set traefik v3 and ollama latest in docker compose
