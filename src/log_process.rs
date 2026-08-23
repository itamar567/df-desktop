use std::ffi::OsStr;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::window::WindowId;

use crate::log::{LogSnapshot, LogTab, SharedLogState, new_shared_log_state};
use crate::log_window::{LogWindow, LogWindowAction};
use crate::player::RuffleEvent;

const CHILD_ARGUMENT: &str = "--dragonfable-log-window";
const MAX_MESSAGE_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogProcessEvent {
    Closed,
    TabSelected(LogTab),
    Exited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum ChildCommand {
    Close,
    Update(LogSnapshot),
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum ChildWindowEvent {
    Closed,
    TabSelected(LogTab),
}

#[derive(Serialize)]
struct MessageRef<'a, T> {
    payload: &'a T,
}

#[derive(Deserialize)]
struct Message<T> {
    payload: T,
}

#[derive(Default)]
struct PendingCommands {
    data: Option<LogSnapshot>,
    close: bool,
    shutdown: bool,
}

struct CommandOutbox {
    pending: Mutex<PendingCommands>,
    ready: Condvar,
}

impl CommandOutbox {
    fn new() -> Self {
        Self {
            pending: Mutex::new(PendingCommands::default()),
            ready: Condvar::new(),
        }
    }

    fn update(&self, data: LogSnapshot) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pending.data = Some(data);
        self.ready.notify_one();
    }

    fn close(&self) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pending.close = true;
        self.ready.notify_one();
    }

    fn shutdown(&self) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pending.shutdown = true;
        self.ready.notify_one();
    }

    fn take(&self) -> PendingCommands {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while pending.data.is_none() && !pending.close && !pending.shutdown {
            pending = self
                .ready
                .wait(pending)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        std::mem::take(&mut *pending)
    }
}

pub struct LogProcess {
    child: Child,
    outbox: Arc<CommandOutbox>,
    writer: Option<JoinHandle<()>>,
}

impl LogProcess {
    pub fn spawn(event_loop: EventLoopProxy<RuffleEvent>) -> Result<Self> {
        let mut child = Command::new(std::env::current_exe()?)
            .arg(CHILD_ARGUMENT)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("failed to start log window process")?;
        let Some(input) = child.stdin.take() else {
            stop_child(&mut child);
            anyhow::bail!("log process stdin unavailable");
        };
        let Some(output) = child.stdout.take() else {
            stop_child(&mut child);
            anyhow::bail!("log process stdout unavailable");
        };
        let mut output = BufReader::new(output);

        if let Err(error) = thread::Builder::new()
            .name("log-window-ipc-reader".to_string())
            .spawn(move || {
                while let Ok(event) = read_message::<ChildWindowEvent>(&mut output) {
                    let event = match event {
                        ChildWindowEvent::Closed => LogProcessEvent::Closed,
                        ChildWindowEvent::TabSelected(tab) => LogProcessEvent::TabSelected(tab),
                    };
                    if event_loop
                        .send_event(RuffleEvent::LogProcess(event))
                        .is_err()
                    {
                        return;
                    }
                }
                let _ = event_loop.send_event(RuffleEvent::LogProcess(LogProcessEvent::Exited));
            })
        {
            stop_child(&mut child);
            return Err(error.into());
        }

        let outbox = Arc::new(CommandOutbox::new());
        let writer_outbox = outbox.clone();
        let writer = match thread::Builder::new()
            .name("log-window-ipc-writer".to_string())
            .spawn(move || write_child_commands(input, &writer_outbox))
        {
            Ok(writer) => writer,
            Err(error) => {
                stop_child(&mut child);
                return Err(error.into());
            }
        };

        Ok(Self {
            child,
            outbox,
            writer: Some(writer),
        })
    }

    pub fn update(&self, data: LogSnapshot) {
        self.outbox.update(data);
    }

    pub fn close(&self) {
        self.outbox.close();
    }
}

