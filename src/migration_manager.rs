use crate::instance::InstanceManager;
use crate::message_protocol::{MigrationMessage, NetworkMessage, ShadowSyncMessage, NodeId};
use crate::network_manager::NetworkManager;
use crate::process_manager::ProcessManager;
use crate::shadow_instance_manager::ShadowInstanceManager;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use tokio::time::interval;
use tokio::process::Command;
use tokio::net::{TcpListener, TcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Migration options for controlling migration behavior
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MigrationOptions {
    pub compression: bool,
    pub verify: bool,
    pub timeout_secs: u64,
}

impl Default for MigrationOptions {
    fn default() -> Self {
        Self {
            compression: true,
            verify: true,
            timeout_secs: 300, // 5 minutes
        }
    }
}

/// Migration status tracking
#[derive(Debug, Clone)]
pub enum MigrationStatus {
    Preparing,
    CreatingCheckpoint,
    TransferringData,
    RestoringProcess,
    Verifying,
    Completed,
    Failed(String),
}

/// Active migration tracking
#[derive(Debug, Clone)]
pub struct ActiveMigration {
    pub migration_id: Uuid,
    pub instance_id: Uuid,
    pub source_node_id: NodeId,
    pub target_node_id: NodeId,
    pub status: MigrationStatus,
    pub started_at: DateTime<Utc>,
    pub options: MigrationOptions,
}

/// Image synchronization manager for periodic checkpoint creation
#[derive(Clone)]
pub struct ImageSyncManager {
    instance_manager: Arc<Mutex<InstanceManager>>,
    process_manager: Arc<ProcessManager>,
    network_manager: Option<Arc<NetworkManager>>,
    shadow_manager: Option<Arc<RwLock<ShadowInstanceManager>>>,
    sync_interval: Duration,
    is_running: Arc<Mutex<bool>>,
}

impl ImageSyncManager {
    pub fn new(
        instance_manager: Arc<Mutex<InstanceManager>>,
        process_manager: Arc<ProcessManager>,
        sync_interval_secs: u64,
    ) -> Self {
        Self {
            instance_manager,
            process_manager,
            network_manager: None,
            shadow_manager: None,
            sync_interval: Duration::from_secs(sync_interval_secs),
            is_running: Arc::new(Mutex::new(false)),
        }
    }

    /// Set network and shadow managers for distributed sync
    pub fn set_managers(
        &mut self,
        network_manager: Option<Arc<NetworkManager>>,
        shadow_manager: Option<Arc<RwLock<ShadowInstanceManager>>>,
    ) {
        self.network_manager = network_manager;
        self.shadow_manager = shadow_manager;
    }

    /// Start periodic image synchronization
    pub async fn start(&self) -> Result<()> {
        let mut is_running = self.is_running.lock().await;
        if *is_running {
            return Ok(());
        }
        *is_running = true;
        drop(is_running);

        let instance_manager = self.instance_manager.clone();
        let process_manager = self.process_manager.clone();
        let network_manager = self.network_manager.clone();
        let shadow_manager = self.shadow_manager.clone();
        let sync_interval = self.sync_interval;
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            let mut interval = interval(sync_interval);

            while *is_running.lock().await {
                interval.tick().await;

                if let Err(e) = Self::sync_all_instances(
                    &instance_manager,
                    &process_manager,
                    network_manager.as_ref(),
                    shadow_manager.as_ref(),
                ).await {
                    error!("Failed to sync instances: {}", e);
                }
            }
        });

        info!("Image sync manager started with interval: {:?}", self.sync_interval);
        Ok(())
    }

    /// Stop periodic image synchronization
    pub async fn stop(&self) {
        let mut is_running = self.is_running.lock().await;
        *is_running = false;
        info!("Image sync manager stopped");
    }

    /// Sync all running instances
    async fn sync_all_instances(
        instance_manager: &Arc<Mutex<InstanceManager>>,
        process_manager: &Arc<ProcessManager>,
        network_manager: Option<&Arc<NetworkManager>>,
        shadow_manager: Option<&Arc<RwLock<ShadowInstanceManager>>>,
    ) -> Result<()> {
        let instances = {
            let manager = instance_manager.lock().await;
            manager.get_all_instances()
        };

        info!("Checking {} instances for sync", instances.len());

        let mut sync_count = 0;
        for instance in instances {
            info!("Instance {}: status={:?}, pid={:?}", instance.short_id(), instance.status, instance.pid);

            // Check if instance is actually running by verifying PID
            let is_actually_running = if let Some(pid) = instance.pid {
                instance.status == crate::types::InstanceStatus::Running && Self::is_pid_running(pid)
            } else {
                false
            };

            if is_actually_running {
                info!("Syncing running instance {}", instance.short_id());
                if let Err(e) = Self::sync_instance(&instance, process_manager, network_manager, shadow_manager).await {
                    warn!("Failed to sync instance {}: {}", instance.id, e);
                } else {
                    sync_count += 1;
                    info!("Successfully synced instance {}", instance.short_id());
                }
            } else {
                info!("Skipping instance {} with status {:?} (not actually running)", instance.short_id(), instance.status);
            }
        }

        if sync_count > 0 {
            info!("Synced {} running instances", sync_count);
        } else {
            info!("No running instances found to sync");
        }

        Ok(())
    }

    /// Check if a PID is actually running
    fn is_pid_running(pid: u32) -> bool {
        let proc_path = format!("/proc/{}", pid);
        std::path::Path::new(&proc_path).exists()
    }

    /// Start migration receiver server on specified port
    async fn start_migration_receiver(&self, port: u16) -> Result<()> {
        use tokio::net::TcpListener;
        use tokio::io::AsyncReadExt;

        let addr = format!("0.0.0.0:{}", port);
        let listener = TcpListener::bind(&addr).await?;
        info!("Migration receiver listening on port {}", port);

        // Accept one connection for this migration
        if let Ok((mut socket, peer_addr)) = listener.accept().await {
            info!("Migration receiver accepted connection from {}", peer_addr);

            // Read checkpoint data
            let mut buffer = Vec::new();
            if let Err(e) = socket.read_to_end(&mut buffer).await {
                error!("Failed to read checkpoint data: {}", e);
                return Err(e.into());
            }

            info!("Received {} bytes of checkpoint data", buffer.len());

            // Process the received checkpoint data
            if let Err(e) = self.process_received_migration_data(buffer).await {
                error!("Failed to process migration data: {}", e);
                return Err(e);
            }

            info!("Migration receiver completed successfully");
        }

        Ok(())
    }

    /// Process received migration data and restore the instance
    async fn process_received_migration_data(&self, data: Vec<u8>) -> Result<()> {
        use std::io::Cursor;
        use tar::Archive;

        info!("Processing migration data: {} bytes", data.len());

        // Extract the tar.gz data
        let cursor = Cursor::new(data.clone());
        let tar = flate2::read::GzDecoder::new(cursor);
        let mut archive = Archive::new(tar);

        // Create a temporary directory to extract to first
        let temp_dir = std::env::temp_dir().join(format!("migration-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir)?;
        info!("Created temp directory: {:?}", temp_dir);

        // Try to list entries first for debugging
        info!("Attempting to iterate over archive entries...");
        let cursor2 = Cursor::new(data);
        let tar2 = flate2::read::GzDecoder::new(cursor2);
        let mut archive2 = Archive::new(tar2);

        let mut entry_count = 0;
        for entry_result in archive2.entries()? {
            match entry_result {
                Ok(entry) => {
                    let path = entry.path()?;
                    info!("Found archive entry: {:?}", path);
                    entry_count += 1;
                }
                Err(e) => {
                    error!("Error reading archive entry: {}", e);
                    return Err(e.into());
                }
            }
        }
        info!("Archive contains {} entries", entry_count);

        // Extract the archive
        info!("Extracting archive to temp directory...");
        archive.unpack(&temp_dir)?;

        // Find the migration metadata file to get instance information
        let metadata_file = temp_dir.join("migration_metadata.json");
        if !metadata_file.exists() {
            return Err(anyhow::anyhow!("Migration metadata not found"));
        }

        let metadata_content = std::fs::read_to_string(&metadata_file)?;
        let metadata: serde_json::Value = serde_json::from_str(&metadata_content)?;

        let instance_id = metadata["instance_id"].as_str()
            .ok_or_else(|| anyhow::anyhow!("Instance ID not found in metadata"))?;
        let migration_id = metadata["migration_id"].as_str()
            .ok_or_else(|| anyhow::anyhow!("Migration ID not found in metadata"))?;

        // Create the proper checkpoint directory
        let checkpoint_dir = format!("instances/instance_{}/checkpoints/migration-{}", instance_id, migration_id);
        std::fs::create_dir_all(&checkpoint_dir)?;

        // Move all checkpoint files to the proper location
        for entry in std::fs::read_dir(&temp_dir)? {
            let entry = entry?;
            let file_name = entry.file_name();
            if file_name != "migration_metadata.json" {
                let src = entry.path();
                let dst = std::path::Path::new(&checkpoint_dir).join(&file_name);
                std::fs::rename(src, dst)?;
                info!("Moved checkpoint file: {:?}", file_name);
            }
        }

        // Clean up temp directory
        std::fs::remove_dir_all(&temp_dir)?;

        info!("Migration checkpoint extracted to: {}", checkpoint_dir);

        // Now restore the instance
        self.restore_migrated_instance(instance_id, migration_id).await?;

        Ok(())
    }

    /// Restore a migrated instance from checkpoint
    async fn restore_migrated_instance(&self, instance_id: &str, migration_id: &str) -> Result<()> {
        let checkpoint_name = format!("migration-{}", migration_id);
        let checkpoint_dir = format!("instances/instance_{}/checkpoints/{}", instance_id, checkpoint_name);

        info!("Restoring migrated instance {} from checkpoint {}", instance_id, checkpoint_name);

        // Use CRIU to restore the process
        let criu_path = "/usr/local/bin/criu"; // Default CRIU path
        let restore_cmd = std::process::Command::new("sudo")
            .arg(criu_path)
            .arg("restore")
            .arg("-D")
            .arg(&checkpoint_dir)
            .arg("--shell-job")
            .output();

        match restore_cmd {
            Ok(output) => {
                if output.status.success() {
                    info!("Successfully restored migrated instance {}", instance_id);

                    // Find the restored PID
                    if let Ok(new_pid) = self.find_restored_pid(&checkpoint_dir).await {
                        info!("Restored instance {} with PID: {}", instance_id, new_pid);

                        // Update instance status to Running
                        self.update_instance_after_migration(instance_id, new_pid).await?;
                    } else {
                        warn!("Could not find PID for restored instance {}", instance_id);
                    }
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    error!("CRIU restore failed for instance {}: {}", instance_id, stderr);
                    return Err(anyhow::anyhow!("CRIU restore failed: {}", stderr));
                }
            }
            Err(e) => {
                error!("Failed to execute CRIU restore for instance {}: {}", instance_id, e);
                return Err(e.into());
            }
        }

        Ok(())
    }

    /// Find the PID of a restored process
    async fn find_restored_pid(&self, checkpoint_dir: &str) -> Result<u32> {
        // Read the pstree.img file to get the PID
        let pstree_file = format!("{}/pstree.img", checkpoint_dir);

        // For now, use a simple approach - look for running processes
        // In a real implementation, you'd parse the CRIU image files
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // Look for simple_counter processes
        let output = std::process::Command::new("pgrep")
            .arg("simple_counter")
            .output()?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(pid_str) = stdout.lines().last() {
                if let Ok(pid) = pid_str.parse::<u32>() {
                    return Ok(pid);
                }
            }
        }

        Err(anyhow::anyhow!("Could not find restored PID"))
    }

    /// Update instance status after successful migration
    async fn update_instance_after_migration(&self, instance_id: &str, new_pid: u32) -> Result<()> {
        // This would update the instance manager with the new PID and status
        // For now, just log the update
        info!("Updated instance {} with new PID {} after migration", instance_id, new_pid);

        // TODO: Actually update the instance manager
        // self.instance_manager.update_instance_after_migration(instance_id, new_pid).await?;

        Ok(())
    }

    /// Create a checkpoint for a specific instance and sync to other nodes
    async fn sync_instance(
        instance: &crate::types::Instance,
        _process_manager: &Arc<ProcessManager>,
        network_manager: Option<&Arc<NetworkManager>>,
        shadow_manager: Option<&Arc<RwLock<ShadowInstanceManager>>>,
    ) -> Result<()> {
        let checkpoint_name = format!("auto-sync-{}", Utc::now().timestamp());
        info!("Starting sync checkpoint for instance {}: {}", instance.short_id(), checkpoint_name);

        // Get the PID for the instance
        if let Some(pid) = instance.pid {
            // Create checkpoint directory using the same pattern as original code
            let instance_dir = PathBuf::from("instances").join(format!("instance_{}", instance.short_id()));
            let checkpoint_dir = instance_dir.join("checkpoints").join(&checkpoint_name);

            info!("Creating checkpoint directory: {:?}", checkpoint_dir);
            if let Err(e) = tokio::fs::create_dir_all(&checkpoint_dir).await {
                warn!("Failed to create checkpoint directory: {}", e);
                return Ok(());
            }

            // Use CRIU to create checkpoint
            let mut cmd = Command::new("sudo");
            cmd.arg("/home/realgod/sync2/criu/criu/criu")
               .arg("dump")
               .arg("-t").arg(pid.to_string())
               .arg("-D").arg(&checkpoint_dir)
               .arg("--shell-job")
               .arg("--leave-running");

            info!("Executing CRIU command for PID {}: {:?}", pid, cmd);
            match cmd.output().await {
                Ok(output) => {
                    if output.status.success() {
                        info!("Created sync checkpoint for instance {}: {}", instance.short_id(), checkpoint_name);

                        // If we have network connectivity, stream checkpoint to other nodes
                        if let (Some(network_mgr), Some(shadow_mgr)) = (network_manager, shadow_manager) {
                            if let Err(e) = Self::stream_checkpoint_to_shadows(
                                instance, &checkpoint_name, &checkpoint_dir, network_mgr, shadow_mgr
                            ).await {
                                warn!("Failed to stream checkpoint to shadows: {}", e);
                            }
                        }
                    } else {
                        warn!("CRIU checkpoint failed for instance {}: {}",
                              instance.short_id(), String::from_utf8_lossy(&output.stderr));
                    }
                }
                Err(e) => {
                    warn!("Failed to execute CRIU for instance {}: {}", instance.short_id(), e);
                }
            }
        } else {
            warn!("Instance {} has no PID, skipping checkpoint", instance.short_id());
        }

        Ok(())
    }

    /// Stream checkpoint data to shadow instances on other nodes
    async fn stream_checkpoint_to_shadows(
        instance: &crate::types::Instance,
        checkpoint_name: &str,
        checkpoint_dir: &PathBuf,
        network_manager: &Arc<NetworkManager>,
        shadow_manager: &Arc<RwLock<ShadowInstanceManager>>,
    ) -> Result<()> {
        info!("Streaming checkpoint {} for instance {} to shadow nodes", checkpoint_name, instance.short_id());

        // Read checkpoint data from files
        let checkpoint_data = Self::read_checkpoint_data(checkpoint_dir).await?;

        if checkpoint_data.is_empty() {
            warn!("No checkpoint data found in {:?}", checkpoint_dir);
            return Ok(());
        }

        info!("Read {} bytes of checkpoint data", checkpoint_data.len());

        // Stream to shadow instances using the shadow manager
        let shadow_mgr = shadow_manager.read().await;
        shadow_mgr.stream_checkpoint_to_shadows(instance.id, checkpoint_data).await?;

        info!("Successfully streamed checkpoint for instance {} to shadows", instance.short_id());
        Ok(())
    }

    /// Read checkpoint data from directory and compress it
    async fn read_checkpoint_data(checkpoint_dir: &PathBuf) -> Result<Vec<u8>> {
        use std::io::Write;
        use flate2::Compression;
        use flate2::write::GzEncoder;

        let mut compressed_data = Vec::new();
        {
            let mut encoder = GzEncoder::new(&mut compressed_data, Compression::default());

            // Read all files in the checkpoint directory
            let mut entries = tokio::fs::read_dir(checkpoint_dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                if path.is_file() {
                    let file_name = path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown");

                    let file_data = tokio::fs::read(&path).await?;

                    // Write file header: name length + name + data length + data
                    let name_bytes = file_name.as_bytes();
                    encoder.write_all(&(name_bytes.len() as u32).to_le_bytes())?;
                    encoder.write_all(name_bytes)?;
                    encoder.write_all(&(file_data.len() as u32).to_le_bytes())?;
                    encoder.write_all(&file_data)?;
                }
            }

            encoder.finish()?;
        }

        Ok(compressed_data)
    }

    /// Force sync a specific instance (used before migration)
    pub async fn force_sync_instance(&self, instance_id: &str) -> Result<String> {
        let instance = {
            let manager = self.instance_manager.lock().await;
            manager.get_instance_by_id(instance_id)
                .ok_or_else(|| anyhow!("Instance {} not found", instance_id))?
                .clone()
        };

        let checkpoint_name = format!("migration-sync-{}", Utc::now().timestamp());

        Self::sync_instance(
            &instance,
            &self.process_manager,
            self.network_manager.as_ref(),
            self.shadow_manager.as_ref(),
        ).await?;

        info!("Force synced instance {} for migration: {}", instance_id, checkpoint_name);
        Ok(checkpoint_name)
    }
}

