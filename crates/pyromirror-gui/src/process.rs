//! Runs `pyromirror-server` / `pyromirror-client` as child processes and collects their log.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use eframe::egui;

const MAX_LOG_LINES: usize = 200;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Clone)]
pub struct LogLine {
    pub level: Level,
    pub text: String,
}

/// Splits env_logger's `[<time> <LEVEL> <module>] message` into level and message.
pub fn parse_line(line: &str) -> LogLine {
    let (level, text) = match line.strip_prefix('[').and_then(|rest| rest.split_once("] ")) {
        Some((header, message)) => {
            let level = if header.contains(" ERROR ") {
                Level::Error
            } else if header.contains(" WARN ") {
                Level::Warn
            } else {
                Level::Info
            };
            (level, message)
        }
        // Anything else is an error report from anyhow, a panic, or the loader.
        None => (if line.starts_with("Error") { Level::Error } else { Level::Info }, line),
    };
    LogLine { level, text: text.trim_end().to_owned() }
}

pub struct Process {
    child: Child,
    log: Arc<Mutex<VecDeque<LogLine>>>,
}

fn sibling(name: &str) -> PathBuf {
    let file = format!("{}{}", name, std::env::consts::EXE_SUFFIX);
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(&file)))
        .filter(|path| path.exists())
        // Fall back to PATH.
        .unwrap_or_else(|| PathBuf::from(file))
}

impl Process {
    /// Starts the program `name` that lives next to this executable.
    pub fn spawn(name: &str, args: &[String], ctx: &egui::Context) -> std::io::Result<Self> {
        let mut command = Command::new(sibling(name));
        command.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn()?;

        let log = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(stderr) = child.stderr.take() {
            let (log, ctx) = (log.clone(), ctx.clone());
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let mut log = log.lock().unwrap();
                    if log.len() == MAX_LOG_LINES {
                        log.pop_front();
                    }
                    log.push_back(parse_line(&line));
                    drop(log);
                    ctx.request_repaint();
                }
                // The process has exited (or closed stderr); let the UI notice.
                ctx.request_repaint();
            });
        }
        Ok(Self { child, log })
    }

    /// `Some(success)` once the process has exited.
    pub fn exited(&mut self) -> Option<bool> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.success()),
            Ok(None) => None,
            Err(_) => Some(false),
        }
    }

    pub fn log(&self) -> Vec<LogLine> {
        self.log.lock().unwrap().iter().cloned().collect()
    }

    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_logger_and_plain_lines() {
        let line = parse_line("[2026-10-03T16:00:41Z WARN  pyromirror_server] no audio; streaming without");
        assert!(line.level == Level::Warn && line.text == "no audio; streaming without");
        let line = parse_line("[2026-10-03T16:00:41Z INFO  pyromirror_server] Listening on 0.0.0.0:9000");
        assert!(line.level == Level::Info && line.text.starts_with("Listening"));
        let line = parse_line("Error: could not start desktop capture");
        assert!(line.level == Level::Error);
        let line = parse_line("    capture initialization failed: [x] y");
        assert!(line.level == Level::Info && line.text.contains("[x] y"));
    }
}
