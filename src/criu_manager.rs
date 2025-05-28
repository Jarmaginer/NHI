use crate::types::{CriuCliError, Result};
use crate::tty_utils::{detect_tty_environment, generate_criu_tty_args, generate_criu_restore_tty_args, print_tty_analysis};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

pub struct CriuManager {
    criu_path: PathBuf,
    checkpoints_dir: PathBuf,
}

impl CriuManager {
    pub fn new() -> Self {
        let criu_path = PathBuf::from("/home/realgod/sync2/criu/criu/criu");
        let checkpoints_dir = PathBuf::from("instances"); // Use instances directory

        // Create instances directory if it doesn't exist
        if !checkpoints_dir.exists() {
            if let Err(e) = std::fs::create_dir_all(&checkpoints_dir) {
                error!("Failed to create instances directory: {}", e);
            }
        }

        Self {
            criu_path,
            checkpoints_dir,
        }
    }

    pub async fn create_checkpoint(
        &self,
        pid: u32,
        checkpoint_name: &str,
        instance_id: &Uuid,
        output_history: Option<Vec<String>>,
    ) -> Result<PathBuf> {
        // Create instance-specific directory structure using short ID
        let short_id = instance_id.to_string()[..8].to_string();
        let instance_dir = self.checkpoints_dir.join(format!("instance_{}", short_id));
        let checkpoint_dir = instance_dir.join("checkpoints").join(checkpoint_name);

        self.create_checkpoint_in_dir(pid, checkpoint_name, &checkpoint_dir, instance_id, output_history).await
    }