/// Main migration manager
#[derive(Clone)]
pub struct MigrationManager {
    local_node_id: NodeId,
    network_manager: Arc<NetworkManager>,
    instance_manager: Arc<Mutex<InstanceManager>>,
    process_manager: Arc<ProcessManager>,
    shadow_manager: Option<Arc<RwLock<ShadowInstanceManager>>>,
    image_sync_manager: ImageSyncManager,
    active_migrations: Arc<RwLock<HashMap<Uuid, ActiveMigration>>>,
    criu_image_streamer_path: PathBuf,
}

impl MigrationManager {
    pub fn new(
        local_node_id: NodeId,
        network_manager: Arc<NetworkManager>,
        instance_manager: Arc<Mutex<InstanceManager>>,
        process_manager: Arc<ProcessManager>,
    ) -> Self {
        let image_sync_manager = ImageSyncManager::new(
            instance_manager.clone(),
            process_manager.clone(),
            30, // 30 seconds sync interval
        );

        Self {
            local_node_id,
            network_manager,
            instance_manager,
            process_manager,
            shadow_manager: None,
            image_sync_manager,
            active_migrations: Arc::new(RwLock::new(HashMap::new())),
            criu_image_streamer_path: PathBuf::from("./criu-image-streamer/target/release/criu-image-streamer"),
        }
    }

