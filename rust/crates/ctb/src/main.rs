use std::sync::Arc;

use ctb_claude_cli::ClaudeCliClient;

use ctb_core::{
    config::Config,
    model::types::{ClaudeCliConfig, PermissionMode},
    session::ClaudeSession,
};

#[tokio::main]
async fn main() -> Result<(), ctb_core::Error> {
    ctb_core::logging::init("ctb")?;

    let cfg = Arc::new(Config::load()?);
    if let Some(dir) = &cfg.claude_config_dir {
        std::env::set_var("CLAUDE_CONFIG_DIR", dir);
    }

    let model = Arc::new(ClaudeCliClient::new(ClaudeCliConfig {
        claude_path: cfg.claude_cli_path.clone(),
        model: None,
        permission_mode: PermissionMode::BypassPermissions,
        dangerously_skip_permissions: true,
        include_partial_messages: true,
    }));

    let session = Arc::new(ClaudeSession::new(cfg.clone(), model));

    ctb_telegram::router::run_polling(cfg, session)
        .await
        .map_err(|e| ctb_core::Error::External(format!("telegram bot failed: {e}")))?;

    Ok(())
}
