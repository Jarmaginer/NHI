use crate::message_protocol::*;
use crate::types::{Instance, InstanceStatus};
use crate::instance::InstanceManager;
use crate::process_manager::ProcessManager;
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::os::unix::process::ExitStatusExt;
use tokio::sync::{RwLock, mpsc};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Manages shadow instances across the cluster
pub struct ShadowInstanceManager {
    local_node_id: NodeId,
    instance_manager: Arc<tokio::sync::Mutex<InstanceManager>>,
    process_manager: Arc<ProcessManager>,
    shadow_registry: Arc<RwLock<HashMap<Uuid, ShadowInstanceInfo>>>,
    network_sender: Option<mpsc::UnboundedSender<NetworkMessage>>,
}

/// Information about a shadow instance
#[derive(Debug, Clone)]
pub struct ShadowInstanceInfo {
    pub instance_id: Uuid,
    pub source_node_id: NodeId,
    pub created_at: DateTime<Utc>,
    pub last_sync_time: DateTime<Utc>,
    pub output_buffer: Vec<u8>,
    pub latest_checkpoint: Option<Vec<u8>>,
    pub data_version: u64,
}

impl ShadowInstanceManager {
    pub fn new(local_node_id: NodeId, instance_manager: Arc<tokio::sync::Mutex<InstanceManager>>, process_manager: Arc<ProcessManager>) -> Self {
        Self {
            local_node_id,
            instance_manager,
            process_manager,
            shadow_registry: Arc::new(RwLock::new(HashMap::new())),
            network_sender: None,
        }
    }

    pub fn set_network_sender(&mut self, sender: mpsc::UnboundedSender<NetworkMessage>) {
        self.network_sender = Some(sender);
    }

    /// Broadcast instance creation to all other nodes (they will create shadow instances)
    pub async fn broadcast_instance_creation(&self, instance: &Instance) -> Result<()> {
        if instance.status != InstanceStatus::Running {
            return Ok(()); // Only create shadows for running instances
        }

        // Broadcast instance creation to all nodes
        if let Some(network_sender) = &self.network_sender {
            let instance_info = InstanceInfo {
                id: instance.id,
                program: instance.program.clone(),
                args: instance.args.clone(),
                status: instance.status.clone(),
                node_id: self.local_node_id,
                created_at: instance.created_at,
                source_node_id: None,
            };

            let sync_message = InstanceSyncMessage {
                sender_id: self.local_node_id,
                instances: vec![instance_info],
                timestamp: Utc::now(),
            };

            let network_message = NetworkMessage::InstanceSync(sync_message);

            if let Err(e) = network_sender.send(network_message) {
                error!("Failed to broadcast instance creation: {}", e);
            } else {
                info!("Broadcasted instance creation for {}", instance.short_id());
            }
        }

        Ok(())
    }

    /// Handle incoming instance sync message (create shadow instances)
    pub async fn handle_instance_sync(&self, sync_message: InstanceSyncMessage) -> Result<()> {
        if sync_message.sender_id == self.local_node_id {
            return Ok(()); // Ignore our own messages
        }

        for instance_info in sync_message.instances {
            if instance_info.status == InstanceStatus::Running {
                self.create_local_shadow_instance(&instance_info, sync_message.sender_id).await?;
            }
        }

        Ok(())
    }