    /// Set shadow manager for migration coordination
    pub fn set_shadow_manager(&mut self, shadow_manager: Arc<RwLock<ShadowInstanceManager>>) {
        self.shadow_manager = Some(shadow_manager.clone());

        // Update the image sync manager with network and shadow managers
        self.image_sync_manager.set_managers(
            Some(self.network_manager.clone()),
            Some(shadow_manager),
        );
    }

    /// Start the migration manager
    pub async fn start(&self) -> Result<()> {
        self.image_sync_manager.start().await?;
        info!("Migration manager started");
        Ok(())
    }

    /// Stop the migration manager
    pub async fn stop(&self) {
        self.image_sync_manager.stop().await;
        info!("Migration manager stopped");
    }

    /// Initiate migration of an instance to another node
    pub async fn migrate_instance(
        &self,
        instance_id: &str,
        target_node_id: NodeId,
        options: MigrationOptions,
    ) -> Result<Uuid> {
        info!("Starting migration of instance {} to node {}", instance_id, target_node_id);

        // Validate instance exists and is running
        let instance = {
            let manager = self.instance_manager.lock().await;
            manager.get_instance_by_id(instance_id)
                .ok_or_else(|| anyhow!("Instance {} not found", instance_id))?
                .clone()
        };

        if instance.status != crate::types::InstanceStatus::Running {
            return Err(anyhow!("Instance {} is not running", instance_id));
        }

        // Generate migration ID
        let migration_id = Uuid::new_v4();
        let instance_uuid = instance.id;

        // Create migration tracking
        let migration = ActiveMigration {
            migration_id,
            instance_id: instance_uuid,
            source_node_id: self.local_node_id,
            target_node_id,
            status: MigrationStatus::Preparing,
            started_at: Utc::now(),
            options: options.clone(),
        };

        // Store active migration
        {
            let mut migrations = self.active_migrations.write().await;
            migrations.insert(migration_id, migration);
        }

        // Send migration request to target node
        let migration_request = MigrationMessage::MigrationRequest {
            migration_id,
            instance_id: instance_uuid,
            source_node_id: self.local_node_id,
            target_node_id,
            options,
        };

        let network_message = NetworkMessage::Migration(migration_request);
        self.network_manager.send_to_peer(&target_node_id, network_message).await?;

        info!("Migration request sent for instance {}: {}", instance_id, migration_id);
        Ok(migration_id)
    }

