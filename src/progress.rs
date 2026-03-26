/// Interactive terminal progress feedback.
/// Writes spinner + status to stderr when TTY, silent when piped.

use std::io::{self, IsTerminal, Write};

pub struct ProgressReporter {
    is_tty: bool,
}

/// Strip ANSI control characters from untrusted input before embedding in terminal output.
fn sanitize(s: &str) -> String {
    s.chars().filter(|c| !c.is_control() || *c == '\n').collect()
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
        eprint!("\x1b[2m{}\x1b[0m ", sanitize(file_path));
        let _ = io::stderr().flush();
    }

    pub fn update(&self, status: &str) {
        if !self.is_tty { return; }
        eprint!("\r\x1b[2K\x1b[2m  {} \x1b[0m", sanitize(status));
        let _ = io::stderr().flush();
    }

    pub fn model_call(&self, file_path: &str, model: &str) {
        if !self.is_tty { return; }
        eprint!("\r\x1b[2K\x1b[2m  {} reviewing with {}...\x1b[0m", sanitize(file_path), sanitize(model));
        let _ = io::stderr().flush();
    }

    pub fn agent_tool_call(&self, file_path: &str, tool_name: &str, iteration: usize) {
        if !self.is_tty { return; }
        eprint!("\r\x1b[2K\x1b[2m  {} [iter {}] calling {}...\x1b[0m", sanitize(file_path), iteration, sanitize(tool_name));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_control_chars() {
        assert_eq!(sanitize("normal.py"), "normal.py");
        assert_eq!(sanitize("evil\x1b[31m.py"), "evil[31m.py");
        assert_eq!(sanitize("tab\there"), "tabhere");
    }

    #[test]
    fn progress_is_tty_aware() {
        let reporter = ProgressReporter::new(false);
        reporter.start_file("test.py");
        reporter.update("reviewing...");
        reporter.finish_file(3);
    }
}
