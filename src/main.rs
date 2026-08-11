#![windows_subsystem = "windows"]

mod app;
mod config;
mod input;
mod migration;
mod navigator;
mod player;
mod ui;

use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;
use winit::event_loop::EventLoop;

fn main() -> anyhow::Result<()> {
    init_logging();

    let event_loop = EventLoop::<crate::player::RuffleEvent>::with_user_event().build()?;
    let mut app = app::App::new(&event_loop)?;
    event_loop.run_app(&mut app)?;
    Ok(())
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