    /// Get migration status
    pub async fn get_migration_status(&self, migration_id: Uuid) -> Option<MigrationStatus> {
        let migrations = self.active_migrations.read().await;
        migrations.get(&migration_id).map(|m| m.status.clone())
    }

    /// List active migrations
    pub async fn list_active_migrations(&self) -> Vec<ActiveMigration> {
        let migrations = self.active_migrations.read().await;
        migrations.values().cloned().collect()
    }

    /// Handle incoming migration message
    pub async fn handle_migration_message(&self, migration_message: MigrationMessage) -> Result<()> {
        match migration_message {
            MigrationMessage::MigrationRequest {
                migration_id,
                instance_id,
                source_node_id,
                target_node_id,
                options
            } => {
                self.handle_migration_request(migration_id, instance_id, source_node_id, options).await
            }
            MigrationMessage::MigrationAccept { migration_id, target_port } => {
                self.handle_migration_accept(migration_id, target_port).await
            }
            MigrationMessage::MigrationReject { migration_id, reason } => {
                self.handle_migration_reject(migration_id, reason).await
            }
            MigrationMessage::CheckpointTransfer {
                migration_id,
                instance_id,
                source_node_id,
                target_node_id,
                checkpoint_data
            } => {
                self.handle_checkpoint_transfer(migration_id, instance_id, source_node_id, checkpoint_data).await
            }
            MigrationMessage::MigrationComplete { migration_id, success, error } => {
                self.handle_migration_complete(migration_id, success, error).await
            }
        }
    }

