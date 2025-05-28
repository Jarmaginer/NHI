use crate::types::{CriuCliError, ProcessInfo, Result, StartMode};
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{error, info, warn};
use uuid::Uuid;
use std::fs::File;
use std::io::Write;

pub struct ProcessManager {
    processes: Arc<Mutex<HashMap<Uuid, ProcessInfo>>>,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            processes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn start_process(
        &self,
        instance_id: Uuid,
        program: &str,
        args: &[String],
        working_dir: &PathBuf,
    ) -> Result<u32> {
        self.start_process_with_mode(instance_id, program, args, working_dir, StartMode::Normal).await
    }

    pub async fn start_process_with_mode(
        &self,
        instance_id: Uuid,
        program: &str,
        args: &[String],
        working_dir: &PathBuf,
        start_mode: StartMode,
    ) -> Result<u32> {
        match start_mode {
            StartMode::Normal => self.start_process_normal(instance_id, program, args, working_dir).await,
            StartMode::Detached => self.start_process_detached(instance_id, program, args, working_dir).await,
        }
    }

    async fn start_process_normal(
        &self,
        instance_id: Uuid,
        program: &str,
        args: &[String],
        working_dir: &PathBuf,
    ) -> Result<u32> {
        info!("Starting process: {} with args: {:?}", program, args);

        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(working_dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| {
            error!("Failed to start process {}: {}", program, e);
            CriuCliError::ProcessError(format!("Failed to start process: {}", e))
        })?;

        let pid = child
            .id()
            .ok_or_else(|| CriuCliError::ProcessError("Failed to get process ID".to_string()))?;

        info!("Started process {} with PID: {}", program, pid);

        // Create shared output history and broadcast channel
        let output_history = Arc::new(Mutex::new(Vec::new()));
        let (output_sender, _) = tokio::sync::broadcast::channel(1000);

        // Create stdin channel for input forwarding
        let (stdin_sender, mut stdin_receiver) = tokio::sync::mpsc::unbounded_channel::<String>();

        // Take stdin, stdout and stderr from child
        let mut stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // Spawn task to handle stdin forwarding
        if let Some(mut stdin_writer) = stdin.take() {
            tokio::spawn(async move {
                while let Some(input) = stdin_receiver.recv().await {
                    if let Err(e) = stdin_writer.write_all(input.as_bytes()).await {
                        error!("Failed to write to stdin: {}", e);
                        break;
                    }
                    if let Err(e) = stdin_writer.flush().await {
                        error!("Failed to flush stdin: {}", e);
                        break;
                    }
                }
            });
        }

        // Spawn tasks to read stdout and stderr
        let stdout_handle = if let Some(stdout) = stdout {
            let history = output_history.clone();
            let sender = output_sender.clone();
            Some(tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let output_line = format!("[STDOUT] {}", line);

                    // Store in history
                    {
                        let mut history = history.lock().await;
                        history.push(output_line.clone());
                    }

                    // Broadcast to any attached listeners
                    let _ = sender.send(output_line);
                }
            }))
        } else {
            None
        };

        let stderr_handle = if let Some(stderr) = stderr {
            let history = output_history.clone();
            let sender = output_sender.clone();
            Some(tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let output_line = format!("[STDERR] {}", line);

                    // Store in history
                    {
                        let mut history = history.lock().await;
                        history.push(output_line.clone());
                    }

                    // Broadcast to any attached listeners
                    let _ = sender.send(output_line);
                }
            }))
        } else {
            None
        };

        let process_info = ProcessInfo {
            pid,
            child,
            output_history,
            stdout_handle,
            stderr_handle,
            output_sender: Some(output_sender),
            stdin_sender: Some(stdin_sender),
        };

        let mut processes = self.processes.lock().await;
        processes.insert(instance_id, process_info);

        Ok(pid)
    }

    pub async fn stop_process(&self, instance_id: &Uuid) -> Result<()> {
        let mut processes = self.processes.lock().await;

        if let Some(mut process_info) = processes.remove(instance_id) {
            info!("Stopping process with PID: {}", process_info.pid);

            // Try graceful shutdown first
            if let Err(e) = process_info.child.kill().await {
                warn!("Failed to kill process {}: {}", process_info.pid, e);
            }

            // Wait for the process to exit
            match process_info.child.wait().await {
                Ok(status) => {
                    info!("Process {} exited with status: {}", process_info.pid, status);
                }
                Err(e) => {
                    error!("Error waiting for process {}: {}", process_info.pid, e);
                }
            }

            Ok(())
        } else {
            Err(CriuCliError::InstanceNotFound(format!(
                "Process not found for instance: {}",
                instance_id
            )))
        }
    }

    pub async fn pause_process(&self, instance_id: &Uuid) -> Result<()> {
        let processes = self.processes.lock().await;

        if let Some(process_info) = processes.get(instance_id) {
            info!("Pausing process with PID: {}", process_info.pid);

            let pid = Pid::from_raw(process_info.pid as i32);
            signal::kill(pid, Signal::SIGSTOP).map_err(|e| {
                error!("Failed to pause process {}: {}", process_info.pid, e);
                CriuCliError::ProcessError(format!("Failed to pause process: {}", e))
            })?;

            info!("Process {} paused successfully", process_info.pid);
            Ok(())
        } else {
            Err(CriuCliError::InstanceNotFound(format!(
                "Process not found for instance: {}",
                instance_id
            )))
        }
    }

    pub async fn resume_process(&self, instance_id: &Uuid) -> Result<()> {
        let processes = self.processes.lock().await;

        if let Some(process_info) = processes.get(instance_id) {
            info!("Resuming process with PID: {}", process_info.pid);

            let pid = Pid::from_raw(process_info.pid as i32);
            signal::kill(pid, Signal::SIGCONT).map_err(|e| {
                error!("Failed to resume process {}: {}", process_info.pid, e);
                CriuCliError::ProcessError(format!("Failed to resume process: {}", e))
            })?;

            info!("Process {} resumed successfully", process_info.pid);
            Ok(())
        } else {
            Err(CriuCliError::InstanceNotFound(format!(
                "Process not found for instance: {}",
                instance_id
            )))
        }
    }

    pub async fn get_process_pid(&self, instance_id: &Uuid) -> Option<u32> {
        let processes = self.processes.lock().await;
        processes.get(instance_id).map(|info| info.pid)
    }

    pub async fn is_process_running(&self, instance_id: &Uuid) -> bool {
        let processes = self.processes.lock().await;
        if let Some(process_info) = processes.get(instance_id) {
            // Check if the process is still alive
            let pid = Pid::from_raw(process_info.pid as i32);
            signal::kill(pid, None).is_ok()
        } else {
            false
        }
    }

    pub async fn remove_process(&self, instance_id: &Uuid) {
        let mut processes = self.processes.lock().await;
        processes.remove(instance_id);
    }

    pub async fn list_processes(&self) -> Vec<(Uuid, u32)> {
        let processes = self.processes.lock().await;
        processes
            .iter()
            .map(|(id, info)| (*id, info.pid))
            .collect()
    }

    pub async fn get_output_history(&self, instance_id: &Uuid) -> Option<Vec<String>> {
        let processes = self.processes.lock().await;
        if let Some(process_info) = processes.get(instance_id) {
            let history = process_info.output_history.lock().await;
            Some(history.clone())
        } else {
            None
        }
    }

    pub async fn get_output_history_arc(&self, instance_id: &Uuid) -> Option<Arc<Mutex<Vec<String>>>> {
        let processes = self.processes.lock().await;
        if let Some(process_info) = processes.get(instance_id) {
            Some(process_info.output_history.clone())
        } else {
            None
        }
    }

    pub async fn subscribe_to_output(&self, instance_id: &Uuid) -> Option<tokio::sync::broadcast::Receiver<String>> {
        let processes = self.processes.lock().await;
        if let Some(process_info) = processes.get(instance_id) {
            if let Some(sender) = &process_info.output_sender {
                Some(sender.subscribe())
            } else {
                None
            }
        } else {
            None
        }
    }

    pub async fn send_input(&self, instance_id: &Uuid, input: String) -> Result<()> {
        let processes = self.processes.lock().await;
        if let Some(process_info) = processes.get(instance_id) {
            if let Some(sender) = &process_info.stdin_sender {
                let input_with_newline = format!("{}\n", input);
                sender.send(input_with_newline).map_err(|_| {
                    CriuCliError::ProcessError("Failed to send input to process".to_string())
                })?;
                Ok(())
            } else {
                Err(CriuCliError::ProcessError("Process has no stdin".to_string()))
            }
        } else {
            Err(CriuCliError::InstanceNotFound(format!(
                "Process not found for instance: {}",
                instance_id
            )))
        }
    }

    pub async fn register_restored_process(
        &self,
        instance_id: Uuid,
        pid: u32,
        restored_history: Option<Vec<String>>,
    ) -> Result<()> {
        // For restored processes, we need to attach to the existing process
        // We can't capture stdout/stderr from an already running process easily,
        // but we can try to send input to it via /proc/PID/fd/0

        let output_history = Arc::new(Mutex::new(restored_history.unwrap_or_default()));
        let (output_sender, _) = tokio::sync::broadcast::channel(1000);
        let (stdin_sender, mut stdin_receiver) = tokio::sync::mpsc::unbounded_channel::<String>();

        // Try to find the output file for this restored process
        // Look for the original detached process output file
        let output_file_path = self.find_output_file_for_pid(pid).await;

        // Start output monitoring if we found an output file
        let output_monitor = if let Some(output_file) = output_file_path {
            let history = output_history.clone();
            let sender = output_sender.clone();
            let output_file_path = output_file.clone();

            info!("Starting output monitoring for restored process {} using file: {}", pid, output_file_path);

            Some(tokio::spawn(async move {
                let mut last_size = 0;

                // Get current file size to start monitoring from the end
                if let Ok(metadata) = std::fs::metadata(&output_file_path) {
                    last_size = metadata.len();
                }

                loop {
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

                    if let Ok(metadata) = std::fs::metadata(&output_file_path) {
                        let current_size = metadata.len();
                        if current_size > last_size {
                            // Read new content
                            if let Ok(content) = std::fs::read_to_string(&output_file_path) {
                                let new_content = &content[last_size as usize..];
                                for line in new_content.lines() {
                                    if !line.is_empty() {
                                        let output_line = format!("[OUTPUT] {}", line);

                                        // Store in history
                                        {
                                            let mut history = history.lock().await;
                                            history.push(output_line.clone());
                                        }

                                        // Broadcast to any attached listeners
                                        let _ = sender.send(output_line);
                                    }
                                }
                            }
                            last_size = current_size;
                        }
                    }

                    // Check if process is still running
                    let proc_path = format!("/proc/{}", pid);
                    if !std::path::Path::new(&proc_path).exists() {
                        info!("Restored process {} is no longer running, stopping output monitoring", pid);
                        break;
                    }
                }
            }))
        } else {
            warn!("No output file found for restored process {}, real-time output monitoring disabled", pid);
            None
        };

        // Create a task to handle stdin forwarding to the restored process
        let stdin_task = {
            let pid_copy = pid;
            tokio::spawn(async move {
                while let Some(input) = stdin_receiver.recv().await {
                    // Try multiple methods to send input to the restored process
                    let mut success = false;

                    // Method 1: Try /proc/PID/fd/0
                    let stdin_path = format!("/proc/{}/fd/0", pid_copy);
                    if let Ok(mut file) = std::fs::OpenOptions::new().write(true).open(&stdin_path) {
                        use std::io::Write;
                        if writeln!(file, "{}", input).is_ok() {
                            info!("Successfully sent input to process {} via /proc/fd/0", pid_copy);
                            success = true;
                        }
                    }

                    // Method 2: Try using kill to send signals (for simple commands)
                    if !success {
                        warn!("Failed to send input to restored process {} via /proc/fd/0", pid_copy);
                        warn!("Input was: {}", input);

                        // Check if process is still running
                        let proc_path = format!("/proc/{}", pid_copy);
                        if !std::path::Path::new(&proc_path).exists() {
                            error!("Process {} is no longer running", pid_copy);
                            break;
                        }
                    }
                }
            })
        };

        // Create a dummy child process - this won't be used for restored processes
        let mut dummy_cmd = Command::new("echo");
        dummy_cmd.arg("dummy");
        let dummy_child = dummy_cmd.spawn().map_err(|e| {
            CriuCliError::ProcessError(format!("Failed to create dummy process: {}", e))
        })?;

        let process_info = ProcessInfo {
            pid,
            child: dummy_child,
            output_history,
            stdout_handle: output_monitor, // Use output monitor for stdout
            stderr_handle: Some(stdin_task), // Use stdin task for stderr
            output_sender: Some(output_sender),
            stdin_sender: Some(stdin_sender),
        };

        let mut processes = self.processes.lock().await;
        processes.insert(instance_id, process_info);

        info!("Registered restored process with PID: {} (stdin forwarding enabled)", pid);
        Ok(())
    }

    async fn start_process_detached(
        &self,
        instance_id: Uuid,
        program: &str,
        args: &[String],
        working_dir: &PathBuf,
    ) -> Result<u32> {
        info!("Starting detached process: {} with args: {:?}", program, args);

        // Create a unique output file for this instance
        let output_file = working_dir.join(format!("instance_{}_output.log", instance_id.to_string()[..8].to_string()));

        // Create a shell script that will start the process in a completely detached way
        let script_path = working_dir.join(format!("start_detached_{}.sh", instance_id.to_string()[..8].to_string()));

        let script_content = format!(
            r#"#!/bin/bash

# Kill any existing processes with the same name
pkill -f "{}" || true

# Change to working directory
cd "{}"

# Start the program with redirected I/O
{} {} </dev/null >"{}" 2>&1 &

# Wait a moment and exit
sleep 0.1
"#,
            program,
            working_dir.display(),
            program,
            args.join(" "),
            output_file.display()
        );

        // Write the script
        info!("Creating detached start script at: {:?}", script_path);
        let mut script_file = File::create(&script_path).map_err(|e| {
            error!("Failed to create detached start script at {:?}: {}", script_path, e);
            CriuCliError::ProcessError(format!("Failed to create detached start script: {}", e))
        })?;

        script_file.write_all(script_content.as_bytes()).map_err(|e| {
            error!("Failed to write detached start script: {}", e);
            CriuCliError::ProcessError(format!("Failed to write detached start script: {}", e))
        })?;

        info!("Successfully created script: {:?}", script_path);

        // Make script executable
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script_path).map_err(|e| {
            CriuCliError::ProcessError(format!("Failed to get script permissions: {}", e))
        })?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).map_err(|e| {
            CriuCliError::ProcessError(format!("Failed to set script permissions: {}", e))
        })?;

        // Start the script directly with bash
        let mut cmd = Command::new("bash");
        cmd.arg(&script_path)
            .current_dir(working_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        let mut child = cmd.spawn().map_err(|e| {
            error!("Failed to start detached process {}: {}", program, e);
            CriuCliError::ProcessError(format!("Failed to start detached process: {}", e))
        })?;

        // Wait for the setsid command to complete
        match child.wait().await {
            Ok(status) => {
                if !status.success() {
                    error!("Script execution failed with status: {}", status);
                    return Err(CriuCliError::ProcessError(format!("Script execution failed: {}", status)));
                }
                info!("Script executed successfully");
            }
            Err(e) => {
                error!("Failed to wait for script execution: {}", e);
                return Err(CriuCliError::ProcessError(format!("Failed to wait for script: {}", e)));
            }
        }

        // Wait a moment for the script to start the actual program
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // Find the actual program PID by name with multiple attempts
        let mut pid = None;
        for attempt in 1..=5 {
            if let Some(found_pid) = self.find_process_pid_by_name(program).await {
                pid = Some(found_pid);
                break;
            }
            info!("Attempt {} to find PID for {}, waiting...", attempt, program);
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }

        let pid = pid.ok_or_else(|| {
            CriuCliError::ProcessError(format!("Failed to find PID for detached process: {} after 5 attempts", program))
        })?;

        info!("Started detached process {} with PID: {}", program, pid);

        // Create shared output history and broadcast channel
        let output_history = Arc::new(Mutex::new(Vec::new()));
        let (output_sender, _) = tokio::sync::broadcast::channel(1000);

        // Create stdin channel for input forwarding (limited for detached processes)
        let (stdin_sender, mut stdin_receiver) = tokio::sync::mpsc::unbounded_channel::<String>();

        // Create a task to monitor the output file
        let output_monitor = {
            let output_file_path = output_file.clone();
            let history = output_history.clone();
            let sender = output_sender.clone();

            tokio::spawn(async move {
                let mut last_size = 0;
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

                    if let Ok(metadata) = std::fs::metadata(&output_file_path) {
                        let current_size = metadata.len();
                        if current_size > last_size {
                            // Read new content
                            if let Ok(content) = std::fs::read_to_string(&output_file_path) {
                                let new_content = &content[last_size as usize..];
                                for line in new_content.lines() {
                                    if !line.is_empty() {
                                        let output_line = format!("[OUTPUT] {}", line);

                                        // Store in history
                                        {
                                            let mut history = history.lock().await;
                                            history.push(output_line.clone());
                                        }

                                        // Broadcast to any attached listeners
                                        let _ = sender.send(output_line);
                                    }
                                }
                            }
                            last_size = current_size;
                        }
                    }

                    // Check if process is still running
                    let proc_path = format!("/proc/{}", pid);
                    if !std::path::Path::new(&proc_path).exists() {
                        info!("Detached process {} is no longer running", pid);
                        break;
                    }
                }
            })
        };

        // Create a task to handle stdin forwarding (limited functionality for detached processes)
        let stdin_task = {
            let pid_copy = pid;
            tokio::spawn(async move {
                while let Some(input) = stdin_receiver.recv().await {
                    warn!("Input to detached process {} (limited functionality): {}", pid_copy, input);
                    // For detached processes, input capability is very limited
                    // We can try to write to /proc/PID/fd/0 but it may not work
                    let stdin_path = format!("/proc/{}/fd/0", pid_copy);
                    if let Ok(mut file) = std::fs::OpenOptions::new().write(true).open(&stdin_path) {
                        if writeln!(file, "{}", input).is_ok() {
                            info!("Successfully sent input to detached process {}", pid_copy);
                        } else {
                            warn!("Failed to send input to detached process {}", pid_copy);
                        }
                    } else {
                        warn!("Cannot send input to detached process {} - no stdin access", pid_copy);
                    }
                }
            })
        };

        let process_info = ProcessInfo {
            pid,
            child,
            output_history,
            stdout_handle: Some(output_monitor),
            stderr_handle: Some(stdin_task),
            output_sender: Some(output_sender),
            stdin_sender: Some(stdin_sender),
        };

        let mut processes = self.processes.lock().await;
        processes.insert(instance_id, process_info);

        // Clean up the script file after a delay
        let script_cleanup = script_path.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            let _ = std::fs::remove_file(script_cleanup);
        });

        Ok(pid)
    }

    async fn find_process_pid_by_name(&self, program_name: &str) -> Option<u32> {
        // Extract just the program name without path
        let program_basename = std::path::Path::new(program_name)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(program_name);

        info!("Looking for process with name: {}", program_basename);

        // Read /proc to find processes
        if let Ok(entries) = std::fs::read_dir("/proc") {
            for entry in entries.flatten() {
                if let Ok(pid_str) = entry.file_name().into_string() {
                    if let Ok(pid) = pid_str.parse::<u32>() {
                        // Read the command line
                        let cmdline_path = format!("/proc/{}/cmdline", pid);
                        if let Ok(cmdline) = std::fs::read_to_string(&cmdline_path) {
                            // cmdline is null-separated, take the first part
                            let cmd = cmdline.split('\0').next().unwrap_or("");
                            let cmd_basename = std::path::Path::new(cmd)
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or(cmd);

                            // Also check if the full command line contains our program
                            if cmd_basename == program_basename ||
                               cmd.contains(program_basename) ||
                               cmdline.replace('\0', " ").contains(program_name) {
                                info!("Found matching process: PID {} with cmdline: {}", pid, cmdline.replace('\0', " "));
                                return Some(pid);
                            }
                        }
                    }
                }
            }
        }

        warn!("No process found with name: {}", program_basename);
        None
    }

    async fn find_output_file_for_pid(&self, pid: u32) -> Option<String> {
        // Try to find the output file for a given PID
        // Look for files matching the pattern instance_*_output.log

        let current_dir = std::env::current_dir().ok()?;

        // Look for output files in the current directory
        if let Ok(entries) = std::fs::read_dir(&current_dir) {
            for entry in entries.flatten() {
                if let Some(filename) = entry.file_name().to_str() {
                    if filename.starts_with("instance_") && filename.ends_with("_output.log") {
                        let file_path = entry.path();

                        // Check if this file is being written to by our PID
                        // We can check the file descriptors of the process
                        let fd_dir = format!("/proc/{}/fd", pid);
                        if let Ok(fd_entries) = std::fs::read_dir(&fd_dir) {
                            for fd_entry in fd_entries.flatten() {
                                if let Ok(link_target) = std::fs::read_link(fd_entry.path()) {
                                    if link_target == file_path {
                                        info!("Found output file for PID {}: {}", pid, file_path.display());
                                        return Some(file_path.to_string_lossy().to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        warn!("No output file found for PID {}", pid);
        None
    }
}
