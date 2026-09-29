//! Protocol debug traces can include credentials and authorization codes.
use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};

pub fn init() {
    let environment = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "babel_audio=info,babel=info,babel_tray=info".into());
    tracing_subscriber::registry()
        .with(environment)
        .with(
            tracing_subscriber::fmt::layer().with_filter(tracing_subscriber::filter::filter_fn(
                |metadata| {
                    // Independent of RUST_LOG: SDK traces include auth messages,
                    // and remote errors logged at warn may echo credentials too.
                    // Babel exposes sanitized diagnostics at its own boundary.
                    !metadata.target().starts_with("rmcp")
                },
            )),
        )
        .init();
}
