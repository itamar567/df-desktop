#![windows_subsystem = "windows"]

mod app;
mod config;
mod input;
mod log;
mod log_process;
mod log_ui;
mod log_window;
mod migration;
mod navigator;
mod player;
mod theme;
mod ui;

/// GL avoids the Vulkan memory leak currently affecting the Ruffle renderer.
pub(crate) const GRAPHICS_BACKENDS: wgpu::Backends = wgpu::Backends::GL;

use std::panic::Location;

use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;
use winit::event_loop::EventLoop;

fn main() -> anyhow::Result<()> {
    if log_process::is_child_process() {
        return log_process::run_child();
    }

    init_logging();
    install_panic_hook();

    let event_loop = EventLoop::<crate::player::RuffleEvent>::with_user_event().build()?;
    let mut app = app::App::new(&event_loop)?;
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// Logs panics to the file log (and stderr, via the default hook) so a crash
/// leaves evidence even when the process aborts. Installed after logging is up.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        tracing::error!("{}", format_panic(info.payload_as_str(), info.location()));
    }));
}

/// Formats a panic for the file log: the payload message plus the panic site.
/// Kept separate from the hook so tests can drive it without panicking.
fn format_panic(payload: Option<&str>, location: Option<&Location<'_>>) -> String {
    let mut message = format!("PANIC: {}", payload.unwrap_or("panic occurred"));
    if let Some(location) = location {
        message.push_str(&format!(
            "\n  at {}:{}:{}",
            location.file(),
            location.line(),
            location.column()
        ));
    }
    message
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("warn,ruffle=info,dragonfable_cache=info"));

    std::fs::create_dir_all(config::log_dir()).expect("log dir must be creatable");
    let (file_writer, guard) = tracing_appender::non_blocking(
        std::fs::File::create(config::log_dir().join("log.txt"))
            .expect("log file must be creatable"),
    );
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr).with_ansi(false))
        .with(tracing_subscriber::fmt::layer().with_writer(file_writer).with_ansi(false))
        .init();
    // The non-blocking writer's worker guard must outlive `main` (it flushes
    // the queue on drop); leaking it keeps the log writer alive for the whole
    // program, which is essential on Windows where there is no console.
    std::mem::forget(guard);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_panic_includes_message_and_location() {
        let formatted = format_panic(Some("boom"), Some(Location::caller()));
        assert!(formatted.starts_with("PANIC: boom"));
        assert!(formatted.contains("at src/main.rs:"), "got: {formatted}");
    }

    #[test]
    fn format_panic_falls_back_without_payload_or_location() {
        let formatted = format_panic(None, None);
        assert_eq!(formatted, "PANIC: panic occurred");
    }
}