    pub async fn create_checkpoint_in_dir(
        &self,
        pid: u32,
        checkpoint_name: &str,
        checkpoint_dir: &PathBuf,
        instance_id: &Uuid,
        output_history: Option<Vec<String>>,
    ) -> Result<PathBuf> {
        // Create checkpoint directory
        std::fs::create_dir_all(&checkpoint_dir).map_err(|e| {
            error!("Failed to create checkpoint directory: {}", e);
            CriuCliError::IoError(e)
        })?;

        info!("Creating checkpoint for PID {} in {:?}", pid, checkpoint_dir);

        // Step 1: Pause the process before checkpoint
        info!("Pausing process {} before checkpoint", pid);
        self.pause_process(pid)?;

        // Analyze TTY environment before creating checkpoint
        let tty_env = match detect_tty_environment(pid) {
            Ok(env) => {
                info!("TTY environment analysis for PID {}:", pid);
                print_tty_analysis(&env);

                if env.is_complex {
                    warn!("Complex TTY environment detected for PID {}. CRIU may have issues.", pid);
                    warn!("Consider using 'start-detached' for better CRIU compatibility.");
                }
                Some(env)
            }
            Err(e) => {
                warn!("Failed to analyze TTY environment for PID {}: {}", pid, e);
                None
            }
        };

        // Save output history first
        if let Some(history) = output_history {
            let history_file = checkpoint_dir.join("output_history.json");
            let history_json = serde_json::to_string_pretty(&history).map_err(|e| {
                error!("Failed to serialize output history: {}", e);
                CriuCliError::CriuError(format!("Failed to serialize output history: {}", e))
            })?;

            std::fs::write(&history_file, history_json).map_err(|e| {
                error!("Failed to write output history: {}", e);
                CriuCliError::IoError(e)
            })?;

            info!("Saved output history with {} lines", history.len());
        }

        // Backup output files that might change after checkpoint
        self.backup_output_files(pid, &checkpoint_dir)?;

        // Build CRIU dump command with TTY arguments
        let mut cmd = Command::new(&self.criu_path);
        cmd.arg("dump")
            .arg("-t")
            .arg(pid.to_string())
            .arg("-D")
            .arg(&checkpoint_dir)
            .arg("-v4")
            .arg("--leave-running")
            .arg("--shell-job");

        // Add TTY-specific arguments if needed
        if let Some(ref env) = tty_env {
            let tty_args = generate_criu_tty_args(env);
            if !tty_args.is_empty() {
                info!("Adding TTY arguments to CRIU dump: {:?}", tty_args);
                for arg in tty_args {
                    cmd.arg(arg);
                }
            }
        }

        // Execute CRIU dump command
        let output = cmd.output().map_err(|e| {
            error!("Failed to execute CRIU dump: {}", e);
            CriuCliError::CriuError(format!("Failed to execute CRIU: {}", e))
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("CRIU dump failed: {}", stderr);

            // Resume the process even if checkpoint failed
            if let Err(resume_err) = self.resume_process(pid) {
                error!("Failed to resume process {} after checkpoint failure: {}", pid, resume_err);
            }

            return Err(CriuCliError::CriuError(format!(
                "CRIU dump failed: {}",
                stderr
            )));
        }

        // Step 2: Resume the original process after successful checkpoint
        info!("Resuming original process {} after checkpoint", pid);
        if let Err(e) = self.resume_process(pid) {
            warn!("Failed to resume process {} after checkpoint: {}", pid, e);
            // Don't fail the checkpoint operation, just warn
        }

        info!("Checkpoint created successfully: {}", checkpoint_name);
        Ok(checkpoint_dir.clone())
    }

    pub async fn restore_checkpoint(
        &self,
        checkpoint_name: &str,
        instance_id: Option<&Uuid>,
    ) -> Result<(u32, Option<Vec<String>>)> {
        // Try to find checkpoint in instance-specific directory first, then search globally
        let checkpoint_dir = if let Some(id) = instance_id {
            let short_id = id.to_string()[..8].to_string();
            let instance_dir = self.checkpoints_dir.join(format!("instance_{}", short_id));
            let expected_checkpoint_path = instance_dir.join("checkpoints").join(checkpoint_name);

            if expected_checkpoint_path.exists() {
                info!("Found checkpoint '{}' in expected instance directory: instance_{}", checkpoint_name, short_id);
                expected_checkpoint_path
            } else {
                warn!("⚠️  Checkpoint '{}' not found in instance_{} directory", checkpoint_name, short_id);
                warn!("🔍 Searching for checkpoint '{}' in all instance directories...", checkpoint_name);

                // Search in all instance directories
                match self.find_checkpoint_in_any_instance(checkpoint_name) {
                    Ok(found_path) => {
                        // Extract the instance directory name from the found path
                        if let Some(parent) = found_path.parent() {
                            if let Some(grandparent) = parent.parent() {
                                if let Some(instance_name) = grandparent.file_name() {
                                    warn!("✅ Found checkpoint '{}' in different instance: {}", checkpoint_name, instance_name.to_string_lossy());
                                    warn!("📁 Will restore from: {}", found_path.display());
                                }
                            }
                        }
                        found_path
                    }
                    Err(e) => return Err(e)
                }
            }
        } else {
            // Fallback: search in all instance directories
            self.find_checkpoint_in_any_instance(checkpoint_name)?
        };

        if !checkpoint_dir.exists() {
            return Err(CriuCliError::CheckpointNotFound(checkpoint_name.to_string()));
        }

        // Convert to absolute path for CRIU
        let checkpoint_dir = checkpoint_dir.canonicalize().map_err(|e| {
            error!("Failed to get absolute path for checkpoint directory: {}", e);
            CriuCliError::IoError(e)
        })?;

        info!("Restoring checkpoint from {:?}", checkpoint_dir);

        // Check for PID conflicts before restoring
        if let Some(original_pid) = self.get_original_pid_from_checkpoint(&checkpoint_dir)? {
            if self.is_pid_in_use(original_pid) {
                warn!("PID {} is already in use. The original process is still running.", original_pid);
                warn!("Will terminate the original process to restore the checkpoint.");

                // Kill the conflicting process
                match self.kill_conflicting_process(original_pid) {
                    Ok(()) => {
                        info!("Successfully terminated conflicting process with PID {}", original_pid);
                        // Wait a moment for the process to fully terminate
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                    Err(e) => {
                        return Err(CriuCliError::CriuError(format!(
                            "Cannot restore: PID {} is in use and could not be terminated: {}. \
                             Please manually stop the conflicting process.",
                            original_pid, e
                        )));
                    }
                }
            } else {
                info!("Original PID {} is available, will restore with same PID", original_pid);
            }
        }

        // Clean up any existing pidfile from previous restore attempts
        let pidfile = checkpoint_dir.join("restored.pid");
        if pidfile.exists() {
            info!("Removing existing pidfile from previous restore");
            if let Err(e) = std::fs::remove_file(&pidfile) {
                warn!("Failed to remove existing pidfile: {}", e);
            }
        }

        // Restore output files to their checkpoint state
        self.restore_output_files(&checkpoint_dir)?;

        // Load output history first
        let output_history = {
            let history_file = checkpoint_dir.join("output_history.json");
            if history_file.exists() {
                match std::fs::read_to_string(&history_file) {
                    Ok(content) => {
                        match serde_json::from_str::<Vec<String>>(&content) {
                            Ok(history) => {
                                info!("Loaded output history with {} lines", history.len());
                                Some(history)
                            }
                            Err(e) => {
                                warn!("Failed to parse output history: {}", e);
                                None
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to read output history: {}", e);
                        None
                    }
                }
            } else {
                info!("No output history found for checkpoint");
                None
            }
        };

        // Execute CRIU restore command
        let mut cmd = Command::new(&self.criu_path);
        cmd.arg("restore")
            .arg("-D")
            .arg(&checkpoint_dir)
            .arg("-v4")
            .arg("--restore-detached")
            .arg("--shell-job")
            .arg("--pidfile")
            .arg(checkpoint_dir.join("restored.pid"))
            .arg("--log-file")
            .arg(checkpoint_dir.join("restore.log"));



        let output = cmd.output().map_err(|e| {
            error!("Failed to execute CRIU restore: {}", e);
            CriuCliError::CriuError(format!("Failed to execute CRIU: {}", e))
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("CRIU restore failed: {}", stderr);
            return Err(CriuCliError::CriuError(format!(
                "CRIU restore failed: {}",
                stderr
            )));
        }

        // Parse the output to get the restored PID
        let stdout = String::from_utf8_lossy(&output.stdout);
        debug!("CRIU restore output: {}", stdout);

        // Get the restored PID
        let restored_pid = self.get_restored_pid(&checkpoint_dir).await?;

        // Resume the restored process (CRIU restores processes in stopped state)
        info!("Resuming restored process {} after CRIU restore", restored_pid);
        if let Err(e) = self.resume_process(restored_pid) {
            warn!("Failed to resume restored process {}: {}", restored_pid, e);
            // Don't fail the restore operation, just warn
        } else {
            info!("Successfully resumed restored process {}", restored_pid);
        }

        info!("Checkpoint restored successfully with PID: {}", restored_pid);
        Ok((restored_pid, output_history))
    }

    async fn get_restored_pid(&self, checkpoint_dir: &Path) -> Result<u32> {
        let pidfile = checkpoint_dir.join("restored.pid");
        let mut found_pid = None;

        // Wait for the pidfile to be created and read it immediately
        for i in 0..50 {  // More attempts with shorter intervals
            if pidfile.exists() {
                match std::fs::read_to_string(&pidfile) {
                    Ok(content) => {
                        if let Ok(pid) = content.trim().parse::<u32>() {
                            info!("Read restored PID from pidfile: {}", pid);
                            found_pid = Some(pid);

                            // Check if process is running
                            let proc_path = format!("/proc/{}", pid);
                            if std::path::Path::new(&proc_path).exists() {
                                info!("Verified process {} is running", pid);
                                return Ok(pid);
                            } else {
                                warn!("Process {} from pidfile is not running (attempt {})", pid, i + 1);
                                // Don't break immediately, the process might be starting
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to read pidfile: {}", e);
                    }
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        // Check restore log for more information
        let restore_log = checkpoint_dir.join("restore.log");
        if restore_log.exists() {
            if let Ok(log_content) = std::fs::read_to_string(&restore_log) {
                error!("CRIU restore log:\n{}", log_content);

                // Try to extract PID from log if pidfile failed
                if found_pid.is_none() {
                    for line in log_content.lines() {
                        if line.contains("pidfile: Wrote pid") {
                            if let Some(pid_str) = line.split("pid ").nth(1) {
                                if let Some(pid_str) = pid_str.split(" ").next() {
                                    if let Ok(pid) = pid_str.parse::<u32>() {
                                        warn!("Extracted PID {} from restore log", pid);
                                        found_pid = Some(pid);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if let Some(pid) = found_pid {
            warn!("Using PID {} from restore process (process may have exited)", pid);
            Ok(pid)
        } else {
            warn!("Could not determine restored PID, using placeholder");
            Ok(1)
        }
    }

    pub fn list_checkpoints(&self) -> Result<Vec<String>> {
        let mut checkpoints = Vec::new();

        if !self.checkpoints_dir.exists() {
            return Ok(checkpoints);
        }

        let entries = std::fs::read_dir(&self.checkpoints_dir).map_err(|e| {
            error!("Failed to read checkpoints directory: {}", e);
            CriuCliError::IoError(e)
        })?;

        for entry in entries {
            let entry = entry.map_err(CriuCliError::IoError)?;
            if entry.file_type().map_err(CriuCliError::IoError)?.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    checkpoints.push(name.to_string());
                }
            }
        }

        Ok(checkpoints)
    }

    fn find_checkpoint_in_any_instance(&self, checkpoint_name: &str) -> Result<PathBuf> {
        if !self.checkpoints_dir.exists() {
            return Err(CriuCliError::CheckpointNotFound(checkpoint_name.to_string()));
        }

        let entries = std::fs::read_dir(&self.checkpoints_dir).map_err(|e| {
            error!("Failed to read instances directory: {}", e);
            CriuCliError::IoError(e)
        })?;

        for entry in entries {
            let entry = entry.map_err(CriuCliError::IoError)?;
            if entry.file_type().map_err(CriuCliError::IoError)?.is_dir() {
                let instance_dir = entry.path();
                let checkpoint_path = instance_dir.join("checkpoints").join(checkpoint_name);
                if checkpoint_path.exists() {
                    info!("Found checkpoint '{}' in instance directory: {:?}", checkpoint_name, instance_dir);
                    return Ok(checkpoint_path);
                }
            }
        }

        Err(CriuCliError::CheckpointNotFound(checkpoint_name.to_string()))
    }

    pub fn checkpoint_exists(&self, checkpoint_name: &str) -> bool {
        // Check if checkpoint exists in any instance directory
        self.find_checkpoint_in_any_instance(checkpoint_name).is_ok()
    }

    pub fn get_checkpoint_path(&self, checkpoint_name: &str) -> PathBuf {
        // Try to find in any instance directory, fallback to old path
        self.find_checkpoint_in_any_instance(checkpoint_name)
            .unwrap_or_else(|_| self.checkpoints_dir.join(checkpoint_name))
    }

    fn get_original_pid_from_checkpoint(&self, checkpoint_dir: &Path) -> Result<Option<u32>> {
        // Try to find core files with PID in filename (core-PID.img)
        if let Ok(entries) = std::fs::read_dir(checkpoint_dir) {
            for entry in entries.flatten() {
                let filename = entry.file_name();
                let filename_str = filename.to_string_lossy();

                // Look for core-PID.img files
                if filename_str.starts_with("core-") && filename_str.ends_with(".img") {
                    let pid_part = &filename_str[5..filename_str.len()-4]; // Remove "core-" and ".img"
                    if let Ok(pid) = pid_part.parse::<u32>() {
                        info!("Found original PID {} from core file: {}", pid, filename_str);
                        return Ok(Some(pid));
                    }
                }
            }
        }

        // Fallback: try to extract PID from the dump log
        let dump_log = checkpoint_dir.join("dump.log");
        if dump_log.exists() {
            if let Ok(log_content) = std::fs::read_to_string(&dump_log) {
                // Look for lines like "dump -t 12345"
                for line in log_content.lines() {
                    if line.contains("dump -t") {
                        // Extract PID from command line like "dump -t 12345"
                        let parts: Vec<&str> = line.split_whitespace().collect();
                        for (i, part) in parts.iter().enumerate() {
                            if part == &"-t" && i + 1 < parts.len() {
                                if let Ok(pid) = parts[i + 1].parse::<u32>() {
                                    info!("Found original PID {} from dump log", pid);
                                    return Ok(Some(pid));
                                }
                            }
                        }
                    }
                }
            }
        }

        warn!("Could not determine original PID from checkpoint");
        Ok(None)
    }

    fn is_pid_in_use(&self, pid: u32) -> bool {
        let proc_path = format!("/proc/{}", pid);
        std::path::Path::new(&proc_path).exists()
    }

    fn kill_conflicting_process(&self, pid: u32) -> Result<()> {
        info!("Attempting to kill conflicting process with PID {}", pid);

        // First try SIGTERM
        let output = Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .output()
            .map_err(|e| {
                CriuCliError::ProcessError(format!("Failed to send SIGTERM to PID {}: {}", pid, e))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CriuCliError::ProcessError(format!(
                "Failed to kill process {}: {}", pid, stderr
            )));
        }

        // Wait a moment for graceful termination
        std::thread::sleep(std::time::Duration::from_millis(1000));

        // Check if process is still running
        if self.is_pid_in_use(pid) {
            warn!("Process {} did not terminate gracefully, using SIGKILL", pid);

            // Force kill with SIGKILL
            let output = Command::new("kill")
                .arg("-KILL")
                .arg(pid.to_string())
                .output()
                .map_err(|e| {
                    CriuCliError::ProcessError(format!("Failed to send SIGKILL to PID {}: {}", pid, e))
                })?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(CriuCliError::ProcessError(format!(
                    "Failed to force kill process {}: {}", pid, stderr
                )));
            }

            // Wait for force termination
            std::thread::sleep(std::time::Duration::from_millis(500));
        }

        // Final check
        if self.is_pid_in_use(pid) {
            return Err(CriuCliError::ProcessError(format!(
                "Process {} is still running after kill attempts", pid
            )));
        }

        Ok(())
    }

    fn backup_output_files(&self, pid: u32, checkpoint_dir: &Path) -> Result<()> {
        info!("Backing up output files for PID {}", pid);

        // Read the process's file descriptors to find output files
        let fd_dir = format!("/proc/{}/fd", pid);
        if let Ok(entries) = std::fs::read_dir(&fd_dir) {
            for entry in entries.flatten() {
                let fd_name = entry.file_name();
                let fd_str = fd_name.to_string_lossy();

                // Skip non-numeric entries
                if let Ok(fd_num) = fd_str.parse::<i32>() {
                    let fd_path = entry.path();

                    // Check if this fd points to a regular file (not stdin/stdout/stderr to TTY)
                    if let Ok(link_target) = std::fs::read_link(&fd_path) {
                        let target_str = link_target.to_string_lossy();

                        // Look for output files (not TTY devices)
                        if !target_str.contains("/dev/") &&
                           (target_str.contains(".log") || target_str.contains("/tmp/")) {

                            info!("Found output file fd {}: {}", fd_num, target_str);

                            // Create backup of the file
                            let backup_name = format!("backup_fd_{}.dat", fd_num);
                            let backup_path = checkpoint_dir.join(&backup_name);

                            if std::fs::copy(&*target_str, &backup_path).is_ok() {
                                info!("Backed up {} to {}", target_str, backup_name);

                                // Also save the file path for restoration
                                let path_file = checkpoint_dir.join(format!("backup_fd_{}.path", fd_num));
                                if let Err(e) = std::fs::write(&path_file, &*target_str) {
                                    warn!("Failed to save file path for fd {}: {}", fd_num, e);
                                }
                            } else {
                                warn!("Failed to backup file: {}", target_str);
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn restore_output_files(&self, checkpoint_dir: &Path) -> Result<()> {
        info!("Restoring output files from checkpoint");

        // Find all backup files
        if let Ok(entries) = std::fs::read_dir(checkpoint_dir) {
            for entry in entries.flatten() {
                let filename = entry.file_name();
                let filename_str = filename.to_string_lossy();

                // Look for backup data files
                if filename_str.starts_with("backup_fd_") && filename_str.ends_with(".dat") {
                    let fd_part = &filename_str[10..filename_str.len()-4]; // Remove "backup_fd_" and ".dat"

                    // Find corresponding path file
                    let path_file = checkpoint_dir.join(format!("backup_fd_{}.path", fd_part));
                    if let Ok(original_path) = std::fs::read_to_string(&path_file) {
                        let backup_path = entry.path();

                        info!("Restoring {} from backup", original_path.trim());

                        if let Err(e) = std::fs::copy(&backup_path, original_path.trim()) {
                            warn!("Failed to restore file {}: {}", original_path.trim(), e);
                        } else {
                            info!("Successfully restored file: {}", original_path.trim());
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn pause_process(&self, pid: u32) -> Result<()> {
        info!("Sending SIGSTOP to process {}", pid);

        let output = Command::new("kill")
            .arg("-STOP")
            .arg(pid.to_string())
            .output()
            .map_err(|e| {
                CriuCliError::ProcessError(format!("Failed to send SIGSTOP to PID {}: {}", pid, e))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CriuCliError::ProcessError(format!(
                "Failed to pause process {}: {}", pid, stderr
            )));
        }

        // Wait a moment for the process to be fully paused
        std::thread::sleep(std::time::Duration::from_millis(100));
        info!("Process {} paused successfully", pid);
        Ok(())
    }

    fn resume_process(&self, pid: u32) -> Result<()> {
        info!("Sending SIGCONT to process {}", pid);

        let output = Command::new("kill")
            .arg("-CONT")
            .arg(pid.to_string())
            .output()
            .map_err(|e| {
                CriuCliError::ProcessError(format!("Failed to send SIGCONT to PID {}: {}", pid, e))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CriuCliError::ProcessError(format!(
                "Failed to resume process {}: {}", pid, stderr
            )));
        }

        info!("Process {} resumed successfully", pid);
        Ok(())
    }

    async fn find_newest_process_by_name(&self, process_name: &str) -> Option<u32> {
        info!("Looking for process with name: {}", process_name);

        let output = Command::new("ps")
            .arg("aux")
            .output()
            .ok()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut candidates = Vec::new();

        for line in stdout.lines() {
            if line.contains(process_name) && !line.contains("ps aux") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    if let Ok(pid) = parts[1].parse::<u32>() {
                        info!("Found candidate process: PID {} - {}", pid, line);
                        candidates.push(pid);
                    }
                }
            }
        }

        // Return the highest PID (most recently created)
        candidates.into_iter().max()
    }
}
