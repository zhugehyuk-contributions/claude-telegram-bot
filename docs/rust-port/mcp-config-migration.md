# MCP Config Migration (TS → Rust)

The TypeScript bot loads MCP servers from `mcp-config.ts` (dynamic import of `MCP_SERVERS`).

For the Rust port, we use a JSON file that matches Claude’s MCP schema so it can be:
- parsed by Rust (`ctb_core::mcp_config::load_mcp_servers`)
- passed directly to the CLI via `claude --mcp-config <file>`

## New File

- `mcp-config.example.json` (copy to `mcp-config.json` locally; do not commit secrets)

## Mapping

TypeScript (`mcp-config.ts`):
```ts
export const MCP_SERVERS = {
  "ask-user": { command: "bun", args: ["run", "./ask_user_mcp/server.ts"] },
  "typefully": { type: "http", url: "https://...${TYPEFULLY_API_KEY}" }
}
```

Rust (`mcp-config.json`):
```json
{
  "ask-user": { "command": "${CTB_REPO_ROOT}/rust/target/release/ctb-ask-user-mcp", "args": [], "env": {} },
  "typefully": { "type": "http", "url": "https://...${TYPEFULLY_API_KEY}", "headers": {} }
}
```

## Env Interpolation

The Rust loader interpolates `${ENV_VAR}` placeholders in all string fields.
- Unset variables become empty strings.

## Notes

- Keep `mcp-config.json` out of git (treat like `.env`).
- If an MCP server needs secrets, prefer environment variables over hardcoding.
- The Rust bot injects `CTB_REPO_ROOT` automatically for config interpolation.
- The Rust bot also injects `TELEGRAM_CHAT_ID` into the `ask-user` server’s env per chat/run.
