use crate::types::{CriuCliError, Result};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct TtyInfo {
    pub rdev: u64,
    pub dev: u64,
    pub tty_format: String,
}

impl TtyInfo {
    pub fn new(rdev: u64, dev: u64) -> Self {
        Self {
            rdev,
            dev,
            tty_format: format!("tty[{:x}:{:x}]", rdev, dev),
        }
    }
}

#[derive(Debug)]
pub struct TtyEnvironment {
    pub is_complex: bool,
    pub tty_fds: Vec<(i32, TtyInfo)>,
    pub recommendations: Vec<String>,
}

pub fn detect_tty_environment(pid: u32) -> Result<TtyEnvironment> {
    let fd_dir = format!("/proc/{}/fd", pid);
    let fd_path = Path::new(&fd_dir);
    
    if !fd_path.exists() {
        return Err(CriuCliError::ProcessError(format!(
            "Process {} not found or no access to /proc/{}/fd", pid, pid
        )));
    }

    let mut tty_fds = Vec::new();
    let mut recommendations = Vec::new();
    
    // Read all file descriptors
    let entries = fs::read_dir(fd_path).map_err(|e| {
        CriuCliError::ProcessError(format!("Failed to read /proc/{}/fd: {}", pid, e))
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| {
            CriuCliError::ProcessError(format!("Failed to read fd entry: {}", e))
        })?;
        
        let fd_name = entry.file_name();
        let fd_str = fd_name.to_string_lossy();
        
        // Skip non-numeric entries
        if let Ok(fd_num) = fd_str.parse::<i32>() {
            let fd_path = entry.path();
            
            // Check if this fd points to a TTY device
            if let Ok(link_target) = fs::read_link(&fd_path) {
                let target_str = link_target.to_string_lossy();
                
                // Check for various TTY patterns
                if target_str.contains("/dev/pts/") || 
                   target_str.contains("/dev/ptmx") || 
                   target_str.contains("/dev/tty") {
                    
                    // Get TTY information
                    if let Ok(metadata) = fs::metadata(&fd_path) {
                        let tty_info = TtyInfo::new(metadata.rdev(), metadata.dev());
                        tty_fds.push((fd_num, tty_info));
                        
                        info!("Found TTY fd {}: {} -> {}", fd_num, target_str, 
                              format!("tty[{:x}:{:x}]", metadata.rdev(), metadata.dev()));
                    }
                }
            }
        }
    }

    // Determine if environment is complex
    let is_complex = tty_fds.len() > 3 || // More than stdin/stdout/stderr TTYs
                     tty_fds.iter().any(|(fd, _)| *fd > 2); // TTY fds beyond std streams

    // Generate recommendations
    if is_complex {
        recommendations.push("Complex TTY environment detected.".to_string());
        recommendations.push("For better CRIU compatibility, consider:".to_string());
        recommendations.push("1. Using 'start-detached' command instead of 'start'".to_string());
        recommendations.push("2. Running programs in a simpler terminal environment".to_string());
        recommendations.push("3. Using output redirection to files".to_string());
        
        if tty_fds.len() > 5 {
            recommendations.push("WARNING: Very complex TTY environment with many file descriptors.".to_string());
            recommendations.push("CRIU checkpoint/restore may fail without proper TTY handling.".to_string());
        }
    } else if !tty_fds.is_empty() {
        recommendations.push("Simple TTY environment detected.".to_string());
        recommendations.push("CRIU should work with proper external TTY parameters.".to_string());
    } else {
        recommendations.push("No TTY file descriptors detected - optimal for CRIU.".to_string());
    }

    Ok(TtyEnvironment {
        is_complex,
        tty_fds,
        recommendations,
    })
}

pub fn generate_criu_tty_args(tty_env: &TtyEnvironment) -> Vec<String> {
    let mut args = Vec::new();
    
    // Collect unique TTY formats
    let mut unique_ttys = std::collections::HashSet::new();
    for (_, tty_info) in &tty_env.tty_fds {
        unique_ttys.insert(tty_info.tty_format.clone());
    }
    
    // Add external TTY arguments for dump
    for tty_format in unique_ttys {
        args.push("--external".to_string());
        args.push(tty_format);
    }
    
    args
}

pub fn generate_criu_restore_tty_args(tty_env: &TtyEnvironment) -> Vec<String> {
    let mut args = Vec::new();
    
    // For restore, we need --inherit-fd arguments
    for (fd, tty_info) in &tty_env.tty_fds {
        args.push("--inherit-fd".to_string());
        args.push(format!("fd[{}]:{}", fd, tty_info.tty_format));
    }
    
    args
}

pub fn print_tty_analysis(tty_env: &TtyEnvironment) {
    println!("=== TTY Environment Analysis ===");
    
    if tty_env.tty_fds.is_empty() {
        println!("✅ No TTY file descriptors detected - optimal for CRIU");
    } else {
        println!("TTY file descriptors found:");
        for (fd, tty_info) in &tty_env.tty_fds {
            println!("  fd {}: {}", fd, tty_info.tty_format);
        }
        
        if tty_env.is_complex {
            println!("⚠️  Complex TTY environment detected");
        } else {
            println!("✅ Simple TTY environment");
        }
    }
    
    println!("\nRecommendations:");
    for rec in &tty_env.recommendations {
        println!("  {}", rec);
    }
    
    if !tty_env.tty_fds.is_empty() {
        println!("\nCRIU TTY arguments that would be used:");
        let dump_args = generate_criu_tty_args(tty_env);
        if !dump_args.is_empty() {
            println!("  For dump: {}", dump_args.join(" "));
        }
        
        let restore_args = generate_criu_restore_tty_args(tty_env);
        if !restore_args.is_empty() {
            println!("  For restore: {}", restore_args.join(" "));
        }
    }
    
    println!("=== End TTY Analysis ===");
}

pub fn check_process_tty_compatibility(pid: u32) -> Result<bool> {
    match detect_tty_environment(pid) {
        Ok(tty_env) => {
            if tty_env.is_complex {
                warn!("Process {} has complex TTY environment - CRIU may have issues", pid);
                print_tty_analysis(&tty_env);
                Ok(false)
            } else {
                info!("Process {} has simple/no TTY environment - good for CRIU", pid);
                Ok(true)
            }
        }
        Err(e) => {
            warn!("Failed to analyze TTY environment for process {}: {}", pid, e);
            Err(e)
        }
    }
}
