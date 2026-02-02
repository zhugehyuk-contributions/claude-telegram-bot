# Messaging Abstraction (Ports & Adapters)

This note documents the cross-messenger abstraction used by the Rust port.

## Where It Lives

- Core types (provider-agnostic): `rust/crates/ctb-core/src/messaging/types.rs`
- Port (trait): `rust/crates/ctb-core/src/messaging/port.rs`
- Telegram adapter (initial): `rust/crates/ctb-telegram/src/lib.rs`

## Design Goals

- Keep application logic independent of Telegram/teloxide.
- Make “missing features” explicit via `MessagingCapabilities` (instead of ad-hoc `cfg`s).
- Allow future adapters to be added without rewriting session/streaming logic.

## Adding A New Adapter (Slack/Discord/WhatsApp)

1. Implement `ctb_core::messaging::port::MessagingPort`.
2. Map `ChatAction`, `InlineKeyboard`, and HTML/text formatting to the platform’s supported subset.
3. Return accurate `MessagingCapabilities`:
   - `supports_edit`: if the platform can edit existing messages.
   - `supports_reactions`: if reactions exist and are available to bots.
   - `supports_chat_actions`: if typing/“uploading” indicators exist.
   - `supports_inline_keyboards`: if clickable buttons/callbacks are supported.
4. In the router layer:
   - Convert platform-specific updates into `IncomingUpdate` (or a richer platform adapter type),
   - Then call core handlers with the port implementation.

## Telegram Notes

Telegram HTML has strict rules; all user/model text must be HTML-escaped and converted by:
- `ctb_core::formatting::convert_markdown_to_html`

