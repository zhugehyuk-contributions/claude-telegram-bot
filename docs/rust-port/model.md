# Model Provider Abstraction

This note documents the Rust port’s model-provider abstraction.

## Where It Lives

- Core types: `rust/crates/ctb-core/src/model/types.rs`
- Core client trait + prompt adapter: `rust/crates/ctb-core/src/model/client.rs`
- Claude CLI adapter: `rust/crates/ctb-claude-cli/src/lib.rs`

## Design Goals

- Keep session/streaming logic independent of the backend (CLI vs HTTP API).
- Handle evolving schemas safely by preserving `raw` JSON in `ModelEvent`.
- Make feature support explicit via `ModelCapabilities`.
- Normalize resume/restore via `SessionRef { provider, id }`.

## Capability Matrix (Initial)

- Claude CLI (`ProviderKind::ClaudeCli`):
  - `supports_streaming`: yes (`--output-format=stream-json`)
  - `supports_tools`: yes (Claude Code tools)
  - `supports_vision`: yes (when sending images via supported CLI mechanisms)
  - `supports_thinking`: yes (configurable; may map to CLI flags or prompting depending on CLI version)
  - `supports_mcp`: yes (`--mcp-config`, MCP tools)

- Anthropic HTTP (future):
  - `supports_streaming`: yes
  - `supports_tools`: limited (no local filesystem tools; only API tool calling)
  - `supports_vision`: yes (model-dependent)
  - `supports_thinking`: yes (model-dependent)
  - `supports_mcp`: no

## Swapping Providers

1. Implement `ctb_core::model::client::ModelClient` for your backend.
2. Implement/extend a `PromptAdapter`:
   - CLI: build argv/env (`CliInvocation`)
   - HTTP: build JSON payload
3. Update the session runner wiring (later task agi-cnf.8/9) to select `ModelConfig`.
