use std::io::IsTerminal;

/// Terminal style detection -- resolved once at startup.
pub struct Style {
    pub dim: &'static str,
    pub bold: &'static str,
    pub green: &'static str,
    pub red: &'static str,
    pub yellow: &'static str,
    pub reset: &'static str,
}

impl Style {
    pub fn detect(no_color_flag: bool) -> Self {
        if should_disable_color(no_color_flag) {
            Self::plain()
        } else {
            Self::ansi()
        }
    }

    fn ansi() -> Self {
        Self {
            dim: "\x1b[2m",
            bold: "\x1b[1m",
            green: "\x1b[32m",
            red: "\x1b[31m",
            yellow: "\x1b[33m",
            reset: "\x1b[0m",
        }
    }

    fn plain() -> Self {
        Self {
            dim: "",
            bold: "",
            green: "",
            red: "",
            yellow: "",
            reset: "",
        }
    }
}

fn should_disable_color(no_color_flag: bool) -> bool {
    no_color_flag
        || !std::io::stdout().is_terminal()
        || std::env::var("NO_COLOR").is_ok_and(|v| !v.is_empty())
        || std::env::var("TERM").is_ok_and(|v| v == "dumb")
}