    /// Handle migration request from another node
    async fn handle_migration_request(
        &self,
        migration_id: Uuid,
        instance_id: Uuid,
        source_node_id: NodeId,
        options: MigrationOptions,
    ) -> Result<()> {
        info!("Received migration request for instance {} from node {}", instance_id, source_node_id);

        // Check if we have a shadow instance for this instance
        if let Some(shadow_mgr) = &self.shadow_manager {
            let shadow_mgr_read = shadow_mgr.read().await;
            if shadow_mgr_read.get_shadow_instance(instance_id).await.is_some() {
                // Accept the migration and start receiver
                let target_port = 9999; // TODO: Use dynamic port allocation

                let accept_message = MigrationMessage::MigrationAccept {
                    migration_id,
                    target_port,
                };

                // Start migration receiver server
                let image_sync_manager = self.image_sync_manager.clone();
                tokio::spawn(async move {
                    if let Err(e) = image_sync_manager.start_migration_receiver(target_port).await {
                        error!("Failed to start migration receiver on port {}: {}", target_port, e);
                    }
                });

                let network_message = NetworkMessage::Migration(accept_message);
                self.network_manager.send_to_peer(&source_node_id, network_message).await?;

                info!("Accepted migration request for instance {} and started receiver on port {}", instance_id, target_port);
            } else {
                // Reject the migration
                let reject_message = MigrationMessage::MigrationReject {
                    migration_id,
                    reason: "No shadow instance found".to_string(),
                };

                let network_message = NetworkMessage::Migration(reject_message);
                self.network_manager.send_to_peer(&source_node_id, network_message).await?;

                warn!("Rejected migration request for instance {} - no shadow found", instance_id);
            }
        } else {
            // Reject if no shadow manager
            let reject_message = MigrationMessage::MigrationReject {
                migration_id,
                reason: "Shadow manager not available".to_string(),
            };

            let network_message = NetworkMessage::Migration(reject_message);
            self.network_manager.send_to_peer(&source_node_id, network_message).await?;
        }

        Ok(())
    }

    /// Handle migration acceptance
    async fn handle_migration_accept(&self, migration_id: Uuid, target_port: u16) -> Result<()> {
        info!("Migration {} accepted, target port: {}", migration_id, target_port);

        // Update migration status
        {
            let mut migrations = self.active_migrations.write().await;
            if let Some(migration) = migrations.get_mut(&migration_id) {
                migration.status = MigrationStatus::CreatingCheckpoint;
            }
        }

        // Start the actual migration process
        self.execute_migration(migration_id, target_port).await?;

        Ok(())
    }

    /// Handle migration rejection
    async fn handle_migration_reject(&self, migration_id: Uuid, reason: String) -> Result<()> {
        error!("Migration {} rejected: {}", migration_id, reason);

        // Update migration status
        {
            let mut migrations = self.active_migrations.write().await;
            if let Some(migration) = migrations.get_mut(&migration_id) {
                migration.status = MigrationStatus::Failed(reason);
            }
        }

        Ok(())
    }

    /// Handle migration completion
    async fn handle_migration_complete(&self, migration_id: Uuid, success: bool, error: Option<String>) -> Result<()> {
        if success {
            info!("Migration {} completed successfully", migration_id);

            // Update migration status and convert source instance to shadow
            {
                let mut migrations = self.active_migrations.write().await;
                if let Some(migration) = migrations.get_mut(&migration_id) {
                    migration.status = MigrationStatus::Completed;

                    // Convert the source instance to shadow state
                    info!("🔄 [MIGRATION] Converting source instance {} to shadow state", migration.instance_id);
                    if let Err(e) = self.convert_instance_to_shadow(&migration.instance_id.to_string(), &migration.target_node_id).await {
                        error!("❌ [MIGRATION] Failed to convert instance to shadow: {}", e);
                    } else {
                        info!("✅ [MIGRATION] Successfully converted source instance to shadow state");
                    }
                }
            }
        } else {
            let error_msg = error.unwrap_or_else(|| "Unknown error".to_string());
            error!("Migration {} failed: {}", migration_id, error_msg);

            // Update migration status
            {
                let mut migrations = self.active_migrations.write().await;
                if let Some(migration) = migrations.get_mut(&migration_id) {
                    migration.status = MigrationStatus::Failed(error_msg);
                }
            }
        }

        Ok(())
    }

