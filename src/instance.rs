use crate::criu_manager::CriuManager;
use crate::process_manager::ProcessManager;
use crate::types::{CriuCliError, Instance, InstanceStatus, Result, StartMode};
use crate::colors::ColorScheme;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use tracing::{error, info, warn};
use uuid::Uuid;

pub struct InstanceManager {
    instances: HashMap<Uuid, Instance>,
    instance_by_short_id: HashMap<String, Uuid>,
}

impl InstanceManager {
    pub fn new() -> Self {
        Self {
            instances: HashMap::new(),
            instance_by_short_id: HashMap::new(),
        }
    }

    pub async fn start_instance(
        &mut self,
        program: String,
        args: Vec<String>,
        process_manager: Arc<ProcessManager>,
    ) -> Result<String> {
        let working_dir = env::current_dir().map_err(CriuCliError::IoError)?;
        let mut instance = Instance::new(program.clone(), args.clone(), working_dir);

        info!("Starting instance: {} {}", program, args.join(" "));

        // Start the process
        match process_manager
            .start_process(instance.id, &program, &args, &instance.working_dir)
            .await
        {
            Ok(pid) => {
                instance.pid = Some(pid);
                instance.status = InstanceStatus::Running;
                info!("Instance {} started with PID: {}", instance.short_id(), pid);
            }
            Err(e) => {
                instance.status = InstanceStatus::Failed;
                error!("Failed to start instance {}: {}", instance.short_id(), e);
                return Err(e);
            }
        }

        let short_id = instance.short_id();
        let instance_id = instance.id;

        // Save instance metadata
        if let Err(e) = instance.save_metadata() {
            warn!("Failed to save instance metadata: {}", e);
        }

        self.instances.insert(instance_id, instance);
        self.instance_by_short_id.insert(short_id.clone(), instance_id);

        Ok(short_id)
    }

    pub async fn start_instance_detached(
        &mut self,
        program: String,
        args: Vec<String>,
        process_manager: Arc<ProcessManager>,
    ) -> Result<String> {
        let working_dir = env::current_dir().map_err(CriuCliError::IoError)?;
        let mut instance = Instance::new_with_mode(program.clone(), args.clone(), working_dir, StartMode::Detached);

        info!("Starting detached instance: {} {}", program, args.join(" "));

        // Start the process in detached mode
        match process_manager
            .start_process_with_mode(instance.id, &program, &args, &instance.working_dir, StartMode::Detached)
            .await
        {
            Ok(pid) => {
                instance.pid = Some(pid);
                instance.status = InstanceStatus::Running;
                info!("Detached instance {} started with PID: {}", instance.short_id(), pid);
            }
            Err(e) => {
                instance.status = InstanceStatus::Failed;
                error!("Failed to start detached instance {}: {}", instance.short_id(), e);
                return Err(e);
            }
        }

        let short_id = instance.short_id();
        let instance_id = instance.id;

        // Save instance metadata
        if let Err(e) = instance.save_metadata() {
            warn!("Failed to save instance metadata: {}", e);
        }

        self.instances.insert(instance_id, instance);
        self.instance_by_short_id.insert(short_id.clone(), instance_id);

        Ok(short_id)
    }