    /// Create a local shadow instance from remote instance info
    async fn create_local_shadow_instance(&self, instance_info: &InstanceInfo, source_node_id: NodeId) -> Result<()> {
        // Check if we already have this instance (running or shadow)
        {
            let instance_manager = self.instance_manager.lock().await;
            info!("🔍 [MIGRATION_SYNC] Checking for existing instance: {}", instance_info.id);

            // Debug: List all instances in memory
            let all_instances = instance_manager.get_all_instances();
            info!("🔍 [MIGRATION_SYNC] Total instances in memory: {}", all_instances.len());
            for inst in &all_instances {
                info!("🔍 [MIGRATION_SYNC] Memory instance: {} ({}), status: {:?}",
                      inst.short_id(), inst.id, inst.status);
            }

            // Try multiple ways to find the instance
            let instance_id_str = instance_info.id.to_string();
            let short_id = &instance_id_str[0..8]; // First 8 chars

            info!("🔍 [MIGRATION_SYNC] Searching for instance with full ID: {}", instance_id_str);
            info!("🔍 [MIGRATION_SYNC] Searching for instance with short ID: {}", short_id);

            // Try full UUID
            let existing_instance = instance_manager.get_instance_by_id(&instance_id_str)
                .or_else(|| {
                    info!("🔍 [MIGRATION_SYNC] Full UUID search failed, trying short ID");
                    instance_manager.get_instance_by_id(short_id)
                });

            if let Some(existing_instance) = existing_instance {
                info!("🔍 [MIGRATION_SYNC] Found existing instance {} with status {:?}, remote status: {:?}",
                      instance_info.id, existing_instance.status, instance_info.status);

                // If we have a running instance and receive notification that it's running elsewhere,
                // this means our instance was migrated and we should convert to shadow
                if existing_instance.status == InstanceStatus::Running && instance_info.status == InstanceStatus::Running {
                    info!("🔄 [MIGRATION_SYNC] Instance {} is now running on node {}, converting local to shadow",
                          instance_info.id, source_node_id);

                    // Drop the lock before calling demote function
                    drop(instance_manager);

                    // Convert our running instance to shadow
                    if let Err(e) = self.demote_running_to_shadow(instance_info.id, source_node_id).await {
                        error!("❌ [MIGRATION_SYNC] Failed to demote running instance to shadow: {}", e);
                        return Err(e);
                    } else {
                        info!("✅ [MIGRATION_SYNC] Successfully converted running instance {} to shadow", instance_info.id);
                        return Ok(());
                    }
                } else {
                    info!("ℹ️ [MIGRATION_SYNC] Instance {} already exists locally with status {:?}, remote status {:?}, skipping",
                           instance_info.id, existing_instance.status, instance_info.status);
                    return Ok(());
                }
            } else {
                info!("ℹ️ [MIGRATION_SYNC] No existing instance found for {} (tried both full and short ID), will create new shadow", instance_info.id);
            }
        }

        // Create shadow instance in local instance manager
        {
            let mut instance_manager = self.instance_manager.lock().await;

            let mut shadow_instance = Instance::new(
                instance_info.program.clone(),
                instance_info.args.clone(),
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/")),
            );

            // Override the UUID to match the source instance
            shadow_instance.id = instance_info.id;
            shadow_instance.status = InstanceStatus::Shadow;
            shadow_instance.source_node_id = Some(source_node_id);
            shadow_instance.created_at = instance_info.created_at;
            shadow_instance.pid = None; // Shadow instances don't have actual processes

            // Save shadow instance metadata
            if let Err(e) = shadow_instance.save_metadata() {
                warn!("Failed to save shadow instance metadata: {}", e);
            }

            // Add to instance manager
            instance_manager.add_instance(shadow_instance);
        }

        // Add to shadow registry
        {
            let mut registry = self.shadow_registry.write().await;
            let shadow_info = ShadowInstanceInfo {
                instance_id: instance_info.id,
                source_node_id,
                created_at: Utc::now(),
                last_sync_time: Utc::now(),
                output_buffer: Vec::new(),
                latest_checkpoint: None,
                data_version: 0,
            };
            registry.insert(instance_info.id, shadow_info);
        }

        info!("Created shadow instance {} from source node {}", instance_info.id, source_node_id);
        Ok(())
    }

    /// Handle incoming shadow data synchronization
    pub async fn handle_shadow_sync(&self, sync_message: ShadowSyncMessage) -> Result<()> {
        if sync_message.sender_id == self.local_node_id {
            return Ok(()); // Ignore our own messages
        }

        let instance_id = sync_message.instance_id;
        let sender_id = sync_message.sender_id;

        info!("Received shadow sync for instance {} from node {}", instance_id, sender_id);

        // Check if we have a running instance that should be demoted to shadow
        {
            let instance_manager = self.instance_manager.lock().await;

            // Try to find the instance by both full and short ID
            let instance_id_str = instance_id.to_string();
            let short_id = &instance_id_str[0..8];

            let existing_instance = instance_manager.get_instance_by_id(&instance_id_str)
                .or_else(|| instance_manager.get_instance_by_id(short_id));

            if let Some(existing_instance) = existing_instance {
                // If we have a running instance and receive shadow sync from another node,
                // this means our instance was migrated and we should convert to shadow
                if existing_instance.status == InstanceStatus::Running {
                    info!("Instance {} migrated to node {}, converting local to shadow",
                          instance_id, sender_id);

                    // Drop the lock before calling demote function
                    drop(instance_manager);

                    // Convert our running instance to shadow
                    if let Err(e) = self.demote_running_to_shadow(instance_id, sender_id).await {
                        error!("Failed to demote running instance to shadow: {}", e);
                        return Err(e);
                    }
                }
            }
        }

        let mut registry = self.shadow_registry.write().await;

        if let Some(shadow_info) = registry.get_mut(&instance_id) {
            // Only update if the version is newer
            if sync_message.data_version > shadow_info.data_version {
                shadow_info.data_version = sync_message.data_version;
                shadow_info.last_sync_time = Utc::now();

                if let Some(checkpoint_data) = sync_message.checkpoint_data {
                    // Save checkpoint data to disk
                    if let Err(e) = self.save_checkpoint_data(instance_id, &checkpoint_data).await {
                        warn!("Failed to save checkpoint data for instance {}: {}", instance_id, e);
                    } else {
                        shadow_info.latest_checkpoint = Some(checkpoint_data.clone());
                        info!("Updated checkpoint data for shadow instance {}", instance_id);

                        // Check if this is a migration checkpoint and auto-restore
                        if let Err(e) = self.check_and_handle_migration_checkpoint(instance_id, &checkpoint_data).await {
                            warn!("Failed to handle potential migration checkpoint for instance {}: {}", instance_id, e);
                        }
                    }
                }

                if let Some(output_data) = sync_message.output_data {
                    shadow_info.output_buffer.extend_from_slice(&output_data);
                    debug!("Updated output buffer for shadow instance {}", instance_id);
                }
            }
        } else {
            // Create new shadow instance if we don't have one
            info!("Creating new shadow instance {} from node {}", instance_id, sender_id);

            let mut latest_checkpoint = None;

            // If we have checkpoint data, save it to disk
            if let Some(checkpoint_data) = sync_message.checkpoint_data {
                if let Err(e) = self.save_checkpoint_data(instance_id, &checkpoint_data).await {
                    warn!("Failed to save checkpoint data for new shadow instance {}: {}", instance_id, e);
                } else {
                    latest_checkpoint = Some(checkpoint_data);
                    info!("Saved checkpoint data for new shadow instance {}", instance_id);
                }
            }

            let shadow_info = ShadowInstanceInfo {
                instance_id,
                source_node_id: sender_id,
                created_at: sync_message.timestamp,
                last_sync_time: sync_message.timestamp,
                output_buffer: sync_message.output_data.unwrap_or_default(),
                latest_checkpoint,
                data_version: sync_message.data_version,
            };

            registry.insert(instance_id, shadow_info);
            info!("Created new shadow instance {} from node {}", instance_id, sender_id);
        }

        Ok(())
    }

