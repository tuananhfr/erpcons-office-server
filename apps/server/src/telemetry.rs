use tracing_subscriber::EnvFilter;

/// `info` by default; `RUST_LOG` overrides. sqlx stays at `warn` so statements are not logged.
const DEFAULT_FILTER: &str = "info,sqlx=warn";

pub(crate) fn init() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .init();
}