    pub async fn stop_instance(
        &mut self,
        instance_id_str: &str,
        process_manager: Arc<ProcessManager>,
    ) -> Result<()> {
        let instance_id = self.resolve_instance_id(instance_id_str)?;

        // First, get the instance info we need without holding a mutable reference
        let (is_detached, stored_pid, program_name, short_id) = {
            if let Some(instance) = self.instances.get(&instance_id) {
                if instance.status != InstanceStatus::Running && instance.status != InstanceStatus::Paused {
                    return Err(CriuCliError::InstanceNotRunning(instance_id_str.to_string()));
                }
                (instance.start_mode == StartMode::Detached, instance.pid, instance.program.clone(), instance.short_id())
            } else {
                return Err(CriuCliError::InstanceNotFound(instance_id_str.to_string()));
            }
        };

        info!("Stopping instance: {}", short_id);

        // Now determine the actual PID to stop
        let actual_pid = if is_detached {
            if let Some(stored_pid) = stored_pid {
                // Try to find the actual running process by name
                if let Some(real_pid) = self.find_actual_process_pid(&program_name).await {
                    if real_pid != stored_pid {
                        warn!("PID mismatch for instance {}: stored {} vs actual {}", short_id, stored_pid, real_pid);
                        // Update the stored PID in the instance
                        if let Some(instance) = self.instances.get_mut(&instance_id) {
                            instance.pid = Some(real_pid);
                        }
                    }
                    real_pid
                } else {
                    stored_pid // Fallback to stored PID
                }
            } else {
                return Err(CriuCliError::ProcessError("Detached instance has no PID".to_string()));
            }
        } else {
            stored_pid.unwrap_or(0) // Will use instance-based stopping anyway
        };

        // Now perform the actual stop operation
        let result = if is_detached {
            process_manager.stop_detached_process(actual_pid).await
        } else {
            process_manager.stop_process(&instance_id).await
        };

        // Finally, update the instance state
        if let Some(instance) = self.instances.get_mut(&instance_id) {
            match result {
                Ok(()) => {
                    instance.status = InstanceStatus::Stopped;
                    instance.pid = None;
                    info!("Instance {} stopped successfully", instance.short_id());
                    Ok(())
                }
                Err(e) => {
                    instance.status = InstanceStatus::Failed;
                    error!("Failed to stop instance {}: {}", instance.short_id(), e);
                    Err(e)
                }
            }
        } else {
            Err(CriuCliError::InstanceNotFound(instance_id_str.to_string()))
        }
    }

    pub async fn pause_instance(
        &mut self,
        instance_id_str: &str,
        process_manager: Arc<ProcessManager>,
    ) -> Result<()> {
        let instance_id = self.resolve_instance_id(instance_id_str)?;

        if let Some(instance) = self.instances.get_mut(&instance_id) {
            if instance.status != InstanceStatus::Running {
                return Err(CriuCliError::InstanceNotRunning(instance_id_str.to_string()));
            }

            info!("Pausing instance: {}", instance.short_id());

            match process_manager.pause_process(&instance_id).await {
                Ok(()) => {
                    instance.status = InstanceStatus::Paused;
                    info!("Instance {} paused successfully", instance.short_id());
                    Ok(())
                }
                Err(e) => {
                    error!("Failed to pause instance {}: {}", instance.short_id(), e);
                    Err(e)
                }
            }
        } else {
            Err(CriuCliError::InstanceNotFound(instance_id_str.to_string()))
        }
    }

    pub async fn resume_instance(
        &mut self,
        instance_id_str: &str,
        process_manager: Arc<ProcessManager>,
    ) -> Result<()> {
        let instance_id = self.resolve_instance_id(instance_id_str)?;

        if let Some(instance) = self.instances.get_mut(&instance_id) {
            if instance.status != InstanceStatus::Paused {
                return Err(CriuCliError::InstanceNotPaused(instance_id_str.to_string()));
            }

            info!("Resuming instance: {}", instance.short_id());

            match process_manager.resume_process(&instance_id).await {
                Ok(()) => {
                    instance.status = InstanceStatus::Running;
                    info!("Instance {} resumed successfully", instance.short_id());
                    Ok(())
                }
                Err(e) => {
                    error!("Failed to resume instance {}: {}", instance.short_id(), e);
                    Err(e)
                }
            }
        } else {
            Err(CriuCliError::InstanceNotFound(instance_id_str.to_string()))
        }
    }

