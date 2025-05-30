use anyhow::Result;
use std::path::Path;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Initialize logging system with both file and console output
pub fn init_logging(log_dir: &Path) -> Result<()> {
    // Create log directory if it doesn't exist
    std::fs::create_dir_all(log_dir)?;

    // Create file appender for detailed logs
    let file_appender = RollingFileAppender::new(
        Rotation::DAILY,
        log_dir,
        "nhi.log"
    );

    // Create console layer with simplified output
    let console_layer = fmt::layer()
        .with_target(false)
        .with_thread_ids(false)
        .with_thread_names(false)
        .with_file(false)
        .with_line_number(false)
        .compact();

    // Create file layer with detailed output
    let file_layer = fmt::layer()
        .with_writer(file_appender)
        .with_target(true)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true)
        .json();

    // Set up environment filter
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    // Initialize subscriber with both layers
    tracing_subscriber::registry()
        .with(env_filter)
        .with(console_layer)
        .with(file_layer)
        .init();

    Ok(())
}

/// Get the default log directory
pub fn default_log_dir() -> std::path::PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("logs")
}
