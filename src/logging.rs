//! Protocol debug traces can include credentials and authorization codes.
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

pub fn init() {
    let environment = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "babel_audio=info,babel=info,babel_tray=info".into());
    tracing_subscriber::registry()
        .with(environment)
        .with(tracing_subscriber::fmt::layer())
        .init();
}