    pub async fn checkpoint_instance(
        &mut self,
        instance_id_str: &str,
        checkpoint_name: &str,
        criu_manager: Arc<CriuManager>,
        process_manager: Arc<ProcessManager>,
    ) -> Result<()> {
        let instance_id = self.resolve_instance_id(instance_id_str)?;

        if let Some(instance) = self.instances.get_mut(&instance_id) {
            if instance.status != InstanceStatus::Running {
                return Err(CriuCliError::InstanceNotRunning(instance_id_str.to_string()));
            }

            let pid = instance.pid.ok_or_else(|| {
                CriuCliError::ProcessError("Instance has no PID".to_string())
            })?;

            info!("Creating checkpoint '{}' for instance: {}", checkpoint_name, instance.short_id());

            // Get output history from process manager
            let output_history = process_manager.get_output_history(&instance_id).await;

            // Use instance's dedicated checkpoints directory
            let checkpoint_dir = instance.checkpoints_dir().join(checkpoint_name);

            match criu_manager
                .create_checkpoint_in_dir(pid, checkpoint_name, &checkpoint_dir, &instance_id, output_history)
                .await
            {
                Ok(checkpoint_dir) => {
                    instance.add_checkpoint(checkpoint_name.to_string(), checkpoint_dir);

                    // Save updated instance metadata
                    if let Err(e) = instance.save_metadata() {
                        warn!("Failed to save instance metadata after checkpoint: {}", e);
                    }

                    info!("Checkpoint '{}' created for instance {}", checkpoint_name, instance.short_id());
                    Ok(())
                }
                Err(e) => {
                    error!("Failed to create checkpoint for instance {}: {}", instance.short_id(), e);
                    Err(e)
                }
            }
        } else {
            Err(CriuCliError::InstanceNotFound(instance_id_str.to_string()))
        }
    }

    pub async fn restore_instance_to_existing(
        &mut self,
        instance_id_str: &str,
        checkpoint_name: &str,
        criu_manager: Arc<CriuManager>,
        process_manager: Arc<ProcessManager>,
    ) -> Result<()> {
        let instance_id = self.resolve_instance_id(instance_id_str)?;

        // Check if instance exists
        if !self.instances.contains_key(&instance_id) {
            return Err(CriuCliError::InstanceNotFound(instance_id_str.to_string()));
        }

        info!("Restoring instance {} from checkpoint: {}", instance_id_str, checkpoint_name);

        // Step 1: Stop the current process if it's running
        {
            let instance = self.instances.get(&instance_id).unwrap();
            if instance.status == InstanceStatus::Running {
                info!("Stopping current process before restore");
                if let Err(e) = self.stop_instance(instance_id_str, process_manager.clone()).await {
                    warn!("Failed to stop current process: {}, continuing with restore", e);
                }
            }
        }

        // Step 2: Restore from checkpoint using the specific instance
        match criu_manager.restore_checkpoint(checkpoint_name, Some(&instance_id)).await {
            Ok((pid, _output_history)) => {
                // Step 3: Update the instance with new PID and status
                if let Some(instance) = self.instances.get_mut(&instance_id) {
                    instance.pid = Some(pid);
                    instance.status = InstanceStatus::Running;
                    info!("Updated instance {} with restored PID {}", instance.short_id(), pid);
                } else {
                    return Err(CriuCliError::InstanceNotFound(instance_id_str.to_string()));
                }

                // Step 4: Register the restored process with the process manager
                if let Err(e) = process_manager.register_restored_process(instance_id, pid, None).await {
                    error!("Failed to register restored process: {}", e);
                    return Err(e);
                }

                info!("Instance {} restored from checkpoint '{}'", instance_id_str, checkpoint_name);
                Ok(())
            }
            Err(e) => {
                error!("Failed to restore checkpoint '{}': {}", checkpoint_name, e);
                // Mark instance as failed
                if let Some(instance) = self.instances.get_mut(&instance_id) {
                    instance.status = InstanceStatus::Failed;
                    instance.pid = None;
                }
                Err(e)
            }
        }
    }