    /// Handle instance stop notification
    pub async fn handle_instance_stop(&self, stop_message: InstanceStopMessage) -> Result<()> {
        if stop_message.sender_id == self.local_node_id {
            return Ok(()); // Ignore our own messages
        }

        info!("Received instance stop notification for instance {} from node {}",
              stop_message.instance_id, stop_message.sender_id);

        // Remove the shadow instance if it exists
        if let Err(e) = self.remove_shadow_instance(stop_message.instance_id).await {
            warn!("Failed to remove shadow instance {}: {}", stop_message.instance_id, e);
        } else {
            info!("Successfully removed shadow instance {} after source stop", stop_message.instance_id);
        }

        Ok(())
    }

    /// Stream output data from a running instance to all shadow instances
    pub async fn stream_output_to_shadows(&self, instance_id: Uuid, output_data: Vec<u8>, _stream_type: StreamType) -> Result<()> {
        if let Some(network_sender) = &self.network_sender {
            let data_version = self.get_next_data_version(instance_id).await;
            debug!("Streaming output to shadows: {} bytes, version {} for instance {}",
                   output_data.len(), data_version, instance_id);

            let sync_message = ShadowSyncMessage {
                sender_id: self.local_node_id,
                instance_id,
                data_version,
                checkpoint_data: None,
                output_data: Some(output_data.clone()),
                timestamp: Utc::now(),
            };

            let network_message = NetworkMessage::ShadowSync(sync_message);

            if let Err(e) = network_sender.send(network_message) {
                error!("Failed to stream output to shadows: {}", e);
            } else {
                debug!("Successfully sent shadow sync message for instance {}", instance_id);
            }
        } else {
            debug!("No network sender available for streaming output to shadows");
        }

        Ok(())
    }

    /// Stream checkpoint data from a running instance to all shadow instances
    pub async fn stream_checkpoint_to_shadows(&self, instance_id: Uuid, checkpoint_data: Vec<u8>) -> Result<()> {
        if let Some(network_sender) = &self.network_sender {
            let sync_message = ShadowSyncMessage {
                sender_id: self.local_node_id,
                instance_id,
                data_version: self.get_next_data_version(instance_id).await,
                checkpoint_data: Some(checkpoint_data),
                output_data: None,
                timestamp: Utc::now(),
            };

            let network_message = NetworkMessage::ShadowSync(sync_message);

            if let Err(e) = network_sender.send(network_message) {
                error!("Failed to stream checkpoint to shadows: {}", e);
            } else {
                info!("Streamed checkpoint data for instance {} to shadow instances", instance_id);
            }
        }

        Ok(())
    }

    /// Get all shadow instances managed by this node
    pub async fn get_shadow_instances(&self) -> Vec<ShadowInstanceInfo> {
        let registry = self.shadow_registry.read().await;
        registry.values().cloned().collect()
    }

    /// Get shadow instance info by ID
    pub async fn get_shadow_instance(&self, instance_id: Uuid) -> Option<ShadowInstanceInfo> {
        let registry = self.shadow_registry.read().await;
        registry.get(&instance_id).cloned()
    }

