use colored::*;

/// Color scheme for NHI terminal output
pub struct ColorScheme;

impl ColorScheme {
    /// Success messages (green)
    pub fn success(text: &str) -> String {
        text.green().to_string()
    }

    /// Error messages (red)
    pub fn error(text: &str) -> String {
        text.red().to_string()
    }

    /// Warning messages (yellow)
    pub fn warning(text: &str) -> String {
        text.yellow().to_string()
    }

    /// Info messages (blue)
    pub fn info(text: &str) -> String {
        text.blue().to_string()
    }

    /// Instance ID (cyan)
    pub fn instance_id(text: &str) -> String {
        text.cyan().bold().to_string()
    }

    /// Process ID (magenta)
    pub fn pid(text: &str) -> String {
        text.magenta().to_string()
    }

    /// Status indicators
    pub fn status_running(text: &str) -> String {
        text.green().bold().to_string()
    }

    pub fn status_stopped(text: &str) -> String {
        text.red().to_string()
    }

    pub fn status_paused(text: &str) -> String {
        text.yellow().to_string()
    }

    pub fn status_starting(text: &str) -> String {
        text.blue().to_string()
    }

    pub fn status_failed(text: &str) -> String {
        text.red().bold().to_string()
    }

    /// Command names (bright white)
    pub fn command(text: &str) -> String {
        text.bright_white().bold().to_string()
    }

    /// File paths (bright black/gray)
    pub fn path(text: &str) -> String {
        text.bright_black().to_string()
    }

    /// Timestamps (bright black/gray)
    pub fn timestamp(text: &str) -> String {
        text.bright_black().to_string()
    }

    /// Headers and titles (bright cyan)
    pub fn header(text: &str) -> String {
        text.bright_cyan().bold().to_string()
    }

    /// Prompt text (bright green)
    pub fn prompt(text: &str) -> String {
        text.bright_green().to_string()
    }

    /// Checkpoint names (bright yellow)
    pub fn checkpoint(text: &str) -> String {
        text.bright_yellow().to_string()
    }

    /// Program names (bright blue)
    pub fn program(text: &str) -> String {
        text.bright_blue().to_string()
    }

    /// Mode indicators (bright magenta)
    pub fn mode(text: &str) -> String {
        text.bright_magenta().to_string()
    }

    /// Format status with appropriate color
    pub fn format_status(status: &str) -> String {
        match status.to_lowercase().as_str() {
            "running" => Self::status_running(status),
            "stopped" => Self::status_stopped(status),
            "paused" => Self::status_paused(status),
            "starting" => Self::status_starting(status),
            "failed" => Self::status_failed(status),
            _ => status.to_string(),
        }
    }

    /// Format instance mode with appropriate color
    pub fn format_mode(mode: &str) -> String {
        match mode.to_lowercase().as_str() {
            "normal" => Self::mode("Normal"),
            "detached" => Self::mode("Detached"),
            _ => mode.to_string(),
        }
    }

    /// Create a colored separator line
    pub fn separator(length: usize) -> String {
        "─".repeat(length).bright_black().to_string()
    }

    /// Create a colored table header
    pub fn table_header(text: &str) -> String {
        text.bright_white().bold().underline().to_string()
    }

    /// Format output with prefix colors
    pub fn format_output_line(line: &str) -> String {
        if line.starts_with("[STDOUT]") {
            format!("{} {}", "[STDOUT]".green(), &line[8..])
        } else if line.starts_with("[STDERR]") {
            format!("{} {}", "[STDERR]".red(), &line[8..])
        } else if line.starts_with("[INFO]") {
            format!("{} {}", "[INFO]".blue(), &line[6..])
        } else if line.starts_with("[ERROR]") {
            format!("{} {}", "[ERROR]".red().bold(), &line[7..])
        } else if line.starts_with("[WARN]") {
            format!("{} {}", "[WARN]".yellow(), &line[6..])
        } else {
            line.to_string()
        }
    }

    /// Create a progress indicator
    pub fn progress(text: &str) -> String {
        format!("{} {}", "⏳".yellow(), text.bright_white())
    }

    /// Create a success indicator
    pub fn success_indicator(text: &str) -> String {
        format!("{} {}", "✅".green(), text.green())
    }

    /// Create an error indicator
    pub fn error_indicator(text: &str) -> String {
        format!("{} {}", "❌".red(), text.red())
    }

    /// Create a warning indicator
    pub fn warning_indicator(text: &str) -> String {
        format!("{} {}", "⚠️".yellow(), text.yellow())
    }

    /// Create an info indicator
    pub fn info_indicator(text: &str) -> String {
        format!("{} {}", "ℹ️".blue(), text.blue())
    }
}

/// Convenience macros for colored output
#[macro_export]
macro_rules! print_success {
    ($($arg:tt)*) => {
        println!("{}", $crate::colors::ColorScheme::success_indicator(&format!($($arg)*)));
    };
}

#[macro_export]
macro_rules! print_error {
    ($($arg:tt)*) => {
        println!("{}", $crate::colors::ColorScheme::error_indicator(&format!($($arg)*)));
    };
}

#[macro_export]
macro_rules! print_warning {
    ($($arg:tt)*) => {
        println!("{}", $crate::colors::ColorScheme::warning_indicator(&format!($($arg)*)));
    };
}

#[macro_export]
macro_rules! print_info {
    ($($arg:tt)*) => {
        println!("{}", $crate::colors::ColorScheme::info_indicator(&format!($($arg)*)));
    };
}

#[macro_export]
macro_rules! print_progress {
    ($($arg:tt)*) => {
        println!("{}", $crate::colors::ColorScheme::progress(&format!($($arg)*)));
    };
}
