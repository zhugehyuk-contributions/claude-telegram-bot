use crate::Result;

/// Initialize logging/tracing for the bot.
///
/// In "offline" sandbox builds we keep this as a no-op (feature disabled),
/// but the public API stays stable.
pub fn init(service_name: &str) -> Result<()> {
    let _ = service_name;

    #[cfg(feature = "tracing")]
    {
        use tracing_subscriber::{fmt, EnvFilter};

        // Default: info for our crates, warn for everything else.
        // Can be overridden with `RUST_LOG`.
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new(format!("info,ctb=info,ctb_core=info,{service_name}=info"))
        });

        fmt()
            .with_env_filter(filter)
            .with_target(false)
            .with_ansi(true)
            .init();
    }

    Ok(())
}