    /// Forward input from shadow instance to source instance
    pub async fn forward_input_to_source(&self, shadow_instance_id: Uuid, input: String) -> Result<()> {
        let registry = self.shadow_registry.read().await;
        if let Some(shadow_info) = registry.get(&shadow_instance_id) {
            if let Some(network_sender) = &self.network_sender {
                let input_message = ShadowInputMessage {
                    sender_id: self.local_node_id,
                    target_node_id: shadow_info.source_node_id,
                    instance_id: shadow_instance_id,
                    input_data: input,
                    timestamp: Utc::now(),
                };

                let network_message = NetworkMessage::ShadowInput(input_message);

                if let Err(e) = network_sender.send(network_message) {
                    error!("Failed to forward input to source: {}", e);
                } else {
                    debug!("Forwarded input to source node {} for instance {}",
                           shadow_info.source_node_id, shadow_instance_id);
                }
            }
        } else {
            return Err(anyhow::anyhow!("Shadow instance not found: {}", shadow_instance_id));
        }

        Ok(())
    }

    /// Broadcast instance stop to all shadow instances
    pub async fn broadcast_instance_stop(&self, instance_id: Uuid) -> Result<()> {
        if let Some(network_sender) = &self.network_sender {
            let stop_message = InstanceStopMessage {
                sender_id: self.local_node_id,
                instance_id,
                timestamp: Utc::now(),
            };

            let network_message = NetworkMessage::InstanceStop(stop_message);

            if let Err(e) = network_sender.send(network_message) {
                error!("Failed to broadcast instance stop: {}", e);
            } else {
                info!("Broadcasted instance stop for {}", instance_id);
            }
        }

        Ok(())
    }

    /// Remove shadow instance when the source instance is stopped
    pub async fn remove_shadow_instance(&self, instance_id: Uuid) -> Result<()> {
        // Remove from shadow registry
        {
            let mut registry = self.shadow_registry.write().await;
            registry.remove(&instance_id);
        }

        // Remove from instance manager
        {
            let mut instance_manager = self.instance_manager.lock().await;
            instance_manager.remove_instance(&instance_id.to_string());
        }

        info!("Removed shadow instance {}", instance_id);
        Ok(())
    }

    /// Promote shadow instance to running instance (for migration)
    pub async fn promote_shadow_to_running(&self, instance_id: Uuid, new_pid: u32) -> Result<()> {
        // Get instance info before updating
        let (program, args, working_dir) = {
            let instance_manager = self.instance_manager.lock().await;
            if let Some(instance) = instance_manager.get_instance_by_id(&instance_id.to_string()) {
                (instance.program.clone(), instance.args.clone(), instance.working_dir.clone())
            } else {
                return Err(anyhow::anyhow!("Instance {} not found", instance_id));
            }
        };

        // Register the migrated process with process_manager
        info!("🔄 [PROMOTE] Registering migrated process {} with process_manager", new_pid);
        if let Err(e) = self.process_manager.register_migrated_process(instance_id, new_pid, &program, &args, &working_dir).await {
            warn!("⚠️ [PROMOTE] Failed to register migrated process with process_manager: {}", e);
        } else {
            info!("✅ [PROMOTE] Successfully registered migrated process with process_manager");
        }

        // Update in instance manager
        {
            let mut instance_manager = self.instance_manager.lock().await;
            if let Some(instance) = instance_manager.get_instance_by_id_mut(&instance_id.to_string()) {
                instance.status = InstanceStatus::Running;
                instance.pid = Some(new_pid);
                instance.source_node_id = None; // No longer a shadow

                // Save updated metadata
                if let Err(e) = instance.save_metadata() {
                    warn!("Failed to save instance metadata after promotion: {}", e);
                }
            }
        }

        // Remove from shadow registry (it's now a running instance)
        {
            let mut registry = self.shadow_registry.write().await;
            registry.remove(&instance_id);
        }

        info!("Promoted shadow instance {} to running with PID {}", instance_id, new_pid);
        Ok(())
    }

    /// Demote running instance to shadow instance (for migration)
    pub async fn demote_running_to_shadow(&self, instance_id: Uuid, new_source_node_id: NodeId) -> Result<()> {
        // Update in instance manager
        {
            let mut instance_manager = self.instance_manager.lock().await;
            if let Some(instance) = instance_manager.get_instance_by_id_mut(&instance_id.to_string()) {
                instance.status = InstanceStatus::Shadow;
                instance.pid = None; // Shadow instances don't have processes
                instance.source_node_id = Some(new_source_node_id);

                // Save updated metadata
                if let Err(e) = instance.save_metadata() {
                    warn!("Failed to save instance metadata after demotion: {}", e);
                }
            }
        }

        // Add to shadow registry
        {
            let mut registry = self.shadow_registry.write().await;
            let shadow_info = ShadowInstanceInfo {
                instance_id,
                source_node_id: new_source_node_id,
                created_at: Utc::now(),
                last_sync_time: Utc::now(),
                output_buffer: Vec::new(),
                latest_checkpoint: None,
                data_version: 0,
            };
            registry.insert(instance_id, shadow_info);
        }

        info!("Demoted running instance {} to shadow for source node {}", instance_id, new_source_node_id);
        Ok(())
    }

