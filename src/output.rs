use colored::*;

/// Colored output utilities for better terminal experience
pub struct Output;

impl Output {
    /// Print success message
    pub fn success(msg: &str) {
        println!("{} {}", "✅".green(), msg.green());
    }
    
    /// Print info message
    pub fn info(msg: &str) {
        println!("{} {}", "ℹ️".blue(), msg.bright_blue());
    }
    
    /// Print warning message
    pub fn warning(msg: &str) {
        println!("{} {}", "⚠️".yellow(), msg.yellow());
    }
    
    /// Print error message
    pub fn error(msg: &str) {
        println!("{} {}", "❌".red(), msg.red());
    }
    
    /// Print network related message
    pub fn network(msg: &str) {
        println!("{} {}", "🌐".cyan(), msg.cyan());
    }
    
    /// Print migration related message
    pub fn migration(msg: &str) {
        println!("{} {}", "🚀".magenta(), msg.magenta());
    }
    
    /// Print instance related message
    pub fn instance(msg: &str) {
        println!("{} {}", "📦".bright_green(), msg.bright_green());
    }
    
    /// Print checkpoint related message
    pub fn checkpoint(msg: &str) {
        println!("{} {}", "💾".bright_yellow(), msg.bright_yellow());
    }
    
    /// Print header with separator
    pub fn header(title: &str) {
        let separator = "═".repeat(60);
        println!("\n{}", separator.bright_blue());
        println!("{} {}", "🎯".bright_white(), title.bright_white().bold());
        println!("{}\n", separator.bright_blue());
    }
    
    /// Print table header
    pub fn table_header(headers: &[&str]) {
        let header_line = headers.join(" ");
        println!("{}", header_line.bright_white().bold());
        println!("{}", "─".repeat(header_line.len()).bright_black());
    }
    
    /// Print status with appropriate color
    pub fn status(status: &str, message: &str) {
        let colored_status = match status.to_lowercase().as_str() {
            "running" => status.bright_green(),
            "shadow" => status.bright_yellow(),
            "stopped" => status.bright_red(),
            "error" => status.red(),
            _ => status.normal(),
        };
        println!("{} {}", colored_status, message);
    }
    
    /// Print node information
    pub fn node_info(node_id: &str, name: &str, addr: &str) {
        println!("{} Node {} ({}) at {}", 
                 "🔗".bright_cyan(), 
                 node_id.bright_cyan(), 
                 name.bright_white(), 
                 addr.bright_blue());
    }
    
    /// Print progress indicator
    pub fn progress(msg: &str) {
        print!("{} {}...", "⏳".yellow(), msg.yellow());
        std::io::Write::flush(&mut std::io::stdout()).unwrap();
    }
    
    /// Print completion for progress
    pub fn progress_done() {
        println!(" {}", "Done".green());
    }
}
