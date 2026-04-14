# Claurst

Open-source, multi-provider terminal coding agent built in Rust. Clean-room reimplementation of Claude Code behavior with TUI interface, plugin system, multi-provider support, chat forking, and memory consolidation.

## Tech Stack

- **Language**: Rust (workspace in `src-rust/`)
- **License**: GPL-3.0
- **Build**: `cargo build --release --package claurst` (from `src-rust/`)

## Project Structure

```text
src-rust/           Rust workspace (Cargo.toml here)
spec/               Behavioral specification
docs/               Documentation
public/             Static assets (screenshots, logo)
```

## Development

```bash
cd src-rust
cargo build --release --package claurst
cargo test
```

## Key Concepts

- Multi-provider: Anthropic, OpenAI, Google, GitHub Copilot, Ollama, DeepSeek, Groq, Mistral, 30+ more
- Managed Agents: Manager-Executor pattern via `/managed-agents`
- No telemetry or tracking