    // Private helper methods

    /// Save checkpoint data to disk for a shadow instance
    async fn save_checkpoint_data(&self, instance_id: Uuid, checkpoint_data: &[u8]) -> Result<()> {
        use std::io::Read;
        use flate2::read::GzDecoder;

        debug!("Saving checkpoint data for instance {}: {} bytes", instance_id, checkpoint_data.len());

        // Create instance directory (same structure as running instances)
        let instance_short_id = instance_id.to_string()[..8].to_string();
        let instance_dir = PathBuf::from("instances").join(format!("instance_{}", instance_short_id));
        let checkpoint_dir = instance_dir.join("checkpoints").join(format!("sync-{}", chrono::Utc::now().timestamp()));

        tokio::fs::create_dir_all(&checkpoint_dir).await?;

        // Decompress and extract checkpoint files
        let mut decoder = GzDecoder::new(checkpoint_data);
        let mut decompressed_data = Vec::new();
        decoder.read_to_end(&mut decompressed_data)?;

        // Parse the decompressed data and extract files
        let mut cursor = std::io::Cursor::new(decompressed_data);
        let mut file_count = 0;

        while cursor.position() < cursor.get_ref().len() as u64 {
            // Read file name length
            let mut name_len_buf = [0u8; 4];
            if cursor.read_exact(&mut name_len_buf).is_err() {
                break; // End of data
            }
            let name_len = u32::from_le_bytes(name_len_buf) as usize;

            // Read file name
            let mut name_buf = vec![0u8; name_len];
            cursor.read_exact(&mut name_buf)?;
            let file_name = String::from_utf8(name_buf)?;

            // Read file data length
            let mut data_len_buf = [0u8; 4];
            cursor.read_exact(&mut data_len_buf)?;
            let data_len = u32::from_le_bytes(data_len_buf) as usize;

            // Read file data
            let mut file_data = vec![0u8; data_len];
            cursor.read_exact(&mut file_data)?;

            // Write file to checkpoint directory
            let file_path = checkpoint_dir.join(&file_name);
            tokio::fs::write(&file_path, &file_data).await?;
            file_count += 1;
        }

        debug!("Extracted {} checkpoint files for instance {}", file_count, instance_id);

        info!("Saved checkpoint data for shadow instance {} to {:?}", instance_short_id, checkpoint_dir);
        Ok(())
    }

