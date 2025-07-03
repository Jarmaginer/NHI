use crate::types::{CriuCliError, ProcessInfo, Result, StartMode};
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
    shadow_manager: Arc<Mutex<Option<Arc<tokio::sync::RwLock<crate::shadow_instance_manager::ShadowInstanceManager>>>>>,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            processes: Arc::new(Mutex::new(HashMap::new())),
            shadow_manager: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn set_shadow_manager(&self, shadow_manager: Arc<tokio::sync::RwLock<crate::shadow_instance_manager::ShadowInstanceManager>>) {
        let mut mgr = self.shadow_manager.lock().await;
        *mgr = Some(shadow_manager);
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
            let shadow_mgr = self.shadow_manager.clone();
            let instance_id_copy = instance_id;
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
                    let _ = sender.send(output_line.clone());

                    // Stream to shadow instances if shadow manager is available
                    if let Some(shadow_mgr_ref) = shadow_mgr.lock().await.as_ref() {
                        let shadow_mgr_read = shadow_mgr_ref.read().await;
                        let output_bytes = format!("{}\n", line).into_bytes();
                        if let Err(e) = shadow_mgr_read.stream_output_to_shadows(
                            instance_id_copy,
                            output_bytes,
                            crate::message_protocol::StreamType::Stdout
                        ).await {
                            tracing::debug!("Failed to stream stdout to shadows: {}", e);
                        }
                    }
                }
            }))
        } else {
            None
        };

        let stderr_handle = if let Some(stderr) = stderr {
            let history = output_history.clone();
            let sender = output_sender.clone();
            let shadow_mgr = self.shadow_manager.clone();
            let instance_id_copy = instance_id;
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
                    let _ = sender.send(output_line.clone());

                    // Stream to shadow instances if shadow manager is available
                    if let Some(shadow_mgr_ref) = shadow_mgr.lock().await.as_ref() {
                        let shadow_mgr_read = shadow_mgr_ref.read().await;
                        let output_bytes = format!("{}\n", line).into_bytes();
                        if let Err(e) = shadow_mgr_read.stream_output_to_shadows(
                            instance_id_copy,
                            output_bytes,
                            crate::message_protocol::StreamType::Stderr
                        ).await {
                            tracing::debug!("Failed to stream stderr to shadows: {}", e);
                        }
                    }
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
            info!("Stopping managed process with PID: {}", process_info.pid);

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
            // For detached processes, we need to kill by PID using system signals
            Err(CriuCliError::InstanceNotFound(format!(
                "Process not found for instance: {} (may be detached)",
                instance_id
            )))
        }
    }

    pub async fn stop_detached_process(&self, pid: u32) -> Result<()> {
        info!("Stopping detached process with PID: {}", pid);

        // Use system signal to kill the detached process
        let pid_nix = Pid::from_raw(pid as i32);

        // Try SIGTERM first (graceful)
        if let Err(e) = signal::kill(pid_nix, Signal::SIGTERM) {
            warn!("Failed to send SIGTERM to process {}: {}", pid, e);

            // Try SIGKILL (forceful)
            if let Err(e) = signal::kill(pid_nix, Signal::SIGKILL) {
                error!("Failed to send SIGKILL to process {}: {}", pid, e);
                return Err(CriuCliError::ProcessError(format!("Failed to kill process {}: {}", pid, e)));
            }
        }

        // Wait a moment and check if process is gone
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        if signal::kill(pid_nix, None).is_ok() {
            warn!("Process {} is still running after kill attempts", pid);
            info!("Detached process {} is no longer running", pid);
        } else {
            info!("Detached process {} is no longer running", pid);
        }

        Ok(())
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

    /// Register a migrated process with the process manager
    pub async fn register_migrated_process(
        &self,
        instance_id: Uuid,
        pid: u32,
        program: &str,
        args: &[String],
        working_dir: &PathBuf,
    ) -> Result<()> {
        info!("🔄 [MIGRATE_REG] Registering migrated process: PID {} for instance {}", pid, instance_id);

        // Create a dummy child process handle for the migrated process
        // Since we can't create a real Child from an existing PID, we'll create a minimal ProcessInfo
        let output_history = Arc::new(Mutex::new(Vec::new()));

        // Try to find and monitor the output file for this migrated process
        let output_file_path = if let Some(output_file) = self.find_output_file_for_pid(pid).await {
            Some(output_file)
        } else {
            // Create output file path based on instance structure
            let instance_short_id = instance_id.to_string()[..8].to_string();
            let instance_dir = PathBuf::from("instances").join(format!("instance_{}", instance_short_id));
            let output_dir = instance_dir.join("output");
            let output_file = output_dir.join("process_output.log");

            // Ensure output directory exists for migrated process
            if let Err(e) = std::fs::create_dir_all(&output_dir) {
                warn!("Failed to create output directory for migrated process: {}", e);
            }

            // Create output file if it doesn't exist
            if !output_file.exists() {
                if let Err(e) = std::fs::File::create(&output_file) {
                    warn!("Failed to create output file for migrated process: {}", e);
                    None
                } else {
                    info!("📄 [MIGRATE_REG] Created output file for migrated process: {}", output_file.display());
                    Some(output_file.to_string_lossy().to_string())
                }
            } else {
                info!("📄 [MIGRATE_REG] Found existing output file for migrated process: {}", output_file.display());
                Some(output_file.to_string_lossy().to_string())
            }
        };

        // Start output monitoring for the migrated process if we found an output file
        let (output_sender, _) = tokio::sync::broadcast::channel::<String>(1000);
        let output_monitor = if let Some(output_file) = output_file_path {
            let output_history_clone = output_history.clone();
            let output_sender_clone = output_sender.clone();

            info!("📄 [MIGRATE_REG] Starting output monitoring for migrated process from file: {}", output_file);

            Some(tokio::spawn(async move {
                // Monitor the output file for changes
                let mut last_size = 0;
                let mut file_path = PathBuf::from(&output_file);

                loop {
                    if let Ok(metadata) = tokio::fs::metadata(&file_path).await {
                        let current_size = metadata.len();
                        if current_size > last_size {
                            // Read new content
                            if let Ok(content) = tokio::fs::read_to_string(&file_path).await {
                                let lines: Vec<&str> = content.lines().collect();
                                let start_line = (last_size as f64 / 50.0) as usize; // Rough estimate

                                for line in lines.iter().skip(start_line) {
                                    let line_str = line.to_string();

                                    // Add to history
                                    {
                                        let mut history = output_history_clone.lock().await;
                                        history.push(line_str.clone());
                                        if history.len() > 1000 {
                                            history.remove(0);
                                        }
                                    }

                                    // Send to subscribers
                                    if output_sender_clone.send(line_str).is_err() {
                                        break;
                                    }
                                }
                            }
                            last_size = current_size;
                        }
                    }

                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
            }))
        } else {
            None
        };

        // Create a dummy child that represents the migrated process
        // We can't control this process directly, but we can monitor it
        let dummy_child = tokio::process::Command::new("sleep")
            .arg("0")
            .spawn()
            .map_err(|e| CriuCliError::ProcessError(format!("Failed to create dummy child: {}", e)))?;

        let process_info = ProcessInfo {
            pid,
            child: dummy_child,
            output_history,
            stdout_handle: output_monitor,
            stderr_handle: None,
            output_sender: Some(output_sender),
            stdin_sender: None, // Migrated processes don't support stdin by default
        };

        // Register the process
        {
            let mut processes = self.processes.lock().await;
            processes.insert(instance_id, process_info);
        }

        info!("✅ [MIGRATE_REG] Successfully registered migrated process {} for instance {}", pid, instance_id);
        Ok(())
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

        // Start with empty history for restored processes - we'll read from the live output file
        let output_history = Arc::new(Mutex::new(Vec::new()));
        let (output_sender, _) = tokio::sync::broadcast::channel(1000);
        let (stdin_sender, mut stdin_receiver) = tokio::sync::mpsc::unbounded_channel::<String>();

        // For restored processes, we know the output file location based on instance ID
        // Find the instance that matches this PID and use its output file
        let output_file_path = self.find_output_file_for_restored_process(instance_id, pid).await;

        // Start output monitoring if we found an output file
        let output_monitor = if let Some(output_file) = output_file_path {
            let history = output_history.clone();
            let sender = output_sender.clone();
            let output_file_path = output_file.clone();

            info!("Starting output monitoring for restored process {} using file: {}", pid, output_file_path);

            Some(tokio::spawn(async move {
                let mut last_size = 0;
                let mut first_read = true;

                loop {
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

                    if let Ok(metadata) = std::fs::metadata(&output_file_path) {
                        let current_size = metadata.len();

                        // For restored processes, read the entire file on first read
                        if first_read || current_size > last_size {
                            if let Ok(content) = std::fs::read_to_string(&output_file_path) {
                                let content_to_process = if first_read {
                                    // Read entire file on first read for restored processes
                                    first_read = false;
                                    &content
                                } else {
                                    // Read only new content on subsequent reads
                                    &content[last_size as usize..]
                                };

                                for line in content_to_process.lines() {
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

        // Create a unique output file for this instance in the instance directory
        let short_id = instance_id.to_string()[..8].to_string();
        let instance_dir = PathBuf::from("instances").join(format!("instance_{}", short_id));
        let output_dir = instance_dir.join("output");

        // Ensure output directory exists
        if let Err(e) = std::fs::create_dir_all(&output_dir) {
            error!("Failed to create output directory {:?}: {}", output_dir, e);
        }

        let output_file = output_dir.join("process_output.log");

        // Create a shell script that will start the process in a completely detached way
        let script_path = working_dir.join(format!("start_detached_{}.sh", instance_id.to_string()[..8].to_string()));

        // Use our daemon wrapper for true daemonization
        let daemon_wrapper_path = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("target")
            .join("daemon_wrapper");

        // Use pure Rust daemonization instead of C wrapper
        info!("Using Rust-native daemonization for better integration");

        // Convert program path to absolute path if it's relative
        let absolute_program_path = if Path::new(program).is_absolute() {
            program.to_string()
        } else {
            working_dir.join(program).display().to_string()
        };

        // Create a Rust-based daemon launcher script
        let script_content = format!(
            r#"#!/bin/bash

# Detailed logging for debugging
LOGFILE="instances/instance_{}/logs/daemon_startup.log"
mkdir -p "$(dirname "$LOGFILE")"
echo "$(date): Starting daemon process for {}" >> "$LOGFILE"

# Kill any existing processes with the same name
echo "$(date): Killing existing processes" >> "$LOGFILE"
pkill -f "{}" || true

# Change to working directory
echo "$(date): Changing to working directory: {}" >> "$LOGFILE"
cd "{}"

# Start process with proper daemonization using nohup and setsid
echo "$(date): Starting daemon process: {}" >> "$LOGFILE"
echo "$(date): Output file: {}" >> "$LOGFILE"
echo "$(date): Arguments: {}" >> "$LOGFILE"

# Use nohup and setsid for proper daemonization
nohup setsid "{}" {} </dev/null >"{}" 2>&1 &
DAEMON_PID=$!

echo "$(date): Daemon started with PID: $DAEMON_PID" >> "$LOGFILE"

# Wait for daemon creation to complete
sleep 1

# Verify the process is running
if ps -p $DAEMON_PID > /dev/null 2>&1; then
    echo "$(date): Daemon process verified running" >> "$LOGFILE"
else
    echo "$(date): ERROR: Daemon process not running!" >> "$LOGFILE"
fi
"#,
            instance_id.to_string()[..8].to_string(),
            program,
            program,
            working_dir.display(),
            working_dir.display(),
            absolute_program_path,
            output_file.display(),
            args.join(" "),
            absolute_program_path,
            args.join(" "),
            output_file.display()
        );

        // Create logs directory for this instance
        let logs_dir = working_dir.join("instances").join(format!("instance_{}", instance_id.to_string()[..8].to_string())).join("logs");
        if let Err(e) = std::fs::create_dir_all(&logs_dir) {
            warn!("Failed to create logs directory {:?}: {}", logs_dir, e);
        }

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
            // Read the daemon startup log for debugging
            let log_file = working_dir.join("instances").join(format!("instance_{}", instance_id.to_string()[..8].to_string())).join("logs").join("daemon_startup.log");
            if let Ok(log_content) = std::fs::read_to_string(&log_file) {
                error!("Daemon startup log content:\n{}", log_content);
            } else {
                error!("Could not read daemon startup log at {:?}", log_file);
            }

            CriuCliError::ProcessError(format!("Failed to find PID for detached process: {} after 5 attempts. Check daemon startup log for details.", program))
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
            let shadow_mgr = self.shadow_manager.clone();
            let instance_id_copy = instance_id;

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
                                        let _ = sender.send(output_line.clone());

                                        // Stream to shadow instances if shadow manager is available
                                        if let Some(shadow_mgr_ref) = shadow_mgr.lock().await.as_ref() {
                                            let shadow_mgr_read = shadow_mgr_ref.read().await;
                                            let output_bytes = format!("{}\n", line).into_bytes();
                                            tracing::debug!("Streaming output to shadows: '{}' for instance {}", line, instance_id_copy);
                                            if let Err(e) = shadow_mgr_read.stream_output_to_shadows(
                                                instance_id_copy,
                                                output_bytes,
                                                crate::message_protocol::StreamType::Stdout
                                            ).await {
                                                tracing::error!("Failed to stream output to shadows: {}", e);
                                            } else {
                                                tracing::debug!("Successfully streamed output to shadows");
                                            }
                                        } else {
                                            tracing::debug!("No shadow manager available for output streaming");
                                        }
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

        // Get list of all processes and find the newest one matching our criteria
        let mut matching_processes: Vec<u32> = Vec::new();

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

                            // More precise matching: exact basename match and not a shell script
                            if cmd_basename == program_basename && !cmd.contains("sh") && !cmd.contains("bash") {
                                // Additional check: make sure it's not a script wrapper
                                let stat_path = format!("/proc/{}/stat", pid);
                                if let Ok(stat_content) = std::fs::read_to_string(&stat_path) {
                                    // Check if this process was recently started (within last 10 seconds)
                                    let parts: Vec<&str> = stat_content.split_whitespace().collect();
                                    if parts.len() > 21 {
                                        if let Ok(starttime) = parts[21].parse::<u64>() {
                                            // This is a more reliable way to find the actual process
                                            matching_processes.push(pid);
                                            info!("Found candidate process: PID {} with cmdline: {}", pid, cmdline.replace('\0', " "));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Return the most recently started process if we found any
        if !matching_processes.is_empty() {
            // Sort by PID (higher PID usually means more recent)
            matching_processes.sort();
            let selected_pid = *matching_processes.last().unwrap();
            info!("Selected PID {} from {} candidates", selected_pid, matching_processes.len());
            return Some(selected_pid);
        }

        warn!("No process found with name: {}", program_basename);
        None
    }

    async fn find_output_file_for_pid(&self, pid: u32) -> Option<String> {
        // Try to find the output file for a given PID
        // Look in the new instances directory structure

        let instances_dir = PathBuf::from("instances");

        // Look for output files in all instance directories
        if let Ok(entries) = std::fs::read_dir(&instances_dir) {
            for entry in entries.flatten() {
                if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    let instance_dir = entry.path();
                    let output_file = instance_dir.join("output").join("process_output.log");

                    if output_file.exists() {
                        // Check if this file is being written to by our PID
                        // We can check the file descriptors of the process
                        let fd_dir = format!("/proc/{}/fd", pid);
                        if let Ok(fd_entries) = std::fs::read_dir(&fd_dir) {
                            for fd_entry in fd_entries.flatten() {
                                if let Ok(link_target) = std::fs::read_link(fd_entry.path()) {
                                    if link_target == output_file {
                                        info!("Found output file for PID {}: {}", pid, output_file.display());
                                        return Some(output_file.to_string_lossy().to_string());
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

    async fn find_output_file_for_restored_process(&self, instance_id: Uuid, pid: u32) -> Option<String> {
        // For restored processes, we can directly construct the output file path
        let short_id = instance_id.to_string()[..8].to_string();
        let output_file = PathBuf::from("instances")
            .join(format!("instance_{}", short_id))
            .join("output")
            .join("process_output.log");

        if output_file.exists() {
            info!("Found output file for restored process {} (instance {}): {}", pid, short_id, output_file.display());
            Some(output_file.to_string_lossy().to_string())
        } else {
            warn!("Output file not found for restored process {} (instance {}): {}", pid, short_id, output_file.display());
            None
        }
    }

    pub async fn show_process_output(&self, instance_id: &Uuid, lines: Option<usize>) -> Result<()> {
        // Try to find output file for this instance
        let short_id = instance_id.to_string()[..8].to_string();
        let instance_dir = PathBuf::from("instances").join(format!("instance_{}", short_id));
        let output_file = instance_dir.join("output").join("process_output.log");

        if output_file.exists() {
            info!("Reading output from: {:?}", output_file);

            match std::fs::read_to_string(&output_file) {
                Ok(content) => {
                    let lines_to_show = lines.unwrap_or(50); // Default to last 50 lines
                    let output_lines: Vec<&str> = content.lines().collect();
                    let start_index = if output_lines.len() > lines_to_show {
                        output_lines.len() - lines_to_show
                    } else {
                        0
                    };

                    println!("=== Process Output (last {} lines) ===", lines_to_show);
                    for line in &output_lines[start_index..] {
                        println!("{}", line);
                    }
                    println!("=== End Output ===");
                    Ok(())
                }
                Err(e) => {
                    error!("Failed to read output file {:?}: {}", output_file, e);
                    Err(CriuCliError::IoError(e))
                }
            }
        } else {
            warn!("No output file found for instance {}", short_id);
            println!("No output file found for this instance.");
            Ok(())
        }
    }
}