    pub async fn restore_instance(
        &mut self,
        checkpoint_name: &str,
        criu_manager: Arc<CriuManager>,
        process_manager: Arc<ProcessManager>,
    ) -> Result<String> {
        if !criu_manager.checkpoint_exists(checkpoint_name) {
            return Err(CriuCliError::CheckpointNotFound(checkpoint_name.to_string()));
        }

        info!("Restoring instance from checkpoint: {}", checkpoint_name);

        match criu_manager.restore_checkpoint(checkpoint_name, None).await {
            Ok((pid, output_history)) => {
                // Try to find the original instance that created this checkpoint FIRST
                let original_instance_info = self.find_instance_with_checkpoint(checkpoint_name);

                let (instance_id, short_id) = if let Some((original_id, _)) = original_instance_info {
                    // Clean up any OTHER instances with the same PID (but keep the original)
                    self.cleanup_instances_with_pid_except(pid, original_id);

                    // Update the original instance
                    if let Some(instance) = self.instances.get_mut(&original_id) {
                        instance.pid = Some(pid);
                        instance.status = InstanceStatus::Running;
                        let short_id = instance.short_id();
                        info!("Updated original instance {} with restored PID {}", short_id, pid);
                        (original_id, short_id)
                    } else {
                        // Fallback if instance was somehow removed
                        self.create_new_restored_instance(checkpoint_name, pid)?
                    }
                } else {
                    // Clean up any existing instances with the same PID
                    self.cleanup_instances_with_pid(pid);

                    // Create a new instance if we can't find the original
                    self.create_new_restored_instance(checkpoint_name, pid)?
                };

                // Register the restored process with the process manager
                if let Err(e) = process_manager.register_restored_process(instance_id, pid, output_history).await {
                    error!("Failed to register restored process: {}", e);
                    return Err(e);
                }

                info!("Instance {} restored from checkpoint '{}'", short_id, checkpoint_name);
                Ok(short_id)
            }
            Err(e) => {
                error!("Failed to restore checkpoint '{}': {}", checkpoint_name, e);
                Err(e)
            }
        }
    }

    pub fn list_instances(&self) {
        if self.instances.is_empty() {
            println!("{}", ColorScheme::info("No instances running."));
            return;
        }

        // Print colorized header
        println!("{:<10} {:<12} {:<20} {:<8} {:<10} {:<30}",
            ColorScheme::table_header("ID"),
            ColorScheme::table_header("STATUS"),
            ColorScheme::table_header("PROGRAM"),
            ColorScheme::table_header("PID"),
            ColorScheme::table_header("MODE"),
            ColorScheme::table_header("CREATED")
        );
        println!("{}", ColorScheme::separator(90));

        // Track PIDs to detect conflicts
        let mut pid_usage: std::collections::HashMap<u32, Vec<String>> = std::collections::HashMap::new();

        // First pass: collect all PIDs and their instances
        for instance in self.instances.values() {
            if let Some(pid) = instance.pid {
                pid_usage.entry(pid).or_insert_with(Vec::new).push(instance.short_id());
            }
        }

        for instance in self.instances.values() {
            let pid_str = instance.pid.map_or("N/A".to_string(), |p| p.to_string());
            let created_str = instance.created_at.format("%Y-%m-%d %H:%M:%S").to_string();
            let mode_str = match instance.start_mode {
                StartMode::Normal => "Normal",
                StartMode::Detached => "Detached",
            };

            // Check if process is actually running and handle PID conflicts
            let actual_status = if let Some(pid) = instance.pid {
                // Check for PID conflicts
                if let Some(instances_with_pid) = pid_usage.get(&pid) {
                    if instances_with_pid.len() > 1 {
                        // Multiple instances claim the same PID
                        if self.is_pid_running(pid) {
                            // Only one can actually be running
                            "Conflict".to_string()
                        } else {
                            "Stopped".to_string()
                        }
                    } else if self.is_pid_running(pid) {
                        instance.status.to_string()
                    } else {
                        "Stopped".to_string()
                    }
                } else {
                    "Stopped".to_string()
                }
            } else {
                instance.status.to_string()
            };

            println!(
                "{:<10} {:<12} {:<20} {:<8} {:<10} {:<30}",
                ColorScheme::instance_id(&instance.short_id()),
                ColorScheme::format_status(&actual_status),
                ColorScheme::program(&instance.program),
                if pid_str == "N/A" { pid_str } else { ColorScheme::pid(&pid_str) },
                ColorScheme::format_mode(mode_str),
                ColorScheme::timestamp(&created_str)
            );
        }

        // Show warnings for PID conflicts
        for (pid, instances) in pid_usage.iter() {
            if instances.len() > 1 {
                println!("\n⚠️  Warning: PID {} is claimed by multiple instances: {}",
                         pid, instances.join(", "));
                println!("   This indicates a state inconsistency. Only one instance can actually be running.");
            }
        }
    }