    /// Check if received checkpoint is a migration checkpoint and handle auto-restore
    async fn check_and_handle_migration_checkpoint(&self, instance_id: Uuid, checkpoint_data: &[u8]) -> Result<()> {
        // Extract checkpoint to instance directory to check for migration metadata
        let instance_short_id = instance_id.to_string()[..8].to_string();
        let instance_dir = PathBuf::from("instances").join(format!("instance_{}", instance_short_id));
        let migration_check_dir = instance_dir.join("checkpoints").join(format!("migration-check-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&migration_check_dir).await?;

        // Extract checkpoint data
        if let Err(e) = self.extract_checkpoint_to_dir(checkpoint_data, &migration_check_dir).await {
            tokio::fs::remove_dir_all(&migration_check_dir).await.ok();
            return Err(e);
        }

        // Check for migration metadata file
        let metadata_file = migration_check_dir.join("migration_metadata.json");
        if metadata_file.exists() {
            info!("🎯 [MIGRATION] Detected migration checkpoint for instance {}, starting auto-restore", instance_id);

            // Read and log migration metadata for debugging
            match tokio::fs::read_to_string(&metadata_file).await {
                Ok(metadata_content) => {
                    info!("📋 [MIGRATION] Migration metadata: {}", metadata_content);
                }
                Err(e) => {
                    warn!("⚠️ [MIGRATION] Failed to read migration metadata: {}", e);
                }
            }

            // This is a migration checkpoint - restore it automatically
            info!("🚀 [MIGRATION] Starting automatic restore process...");

            // Move the migration checkpoint to a permanent location before restore
            let final_checkpoint_dir = instance_dir.join("checkpoints").join(format!("migration-{}", uuid::Uuid::new_v4()));
            tokio::fs::create_dir_all(&final_checkpoint_dir).await?;

            // Move all files from temp to final location
            let mut entries = tokio::fs::read_dir(&migration_check_dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let src = entry.path();
                let dst = final_checkpoint_dir.join(entry.file_name());
                tokio::fs::rename(src, dst).await?;
            }

            // Clean up temp directory
            tokio::fs::remove_dir_all(&migration_check_dir).await.ok();

            match self.restore_migration_checkpoint(instance_id, &final_checkpoint_dir, &instance_dir).await {
                Ok(_) => {
                    info!("✅ [MIGRATION] Successfully restored migration checkpoint for instance {}", instance_id);
                }
                Err(e) => {
                    error!("❌ [MIGRATION] Failed to restore migration checkpoint for instance {}: {}", instance_id, e);
                    return Err(e);
                }
            }
        } else {
            info!("ℹ️ [MIGRATION] No migration metadata found, treating as regular shadow sync");
        }

        // Clean up migration check directory
        tokio::fs::remove_dir_all(&migration_check_dir).await.ok();
        Ok(())
    }

    /// Extract checkpoint data to a directory
    async fn extract_checkpoint_to_dir(&self, checkpoint_data: &[u8], target_dir: &PathBuf) -> Result<()> {
        use std::io::Read;
        use flate2::read::GzDecoder;

        // Decompress checkpoint data
        let mut decoder = GzDecoder::new(checkpoint_data);
        let mut decompressed_data = Vec::new();
        decoder.read_to_end(&mut decompressed_data)?;

        // Parse and extract files
        let mut cursor = std::io::Cursor::new(decompressed_data);

        while cursor.position() < cursor.get_ref().len() as u64 {
            // Read file name length
            let mut name_len_buf = [0u8; 4];
            if cursor.read_exact(&mut name_len_buf).is_err() {
                break;
            }
            let name_len = u32::from_le_bytes(name_len_buf) as usize;

            // Read file name
            let mut name_buf = vec![0u8; name_len];
            cursor.read_exact(&mut name_buf)?;
            let file_name = String::from_utf8(name_buf)?;

            // Read file data length
            let mut data_len_buf = [0u8; 4];
            cursor.read_exact(&mut data_len_buf)?;
            let data_len = u32::from_le_bytes(data_len_buf) as usize;

            // Read file data
            let mut file_data = vec![0u8; data_len];
            cursor.read_exact(&mut file_data)?;

            // Write file
            let file_path = target_dir.join(&file_name);
            tokio::fs::write(&file_path, &file_data).await?;
        }

        Ok(())
    }

    /// Restore migration checkpoint and promote shadow to running
    async fn restore_migration_checkpoint(&self, instance_id: Uuid, checkpoint_dir: &PathBuf, instance_dir: &PathBuf) -> Result<()> {
        use tokio::process::Command;

        info!("🔄 [RESTORE] Starting migration checkpoint restore from {:?}", checkpoint_dir);
        info!("🔄 [RESTORE] Instance working directory: {:?}", instance_dir);

        // List files in checkpoint directory for debugging
        match tokio::fs::read_dir(checkpoint_dir).await {
            Ok(mut entries) => {
                info!("📁 [RESTORE] Files in checkpoint directory:");
                while let Ok(Some(entry)) = entries.next_entry().await {
                    if let Ok(metadata) = entry.metadata().await {
                        info!("  📄 {} ({} bytes)", entry.file_name().to_string_lossy(), metadata.len());
                    }
                }
            }
            Err(e) => {
                error!("❌ [RESTORE] Failed to read checkpoint directory: {}", e);
            }
        }

        info!("🚀 [RESTORE] Executing CRIU restore command...");

        // Ensure instance directory exists
        tokio::fs::create_dir_all(instance_dir).await?;

        // Use CRIU to restore the process with verbose output and correct working directory
        let mut cmd = Command::new("sudo");
        cmd.arg("/home/realgod/sync2/criu/criu/criu")
           .arg("restore")
           .arg("-D").arg(checkpoint_dir.canonicalize()?)  // Use absolute path
           .arg("--shell-job")
           .arg("-v4")  // Very verbose output
           .arg("--log-file").arg("/tmp/criu-restore.log")  // Log to file
           .arg("--log-pid")  // Include PID in logs
           .current_dir(instance_dir.canonicalize()?);  // Set working directory to absolute instance directory

        info!("🔧 [RESTORE] CRIU command: {:?}", cmd);

        // Use timeout for CRIU restore command since it might hang after successful restore
        let output = match tokio::time::timeout(
            tokio::time::Duration::from_secs(10),
            cmd.output()
        ).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                error!("❌ [RESTORE] Failed to execute CRIU command: {}", e);
                return Err(anyhow::anyhow!("Failed to execute CRIU command: {}", e));
            }
            Err(_) => {
                warn!("⚠️ [RESTORE] CRIU restore command timed out after 10 seconds, checking if restore was successful...");

                // Check if restore was successful by reading the log file
                match tokio::fs::read_to_string("/tmp/criu-restore.log").await {
                    Ok(log_content) => {
                        if log_content.contains("Restore finished successfully") {
                            info!("✅ [RESTORE] CRIU restore appears to have succeeded based on log file");
                            // Create a fake successful output
                            std::process::Output {
                                status: std::process::ExitStatus::from_raw(0),
                                stdout: Vec::new(),
                                stderr: Vec::new(),
                            }
                        } else {
                            error!("❌ [RESTORE] CRIU restore timed out and log doesn't show success");
                            return Err(anyhow::anyhow!("CRIU restore timed out and appears to have failed"));
                        }
                    }
                    Err(e) => {
                        error!("❌ [RESTORE] CRIU restore timed out and couldn't read log file: {}", e);
                        return Err(anyhow::anyhow!("CRIU restore timed out: {}", e));
                    }
                }
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        info!("📤 [RESTORE] CRIU stdout ({} bytes): {}", output.stdout.len(), stdout);
        info!("📤 [RESTORE] CRIU stderr ({} bytes): {}", output.stderr.len(), stderr);

        // Log exit status
        info!("📤 [RESTORE] CRIU exit status: {:?}", output.status);

        if !output.status.success() {
            error!("❌ [RESTORE] CRIU restore failed with exit code: {:?}", output.status.code());
            error!("❌ [RESTORE] CRIU stdout: {}", stdout);
            error!("❌ [RESTORE] CRIU stderr: {}", stderr);
            return Err(anyhow::anyhow!("CRIU restore failed with exit code {:?}: {}", output.status.code(), stderr));
        }

        info!("✅ [RESTORE] CRIU restore command completed successfully");

        // Read and display CRIU log file
        info!("📋 [RESTORE] Reading CRIU log file...");
        match tokio::fs::read_to_string("/tmp/criu-restore.log").await {
            Ok(log_content) => {
                info!("📋 [RESTORE] CRIU restore log content:");
                info!("================== CRIU RESTORE LOG START ==================");
                for line in log_content.lines() {
                    info!("CRIU: {}", line);
                }
                info!("================== CRIU RESTORE LOG END ==================");

                // Check for specific error patterns
                if log_content.contains("Error") || log_content.contains("Failed") || log_content.contains("failed") {
                    warn!("⚠️ [RESTORE] CRIU log contains error messages");
                }
                if log_content.contains("Restore finished successfully") {
                    info!("✅ [RESTORE] CRIU log confirms successful restore");
                }
            }
            Err(e) => {
                warn!("⚠️ [RESTORE] Failed to read CRIU log file: {}", e);
            }
        }

        // Wait a bit for the process to fully start
        info!("⏳ [RESTORE] Waiting for restored process to start...");
        tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;

        info!("🔍 [RESTORE] Looking for restored process PID...");
        let new_pid = match self.find_restored_pid().await {
            Ok(pid) => {
                info!("✅ [RESTORE] Found restored process with PID: {}", pid);
                pid
            }
            Err(e) => {
                error!("❌ [RESTORE] Failed to find restored PID: {}", e);
                return Err(e);
            }
        };

        info!("🔄 [RESTORE] Promoting shadow instance to running state...");

        // Promote shadow to running instance
        match self.promote_shadow_to_running(instance_id, new_pid).await {
            Ok(_) => {
                info!("✅ [RESTORE] Successfully promoted shadow to running instance");
            }
            Err(e) => {
                error!("❌ [RESTORE] Failed to promote shadow to running: {}", e);
                return Err(e);
            }
        }

        // Verify process is still running after promotion
        info!("🔍 [RESTORE] Verifying migrated process health...");
        match self.verify_process_health(new_pid).await {
            Ok(true) => {
                info!("✅ [RESTORE] Migrated process {} is healthy and running", new_pid);
            }
            Ok(false) => {
                error!("❌ [RESTORE] Migrated process {} is not running after promotion!", new_pid);
                return Err(anyhow::anyhow!("Migrated process died after promotion"));
            }
            Err(e) => {
                warn!("⚠️ [RESTORE] Failed to verify process health: {}", e);
            }
        }

        // Broadcast migration completion to all nodes
        info!("📡 [RESTORE] Broadcasting migration completion...");
        if let Err(e) = self.broadcast_migration_completion(instance_id, new_pid).await {
            warn!("⚠️ [RESTORE] Failed to broadcast migration completion: {}", e);
        }

        info!("🎉 [RESTORE] Migration completed: instance {} is now running with PID {}", instance_id, new_pid);
        Ok(())
    }

    /// Find the PID of the restored process
    async fn find_restored_pid(&self) -> Result<u32> {
        info!("🔍 [PID_SEARCH] Starting search for restored simple_counter process...");

        // First, let's see all running processes to debug
        let ps_output = tokio::process::Command::new("ps")
            .args(&["aux"])
            .output()
            .await?;

        let ps_stdout = String::from_utf8_lossy(&ps_output.stdout);
        info!("🔍 [PID_SEARCH] All running processes containing 'simple_counter':");
        for line in ps_stdout.lines() {
            if line.contains("simple_counter") {
                info!("PS: {}", line);
            }
        }

        // Look for simple_counter processes specifically
        info!("🔍 [PID_SEARCH] Searching for simple_counter processes with pgrep...");
        let output = tokio::process::Command::new("pgrep")
            .arg("-f")  // Full command line match
            .arg("simple_counter")
            .output()
            .await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        info!("🔍 [PID_SEARCH] pgrep stdout: '{}'", stdout);
        info!("🔍 [PID_SEARCH] pgrep stderr: '{}'", stderr);
        info!("🔍 [PID_SEARCH] pgrep exit status: {:?}", output.status);

        if output.status.success() {
            let pids: Vec<u32> = stdout
                .lines()
                .filter_map(|line| {
                    let pid_str = line.trim();
                    match pid_str.parse::<u32>() {
                        Ok(pid) => {
                            info!("🔍 [PID_SEARCH] Found PID candidate: {}", pid);
                            Some(pid)
                        }
                        Err(e) => {
                            warn!("⚠️ [PID_SEARCH] Failed to parse PID '{}': {}", pid_str, e);
                            None
                        }
                    }
                })
                .collect();

            if !pids.is_empty() {
                let selected_pid = pids[0];
                info!("✅ [PID_SEARCH] Found {} PID(s), selected: {}", pids.len(), selected_pid);
                return Ok(selected_pid);
            }
        }

        // Try alternative search with ps + grep
        info!("🔍 [PID_SEARCH] pgrep failed, trying alternative search...");
        let ps_grep_output = tokio::process::Command::new("sh")
            .args(&["-c", "ps aux | grep simple_counter | grep -v grep"])
            .output()
            .await?;

        let ps_grep_stdout = String::from_utf8_lossy(&ps_grep_output.stdout);
        info!("🔍 [PID_SEARCH] ps + grep result: '{}'", ps_grep_stdout);

        if !ps_grep_stdout.trim().is_empty() {
            // Try to extract PID from ps output
            for line in ps_grep_stdout.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    if let Ok(pid) = parts[1].parse::<u32>() {
                        info!("✅ [PID_SEARCH] Found PID from ps output: {}", pid);
                        return Ok(pid);
                    }
                }
            }
        }

        error!("❌ [PID_SEARCH] No simple_counter processes found with any method");
        Err(anyhow::anyhow!("Could not find restored PID - no simple_counter processes running"))
    }