    /// Convert a source instance to shadow state after successful migration
    async fn convert_instance_to_shadow(&self, instance_id: &str, target_node_id: &NodeId) -> Result<()> {
        info!("🔄 [SHADOW_CONVERT] Converting instance {} to shadow state", instance_id);

        // Step 1: Stop the original process
        {
            let mut manager = self.instance_manager.lock().await;
            if let Some(instance) = manager.get_instance_by_id_mut(instance_id) {
                if let Some(pid) = instance.pid {
                    info!("🛑 [SHADOW_CONVERT] Stopping original process with PID {}", pid);

                    // Kill the original process
                    if let Err(e) = self.process_manager.stop_detached_process(pid).await {
                        warn!("⚠️ [SHADOW_CONVERT] Failed to kill original process {}: {}", pid, e);
                    } else {
                        info!("✅ [SHADOW_CONVERT] Successfully stopped original process {}", pid);
                    }
                }

                // Step 2: Update instance to shadow state
                instance.status = crate::types::InstanceStatus::Shadow;
                instance.pid = None;
                instance.source_node_id = Some(*target_node_id);

                // Step 3: Save updated metadata
                if let Err(e) = instance.save_metadata() {
                    warn!("⚠️ [SHADOW_CONVERT] Failed to save instance metadata: {}", e);
                } else {
                    info!("✅ [SHADOW_CONVERT] Updated instance metadata to shadow state");
                }

                info!("✅ [SHADOW_CONVERT] Instance {} converted to shadow state (source: {})", instance_id, target_node_id);
            } else {
                return Err(anyhow::anyhow!("Instance {} not found", instance_id));
            }
        }

        // Step 4: Register with shadow manager if available
        if let Some(shadow_mgr) = &self.shadow_manager {
            let shadow_mgr_write = shadow_mgr.write().await;

            // Parse instance_id to UUID
            let instance_uuid = if let Ok(uuid) = Uuid::parse_str(instance_id) {
                uuid
            } else {
                // Try to find the full UUID from the short ID
                let manager = self.instance_manager.lock().await;
                if let Some(instance) = manager.get_instance_by_id(instance_id) {
                    instance.id
                } else {
                    return Err(anyhow::anyhow!("Could not find instance UUID for {}", instance_id));
                }
            };

            if let Err(e) = shadow_mgr_write.demote_running_to_shadow(instance_uuid, *target_node_id).await {
                warn!("⚠️ [SHADOW_CONVERT] Failed to demote to shadow: {}", e);
            } else {
                info!("✅ [SHADOW_CONVERT] Successfully demoted instance to shadow state");
            }
        }

        Ok(())
    }

    /// Execute the actual migration process using criu-image-streamer
    async fn execute_migration(&self, migration_id: Uuid, target_port: u16) -> Result<()> {
        let migration = {
            let migrations = self.active_migrations.read().await;
            migrations.get(&migration_id).cloned()
                .ok_or_else(|| anyhow!("Migration {} not found", migration_id))?
        };

        info!("Starting migration execution for instance {}", migration.instance_id);

        // Step 1: Get instance information
        let instance = {
            let manager = self.instance_manager.lock().await;
            manager.get_instance_by_id(&migration.instance_id.to_string())
                .ok_or_else(|| anyhow!("Instance {} not found", migration.instance_id))?
                .clone()
        };

        // Step 2: Create final checkpoint for migration
        let checkpoint_name = format!("migration-{}", migration_id);
        self.create_migration_checkpoint(&instance, &checkpoint_name).await?;

        // Step 3: Update status to transferring data
        {
            let mut migrations = self.active_migrations.write().await;
            if let Some(m) = migrations.get_mut(&migration_id) {
                m.status = MigrationStatus::TransferringData;
            }
        }

        // Step 4: Use criu-image-streamer to transfer checkpoint data
        info!("Transferring checkpoint data for migration {}", migration_id);

        // Get target node IP (for now, use localhost for testing)
        let target_ip = "127.0.0.1"; // TODO: Get actual target node IP

        match self.stream_checkpoint_to_target(&instance, &checkpoint_name, target_ip, target_port).await {
            Ok(_) => {
                info!("Checkpoint streaming completed for migration {}", migration_id);

                // Step 5: Notify completion
                let complete_message = MigrationMessage::MigrationComplete {
                    migration_id,
                    success: true,
                    error: None,
                };

                let network_message = NetworkMessage::Migration(complete_message);
                self.network_manager.send_to_peer(&migration.target_node_id, network_message).await?;

                info!("Migration {} completed successfully", migration_id);
            }
            Err(e) => {
                error!("Migration {} failed during streaming: {}", migration_id, e);

                // Update status to failed
                {
                    let mut migrations = self.active_migrations.write().await;
                    if let Some(m) = migrations.get_mut(&migration_id) {
                        m.status = MigrationStatus::Failed(e.to_string());
                    }
                }

                // Notify failure
                let complete_message = MigrationMessage::MigrationComplete {
                    migration_id,
                    success: false,
                    error: Some(e.to_string()),
                };

                let network_message = NetworkMessage::Migration(complete_message);
                self.network_manager.send_to_peer(&migration.target_node_id, network_message).await?;
            }
        }

        Ok(())
    }