    fn is_pid_running(&self, pid: u32) -> bool {
        let proc_path = format!("/proc/{}", pid);
        std::path::Path::new(&proc_path).exists()
    }

    fn cleanup_instances_with_pid(&mut self, pid: u32) {
        let mut instances_to_remove = Vec::new();

        for (instance_id, instance) in &self.instances {
            if instance.pid == Some(pid) {
                info!("Cleaning up conflicting instance {} with PID {}", instance.short_id(), pid);
                instances_to_remove.push(*instance_id);
            }
        }

        for instance_id in instances_to_remove {
            if let Some(instance) = self.instances.remove(&instance_id) {
                self.instance_by_short_id.remove(&instance.short_id());
                info!("Removed conflicting instance {}", instance.short_id());
            }
        }
    }

    fn cleanup_instances_with_pid_except(&mut self, pid: u32, except_instance_id: Uuid) {
        let mut instances_to_remove = Vec::new();

        for (instance_id, instance) in &self.instances {
            if instance.pid == Some(pid) && *instance_id != except_instance_id {
                info!("Cleaning up conflicting instance {} with PID {} (except {})", instance.short_id(), pid, except_instance_id);
                instances_to_remove.push(*instance_id);
            }
        }

        for instance_id in instances_to_remove {
            if let Some(instance) = self.instances.remove(&instance_id) {
                self.instance_by_short_id.remove(&instance.short_id());
                info!("Removed conflicting instance {}", instance.short_id());
            }
        }
    }

    fn find_instance_with_checkpoint(&self, checkpoint_name: &str) -> Option<(Uuid, &Instance)> {
        for (instance_id, instance) in &self.instances {
            if instance.checkpoints.contains_key(checkpoint_name) {
                info!("Found original instance {} that created checkpoint '{}'", instance.short_id(), checkpoint_name);
                return Some((*instance_id, instance));
            }
        }

        warn!("Could not find original instance that created checkpoint '{}'", checkpoint_name);
        None
    }

    fn create_new_restored_instance(&mut self, checkpoint_name: &str, pid: u32) -> Result<(Uuid, String)> {
        let working_dir = env::current_dir().map_err(CriuCliError::IoError)?;
        let mut instance = Instance::new(
            format!("restored-{}", checkpoint_name),
            vec![],
            working_dir,
        );

        instance.pid = Some(pid);
        instance.status = InstanceStatus::Running;

        let short_id = instance.short_id();
        let instance_id = instance.id;

        self.instances.insert(instance_id, instance);
        self.instance_by_short_id.insert(short_id.clone(), instance_id);

        info!("Created new instance {} for restored checkpoint", short_id);
        Ok((instance_id, short_id))
    }

    pub fn has_instance(&self, instance_id_str: &str) -> bool {
        self.resolve_instance_id(instance_id_str).is_ok()
    }

    pub fn resolve_instance_id(&self, instance_id_str: &str) -> Result<Uuid> {
        // Try to parse as full UUID first
        if let Ok(uuid) = Uuid::parse_str(instance_id_str) {
            if self.instances.contains_key(&uuid) {
                return Ok(uuid);
            }
        }

        // Try to resolve as short ID
        if let Some(&uuid) = self.instance_by_short_id.get(instance_id_str) {
            return Ok(uuid);
        }

        Err(CriuCliError::InstanceNotFound(instance_id_str.to_string()))
    }

    async fn find_actual_process_pid(&self, program_path: &str) -> Option<u32> {
        // Extract just the program name without path
        let program_basename = std::path::Path::new(program_path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(program_path);

        info!("Looking for actual PID of process: {}", program_basename);

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

                            // Exact match for the program name
                            if cmd_basename == program_basename && !cmd.contains("sh") && !cmd.contains("bash") {
                                info!("Found actual process: PID {} with cmdline: {}", pid, cmdline.replace('\0', " "));
                                return Some(pid);
                            }
                        }
                    }
                }
            }
        }

        warn!("No actual process found with name: {}", program_basename);
        None
    }
}