    async fn get_next_data_version(&self, _instance_id: Uuid) -> u64 {
        // For source nodes, we need to track data versions separately
        // since they don't have shadow registry entries for their own instances
        static DATA_VERSION_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        DATA_VERSION_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// Verify that a process is still running and healthy
    async fn verify_process_health(&self, pid: u32) -> Result<bool> {
        // Check if process exists using kill -0
        let output = tokio::process::Command::new("kill")
            .args(&["-0", &pid.to_string()])
            .output()
            .await?;

        if output.status.success() {
            info!("✅ [HEALTH_CHECK] Process {} is running", pid);
            return Ok(true);
        }

        // Double-check with ps command
        let ps_output = tokio::process::Command::new("ps")
            .args(&["-p", &pid.to_string()])
            .output()
            .await?;

        let ps_stdout = String::from_utf8_lossy(&ps_output.stdout);
        let is_running = ps_stdout.lines().count() > 1; // Header + process line

        if is_running {
            info!("✅ [HEALTH_CHECK] Process {} confirmed running via ps", pid);
        } else {
            warn!("❌ [HEALTH_CHECK] Process {} not found", pid);
        }

        Ok(is_running)
    }

    /// Broadcast migration completion to all nodes
    async fn broadcast_migration_completion(&self, instance_id: Uuid, new_pid: u32) -> Result<()> {
        if let Some(network_sender) = &self.network_sender {
            // First, broadcast that this instance is now running on this node
            let instance_info = {
                let instance_manager = self.instance_manager.lock().await;
                if let Some(instance) = instance_manager.get_instance_by_id(&instance_id.to_string()) {
                    Some(InstanceInfo {
                        id: instance.id,
                        program: instance.program.clone(),
                        args: instance.args.clone(),
                        status: instance.status.clone(),
                        node_id: self.local_node_id,
                        created_at: instance.created_at,
                        source_node_id: instance.source_node_id,
                    })
                } else {
                    None
                }
            };

            if let Some(instance_info) = instance_info {
                // Broadcast instance creation (this will update other nodes)
                let sync_message = InstanceSyncMessage {
                    sender_id: self.local_node_id,
                    instances: vec![instance_info],
                    timestamp: Utc::now(),
                };

                let network_message = NetworkMessage::InstanceSync(sync_message);

                if let Err(e) = network_sender.send(network_message) {
                    error!("Failed to broadcast migration completion: {}", e);
                    return Err(anyhow::anyhow!("Failed to broadcast migration completion: {}", e));
                } else {
                    info!("📡 [MIGRATION_BROADCAST] Broadcasted migration completion for instance {} with PID {}", instance_id, new_pid);
                }
            }
        }

        Ok(())
    }
}