impl Drop for LogProcess {
    fn drop(&mut self) {
        self.outbox.shutdown();
        stop_child(&mut self.child);
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

pub fn is_child_process() -> bool {
    std::env::args_os().nth(1).as_deref() == Some(OsStr::new(CHILD_ARGUMENT))
}

pub fn run_child() -> Result<()> {
    let event_loop = EventLoop::<ChildCommand>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    thread::Builder::new()
        .name("log-window-stdin".to_string())
        .spawn(move || read_child_commands(proxy))?;

    let mut app = LogChildApp::new();
    event_loop.run_app(&mut app)?;
    Ok(())
}

fn write_child_commands(input: impl Write, outbox: &CommandOutbox) {
    let mut input = BufWriter::new(input);
    loop {
        let mut pending = outbox.take();
        if pending.shutdown {
            let _ = write_message(&mut input, &ChildCommand::Shutdown);
            return;
        }
        if let Some(data) = pending.data.take()
            && write_message(&mut input, &ChildCommand::Update(data)).is_err()
        {
            return;
        }
        if pending.close && write_message(&mut input, &ChildCommand::Close).is_err() {
            return;
        }
    }
}

fn read_child_commands(event_loop: EventLoopProxy<ChildCommand>) {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    while let Ok(command) = read_message(&mut input) {
        if event_loop.send_event(command).is_err() {
            return;
        }
    }
    let _ = event_loop.send_event(ChildCommand::Shutdown);
}

struct LogChildApp {
    window: Option<LogWindow>,
    logs: SharedLogState,
    output: BufWriter<io::Stdout>,
}

impl LogChildApp {
    fn new() -> Self {
        Self {
            window: None,
            logs: new_shared_log_state(|| {}),
            output: BufWriter::new(io::stdout()),
        }
    }

    fn send_event(&mut self, event: ChildWindowEvent, event_loop: &ActiveEventLoop) {
        if write_message(&mut self.output, &event).is_err() {
            event_loop.exit();
        }
    }
}

impl ApplicationHandler<ChildCommand> for LogChildApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        match LogWindow::new(event_loop) {
            Ok(window) => {
                window.request_redraw();
                self.window = Some(window);
            }
            Err(error) => {
                eprintln!("Failed to create log window: {error}");
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, command: ChildCommand) {
        match command {
            ChildCommand::Close => event_loop.exit(),
            ChildCommand::Update(data) => {
                self.logs.replace(data);
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            ChildCommand::Shutdown => event_loop.exit(),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let action = {
            let Some(window) = &mut self.window else {
                return;
            };
            if window.id() != id {
                return;
            }
            window.handle_event(&event, &self.logs)
        };

        match action {
            None => {}
            Some(LogWindowAction::Close) => {
                self.send_event(ChildWindowEvent::Closed, event_loop);
                event_loop.exit();
            }
            Some(LogWindowAction::TabSelected(tab)) => {
                self.send_event(ChildWindowEvent::TabSelected(tab), event_loop);
            }
        }
    }
}

fn write_message<T: Serialize>(writer: &mut impl Write, payload: &T) -> io::Result<()> {
    let message = toml::to_string(&MessageRef { payload })
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if message.len() > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "IPC message exceeds size limit",
        ));
    }
    let length = u32::try_from(message.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "IPC message is too large"))?;
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(message.as_bytes())?;
    writer.flush()
}

fn read_message<T: DeserializeOwned>(reader: &mut impl Read) -> io::Result<T> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IPC message exceeds size limit",
        ));
    }
    let mut message = String::with_capacity(length);
    reader.take(length as u64).read_to_string(&mut message)?;
    if message.len() != length {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "incomplete IPC message",
        ));
    }
    toml::from_str::<Message<T>>(&message)
        .map(|message| message.payload)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn outbox_coalesces_log_updates_to_the_latest_data() {
        let outbox = CommandOutbox::new();
        let first = LogSnapshot {
            tab: LogTab::Game,
            content: "first".to_string(),
        };
        let latest = LogSnapshot {
            tab: LogTab::Battle,
            content: "battle".to_string(),
        };

        outbox.update(first);
        outbox.update(latest.clone());
        let pending = outbox.take();

        assert_eq!(pending.data, Some(latest));
    }

    #[test]
    fn child_commands_round_trip() {
        let data = LogSnapshot {
            tab: LogTab::Battle,
            content: "battle\nlog".to_string(),
        };
        let commands = [
            ChildCommand::Close,
            ChildCommand::Update(data),
            ChildCommand::Shutdown,
        ];

        for command in commands {
            let mut bytes = Vec::new();
            write_message(&mut bytes, &command).unwrap();
            assert_eq!(
                read_message::<ChildCommand>(&mut Cursor::new(bytes)).unwrap(),
                command
            );
        }
    }

    #[test]
    fn parent_events_round_trip() {
        for event in [
            ChildWindowEvent::Closed,
            ChildWindowEvent::TabSelected(LogTab::Game),
            ChildWindowEvent::TabSelected(LogTab::Battle),
        ] {
            let mut bytes = Vec::new();
            write_message(&mut bytes, &event).unwrap();
            assert_eq!(
                read_message::<ChildWindowEvent>(&mut Cursor::new(bytes)).unwrap(),
                event
            );
        }
    }

    #[test]
    fn oversized_message_is_rejected_before_allocation() {
        let bytes = Vec::from(((MAX_MESSAGE_BYTES + 1) as u32).to_le_bytes());

        let error = read_message::<ChildCommand>(&mut Cursor::new(bytes)).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn incomplete_message_is_rejected() {
        let mut bytes = Vec::from(10_u32.to_le_bytes());
        bytes.extend_from_slice(b"short");

        let error = read_message::<ChildCommand>(&mut Cursor::new(bytes)).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }
}
