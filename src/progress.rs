/// Interactive terminal progress feedback.
/// Writes spinner + status to stderr when TTY, silent when piped.

use std::io::{self, IsTerminal, Write};

pub struct ProgressReporter {
    is_tty: bool,
}

impl ProgressReporter {
    pub fn new(is_tty: bool) -> Self {
        Self { is_tty }
    }

    pub fn detect() -> Self {
        Self { is_tty: io::stderr().is_terminal() }
    }

    pub fn start_file(&self, file_path: &str) {
        if !self.is_tty { return; }
        eprint!("\x1b[2m{}\x1b[0m ", file_path);
        let _ = io::stderr().flush();
    }

    pub fn update(&self, status: &str) {
        if !self.is_tty { return; }
        eprint!("\r\x1b[2K\x1b[2m  {} \x1b[0m", status);
        let _ = io::stderr().flush();
    }

    pub fn model_call(&self, file_path: &str, model: &str) {
        if !self.is_tty { return; }
        eprint!("\r\x1b[2K\x1b[2m  {} reviewing with {}...\x1b[0m", file_path, model);
        let _ = io::stderr().flush();
    }

    pub fn agent_tool_call(&self, file_path: &str, tool_name: &str, iteration: usize) {
        if !self.is_tty { return; }
        eprint!("\r\x1b[2K\x1b[2m  {} [iter {}] calling {}...\x1b[0m", file_path, iteration, tool_name);
        let _ = io::stderr().flush();
    }

    pub fn finish_file(&self, finding_count: usize) {
        if !self.is_tty { return; }
        if finding_count == 0 {
            eprintln!("\r\x1b[2K\x1b[32m  clean\x1b[0m");
        } else {
            eprintln!("\r\x1b[2K  {} finding(s)", finding_count);
        }
    }

    pub fn clear_line(&self) {
        if !self.is_tty { return; }
        eprint!("\r\x1b[2K");
        let _ = io::stderr().flush();
    }
}

pub fn format_status(file_path: &str, status: &str) -> String {
    format!("  {} {}", file_path, status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_formats_status_message() {
        let msg = format_status("src/auth.py", "reviewing with gpt-5.4...");
        assert!(msg.contains("src/auth.py"));
        assert!(msg.contains("gpt-5.4"));
    }

    #[test]
    fn progress_is_tty_aware() {
        let reporter = ProgressReporter::new(false);
        reporter.start_file("test.py");
        reporter.update("reviewing...");
        reporter.finish_file(3);
        // No panic, no output — just validates the code path
    }
}