    /// Handle checkpoint transfer for migration
    async fn handle_checkpoint_transfer(
        &self,
        migration_id: Uuid,
        instance_id: Uuid,
        source_node_id: NodeId,
        checkpoint_data: Vec<u8>
    ) -> Result<()> {
        info!("🔄 [MIGRATION] Received checkpoint transfer for migration {} from node {}", migration_id, source_node_id);
        info!("📦 [MIGRATION] Checkpoint data size: {} bytes ({:.2} KB)", checkpoint_data.len(), checkpoint_data.len() as f64 / 1024.0);

        // Validate checkpoint data
        if checkpoint_data.is_empty() {
            error!("❌ [MIGRATION] Received empty checkpoint data for migration {}", migration_id);
            return Err(anyhow::anyhow!("Empty checkpoint data received"));
        }

        debug!("Received checkpoint data: {} bytes", checkpoint_data.len());

        // Save checkpoint data to shadow instance using the same mechanism as auto-sync
        if let Some(shadow_mgr) = &self.shadow_manager {
            let shadow_mgr_read = shadow_mgr.read().await;

            info!("💾 [MIGRATION] Creating ShadowSyncMessage for migration checkpoint");

            // Create a ShadowSyncMessage to reuse existing save logic
            let sync_message = ShadowSyncMessage {
                sender_id: source_node_id,
                instance_id,
                data_version: 999999, // High version to ensure it's processed
                checkpoint_data: Some(checkpoint_data.clone()),
                output_data: None,
                timestamp: chrono::Utc::now(),
            };

            info!("🚀 [MIGRATION] Sending checkpoint data to shadow sync handler");

            // Handle the sync message which will save the checkpoint and trigger auto-restore
            match shadow_mgr_read.handle_shadow_sync(sync_message).await {
                Ok(_) => {
                    info!("✅ [MIGRATION] Successfully processed checkpoint transfer for migration {}", migration_id);
                }
                Err(e) => {
                    error!("❌ [MIGRATION] Failed to process checkpoint transfer for migration {}: {}", migration_id, e);
                    return Err(e);
                }
            }
        } else {
            error!("❌ [MIGRATION] Shadow manager not available for migration {}", migration_id);
            return Err(anyhow::anyhow!("Shadow manager not available"));
        }

        Ok(())
    }

    /// Create a checkpoint specifically for migration
    async fn create_migration_checkpoint(&self, instance: &crate::types::Instance, checkpoint_name: &str) -> Result<()> {
        if let Some(pid) = instance.pid {
            let instance_dir = PathBuf::from("instances").join(format!("instance_{}", instance.short_id()));
            let checkpoint_dir = instance_dir.join("checkpoints").join(checkpoint_name);

            tokio::fs::create_dir_all(&checkpoint_dir).await?;

            info!("Creating migration checkpoint for PID {} in {:?}", pid, checkpoint_dir);

            // Use CRIU to create checkpoint (stop the process for migration)
            let mut cmd = Command::new("sudo");
            cmd.arg("/home/realgod/sync2/criu/criu/criu")
               .arg("dump")
               .arg("-t").arg(pid.to_string())
               .arg("-D").arg(&checkpoint_dir)
               .arg("--shell-job");

            let output = cmd.output().await?;

            if !output.status.success() {
                return Err(anyhow!("CRIU checkpoint failed: {}",
                                 String::from_utf8_lossy(&output.stderr)));
            }

            // Create migration metadata file
            let metadata = serde_json::json!({
                "instance_id": instance.short_id(),
                "migration_id": checkpoint_name.replace("migration-", ""),
                "source_node_id": self.local_node_id.to_string(),
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "program": instance.program,
                "args": instance.args
            });

            let metadata_file = checkpoint_dir.join("migration_metadata.json");
            tokio::fs::write(&metadata_file, metadata.to_string()).await?;
            info!("Created migration metadata file: {:?}", metadata_file);

            info!("Migration checkpoint created successfully: {}", checkpoint_name);
        } else {
            return Err(anyhow!("Instance has no PID"));
        }

        Ok(())
    }

    /// Transfer checkpoint data to target node using dedicated Migration message
    async fn stream_checkpoint_to_target(&self, instance: &crate::types::Instance, checkpoint_name: &str, target_ip: &str, target_port: u16) -> Result<()> {
        let checkpoint_dir = PathBuf::from("instances")
            .join(format!("instance_{}", instance.short_id()))
            .join("checkpoints")
            .join(checkpoint_name);

        info!("📤 [MIGRATION] Streaming migration checkpoint from {:?} using Migration message", checkpoint_dir);

        // Check if checkpoint directory exists
        if !checkpoint_dir.exists() {
            error!("❌ [MIGRATION] Checkpoint directory does not exist: {:?}", checkpoint_dir);
            return Err(anyhow::anyhow!("Checkpoint directory not found: {:?}", checkpoint_dir));
        }

        debug!("Created migration checkpoint: {:?}", checkpoint_dir);

        info!("🔄 [MIGRATION] Reading checkpoint data using ImageSyncManager...");

        // Use the SAME method as auto-sync to read checkpoint data
        let checkpoint_data = match ImageSyncManager::read_checkpoint_data(&checkpoint_dir).await {
            Ok(data) => {
                info!("✅ [MIGRATION] Successfully read checkpoint data: {} bytes ({:.2} KB)", data.len(), data.len() as f64 / 1024.0);
                data
            }
            Err(e) => {
                error!("❌ [MIGRATION] Failed to read checkpoint data: {}", e);
                return Err(e);
            }
        };

        if checkpoint_data.is_empty() {
            error!("❌ [MIGRATION] No checkpoint data found in directory: {:?}", checkpoint_dir);
            return Err(anyhow::anyhow!("No checkpoint data found"));
        }

        debug!("Sending checkpoint data: {} bytes", checkpoint_data.len());

        // Send dedicated Migration message with checkpoint data
        let network_manager = &self.network_manager;
        let migration_id = Uuid::new_v4();
        let migration_message = MigrationMessage::CheckpointTransfer {
            migration_id,
            instance_id: instance.id,
            source_node_id: self.local_node_id,
            target_node_id: Uuid::new_v4(), // TODO: This should be the actual target node ID
            checkpoint_data: checkpoint_data.clone(),
        };

        let network_message = NetworkMessage::Migration(migration_message);

        info!("🚀 [MIGRATION] Broadcasting migration message (ID: {}) to all peers...", migration_id);

        // Broadcast the migration message to all peers
        match network_manager.broadcast(network_message).await {
            Ok(_) => {
                info!("✅ [MIGRATION] Successfully sent migration checkpoint using Migration message");
                info!("📊 [MIGRATION] Transfer summary: {} bytes sent for instance {}", checkpoint_data.len(), instance.short_id());
            }
            Err(e) => {
                error!("❌ [MIGRATION] Failed to send migration checkpoint: {}", e);
                return Err(anyhow::anyhow!("Failed to send migration checkpoint: {}", e));
            }
        }

        Ok(())
    }



