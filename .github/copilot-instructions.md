# CLAURST

## Project
- **Name**: claurst
- **Org**: AiFeatures (iAiFy)
- **Language**: Rust
- **Description**: Open-source multi-provider terminal coding agent built in Rust. Clean-room reimplementation of Claude Code behavior with TUI, plugin system, chat forking, and memory consolidation.

## Build / Test / Lint
- Build: `cargo build` (from project root or `src-rust/`)
- Test: `cargo test`
- Lint: `cargo clippy --workspace --all-targets -- -D warnings`
- Format: `cargo fmt`

## Conventions
- GPL-3.0 licensed
- Conventional Commits
- No telemetry or tracking

## CI/CD
- Uses shared workflows from `Ai-road-4-You/enterprise-ci-cd`