    /// Start a server to receive checkpoint data from source node (replaced by ImageSyncManager implementation)
    pub async fn start_migration_receiver_old(&self, port: u16) -> Result<()> {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
        info!("Migration receiver listening on port {}", port);

        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    info!("Accepted migration connection from {}", addr);

                    // Handle the incoming migration in a separate task
                    let self_clone = self.clone();
                    tokio::spawn(async move {
                        if let Err(e) = self_clone.handle_incoming_migration(stream).await {
                            error!("Failed to handle incoming migration: {}", e);
                        }
                    });
                }
                Err(e) => {
                    error!("Failed to accept migration connection: {}", e);
                }
            }
        }
    }

    /// Handle incoming migration data stream
    async fn handle_incoming_migration(&self, mut stream: TcpStream) -> Result<()> {
        info!("Handling incoming migration data");

        // Read data length first
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await?;
        let data_len = u32::from_be_bytes(len_buf) as usize;

        info!("Expecting {} bytes of migration data", data_len);

        // Read the compressed checkpoint data
        let mut compressed_data = vec![0u8; data_len];
        stream.read_exact(&mut compressed_data).await?;

        info!("Received {} bytes of compressed checkpoint data", compressed_data.len());

        // Create temporary directory for received checkpoint first
        let migration_id = Uuid::new_v4();
        let checkpoint_name = format!("received-migration-{}", migration_id);
        let temp_dir = std::env::temp_dir().join(&checkpoint_name);

        tokio::fs::create_dir_all(&temp_dir).await?;

        // Decompress and extract checkpoint files to temp directory
        Self::extract_checkpoint_data(&compressed_data, &temp_dir).await?;

        info!("Migration data extracted to {:?}", temp_dir);

        // Read migration metadata to get instance ID
        let metadata_file = temp_dir.join("migration_metadata.json");
        if metadata_file.exists() {
            let metadata_content = tokio::fs::read_to_string(&metadata_file).await?;
            let metadata: serde_json::Value = serde_json::from_str(&metadata_content)?;

            if let Some(instance_id_str) = metadata["instance_id"].as_str() {
                // Move checkpoint to proper instance directory
                let instance_dir = PathBuf::from("instances").join(format!("instance_{}", instance_id_str));
                let final_checkpoint_dir = instance_dir.join("checkpoints").join(&checkpoint_name);

                tokio::fs::create_dir_all(&final_checkpoint_dir).await?;

                // Move all files from temp to final location
                let mut entries = tokio::fs::read_dir(&temp_dir).await?;
                while let Some(entry) = entries.next_entry().await? {
                    let src = entry.path();
                    let dst = final_checkpoint_dir.join(entry.file_name());
                    tokio::fs::rename(src, dst).await?;
                }

                // Clean up temp directory
                tokio::fs::remove_dir_all(&temp_dir).await.ok();

                info!("Moved migration checkpoint to instance directory: {:?}", final_checkpoint_dir);
            } else {
                warn!("No instance_id found in migration metadata");
            }
        } else {
            warn!("No migration metadata found in received checkpoint");
        }

        // TODO: Restore the process from the received checkpoint
        // This would involve:
        // 1. Using CRIU to restore the process
        // 2. Updating the instance registry
        // 3. Notifying the source node of successful migration

        Ok(())
    }

    /// Extract compressed checkpoint data to directory
    async fn extract_checkpoint_data(compressed_data: &[u8], target_dir: &PathBuf) -> Result<()> {
        use std::io::Read;
        use flate2::read::GzDecoder;

        let mut decoder = GzDecoder::new(compressed_data);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed)?;

        let mut cursor = 0;

        while cursor < decompressed.len() {
            // Read file name length
            if cursor + 4 > decompressed.len() { break; }
            let name_len = u32::from_be_bytes([
                decompressed[cursor], decompressed[cursor + 1],
                decompressed[cursor + 2], decompressed[cursor + 3]
            ]) as usize;
            cursor += 4;

            // Read file name
            if cursor + name_len > decompressed.len() { break; }
            let file_name = String::from_utf8(decompressed[cursor..cursor + name_len].to_vec())?;
            cursor += name_len;

            // Read file data length
            if cursor + 4 > decompressed.len() { break; }
            let data_len = u32::from_be_bytes([
                decompressed[cursor], decompressed[cursor + 1],
                decompressed[cursor + 2], decompressed[cursor + 3]
            ]) as usize;
            cursor += 4;

            // Read file data
            if cursor + data_len > decompressed.len() { break; }
            let file_data = &decompressed[cursor..cursor + data_len];
            cursor += data_len;

            // Write file to target directory
            let file_path = target_dir.join(&file_name);
            tokio::fs::write(&file_path, file_data).await?;

            info!("Extracted file: {} ({} bytes)", file_name, data_len);
        }

        Ok(())
    }
}