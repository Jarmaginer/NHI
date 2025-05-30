use crate::message_protocol::*;
use crate::types::{Instance, InstanceStatus};
use crate::criu_manager::CriuManager;
use crate::process_manager::ProcessManager;
use crate::instance::InstanceManager;
use anyhow::{Result, Context};
use chrono::{DateTime, Utc};
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use tokio::process::Command;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::fs::File;
use tracing::{debug, error, info, warn};
use uuid::Uuid;
use std::path::PathBuf;

/// Executes actual instance migration between nodes
pub struct MigrationExecutor {
    local_node_id: NodeId,
    criu_manager: Arc<CriuManager>,
    process_manager: Arc<ProcessManager>,
    instance_manager: Arc<RwLock<InstanceManager>>,
    network_sender: Option<mpsc::UnboundedSender<NetworkMessage>>,
}

/// Migration execution state
#[derive(Debug, Clone)]
pub struct MigrationExecution {
    pub migration_id: Uuid,
    pub instance_id: Uuid,
    pub source_node_id: NodeId,
    pub target_node_id: NodeId,
    pub checkpoint_data: Option<Vec<u8>>,
    pub started_at: DateTime<Utc>,
    pub phase: MigrationPhase,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MigrationPhase {
    Preparing,
    CreatingCheckpoint,
    TransferringData,
    RestoringProcess,
    UpdatingRegistry,
    Completed,
    Failed(String),
}

impl MigrationExecutor {
    pub fn new(
        local_node_id: NodeId,
        criu_manager: Arc<CriuManager>,
        process_manager: Arc<ProcessManager>,
        instance_manager: Arc<RwLock<InstanceManager>>,
    ) -> Self {
        Self {
            local_node_id,
            criu_manager,
            process_manager,
            instance_manager,
            network_sender: None,
        }
    }

    pub fn set_network_sender(&mut self, sender: mpsc::UnboundedSender<NetworkMessage>) {
        self.network_sender = Some(sender);
    }

    /// Execute migration from this node (source) to target node
    pub async fn execute_source_migration(
        &self,
        instance_id: Uuid,
        target_node_id: NodeId,
        migration_id: Uuid,
    ) -> Result<()> {
        info!("Starting source migration for instance {} to node {}", instance_id, target_node_id);

        let mut execution = MigrationExecution {
            migration_id,
            instance_id,
            source_node_id: self.local_node_id,
            target_node_id,
            checkpoint_data: None,
            started_at: Utc::now(),
            phase: MigrationPhase::Preparing,
        };

        // Phase 1: Get instance information
        let (instance_pid, checkpoint_name) = {
            let instance_manager = self.instance_manager.read().await;
            let instance = instance_manager.get_instance_by_id(&instance_id.to_string())
                .ok_or_else(|| anyhow::anyhow!("Instance not found: {}", instance_id))?;

            if instance.status != InstanceStatus::Running {
                return Err(anyhow::anyhow!("Instance is not running: {:?}", instance.status));
            }

            let pid = instance.pid.ok_or_else(|| anyhow::anyhow!("Instance has no PID"))?;
            let checkpoint_name = format!("migration_{}_{}", migration_id, instance_id);

            (pid, checkpoint_name)
        };

        // Phase 2: Create checkpoint using CRIU
        execution.phase = MigrationPhase::CreatingCheckpoint;
        info!("Creating checkpoint for PID {} with name {}", instance_pid, checkpoint_name);

        let checkpoint_result = self.criu_manager
            .create_checkpoint(instance_pid, &checkpoint_name, &instance_id, None)
            .await;

        match checkpoint_result {
            Ok(_) => {
                info!("Checkpoint created successfully: {}", checkpoint_name);
            }
            Err(e) => {
                execution.phase = MigrationPhase::Failed(format!("Checkpoint failed: {}", e));
                error!("Failed to create checkpoint: {}", e);
                return Err(e.into());
            }
        }

        // Phase 3: Read checkpoint data for network transfer
        execution.phase = MigrationPhase::TransferringData;
        let checkpoint_data = self.read_checkpoint_data(&checkpoint_name).await?;
        execution.checkpoint_data = Some(checkpoint_data.clone());

        // Phase 4: Send checkpoint data to target node
        self.send_checkpoint_to_target(
            target_node_id,
            migration_id,
            instance_id,
            checkpoint_data,
        ).await?;

        // Phase 5: Wait for target node confirmation and stop local process
        self.stop_source_process(instance_id).await?;

        execution.phase = MigrationPhase::Completed;
        info!("Source migration completed for instance {}", instance_id);

        Ok(())
    }

    /// Execute migration on target node (restore from checkpoint)
    pub async fn execute_target_migration(
        &self,
        migration_id: Uuid,
        instance_id: Uuid,
        source_node_id: NodeId,
        checkpoint_data: Vec<u8>,
    ) -> Result<()> {
        info!("Starting target migration for instance {} from node {}", instance_id, source_node_id);

        let mut execution = MigrationExecution {
            migration_id,
            instance_id,
            source_node_id,
            target_node_id: self.local_node_id,
            checkpoint_data: Some(checkpoint_data.clone()),
            started_at: Utc::now(),
            phase: MigrationPhase::Preparing,
        };

        // Phase 1: Write checkpoint data to local storage
        execution.phase = MigrationPhase::TransferringData;
        let checkpoint_name = format!("migration_{}_{}", migration_id, instance_id);
        self.write_checkpoint_data(&checkpoint_name, &checkpoint_data).await?;

        // Phase 2: Restore process from checkpoint
        execution.phase = MigrationPhase::RestoringProcess;
        info!("Restoring process from checkpoint: {}", checkpoint_name);

        let restore_result = self.criu_manager
            .restore_checkpoint(&checkpoint_name, Some(&instance_id))
            .await;

        let new_pid = match restore_result {
            Ok((pid, _)) => {
                info!("Process restored successfully with PID: {}", pid);
                pid
            }
            Err(e) => {
                execution.phase = MigrationPhase::Failed(format!("Restore failed: {}", e));
                error!("Failed to restore process: {}", e);
                return Err(e.into());
            }
        };

        // Phase 3: Update instance registry
        execution.phase = MigrationPhase::UpdatingRegistry;
        self.update_instance_after_migration(instance_id, new_pid).await?;

        execution.phase = MigrationPhase::Completed;
        info!("Target migration completed for instance {} with new PID {}", instance_id, new_pid);

        Ok(())
    }

    /// Handle incoming checkpoint data from source node
    pub async fn handle_checkpoint_transfer(
        &self,
        _migration_message: MigrationMessage,
        _checkpoint_data: Vec<u8>,
    ) -> Result<()> {
        // TODO: Update this to work with new MigrationMessage enum
        warn!("Migration executor checkpoint transfer not yet implemented for new message format");
        Ok(())
    }

    // Private helper methods

    async fn read_checkpoint_data(&self, checkpoint_name: &str) -> Result<Vec<u8>> {
        // Use criu-image-streamer to efficiently read checkpoint data
        let temp_dir = std::env::temp_dir().join(format!("nhi_checkpoint_{}", checkpoint_name));
        std::fs::create_dir_all(&temp_dir)?;

        let images_dir = temp_dir.join("images");
        let output_file = temp_dir.join("checkpoint.data");

        // Use criu-image-streamer extract to read checkpoint data
        let mut cmd = Command::new("./criu-image-streamer/target/release/criu-image-streamer");
        cmd.arg("--images-dir").arg(&images_dir)
           .arg("extract");

        let output = cmd.output().await
            .context("Failed to run criu-image-streamer extract")?;

        if !output.status.success() {
            return Err(anyhow::anyhow!("criu-image-streamer extract failed: {}",
                                     String::from_utf8_lossy(&output.stderr)));
        }

        // Read the extracted data
        let checkpoint_data = tokio::fs::read(&output_file).await
            .context("Failed to read checkpoint data")?;

        // Clean up
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;

        Ok(checkpoint_data)
    }

    async fn write_checkpoint_data(&self, checkpoint_name: &str, data: &[u8]) -> Result<()> {
        let temp_dir = std::env::temp_dir().join(format!("nhi_restore_{}", checkpoint_name));
        std::fs::create_dir_all(&temp_dir)?;

        let checkpoint_file = temp_dir.join("checkpoint.data");
        tokio::fs::write(&checkpoint_file, data).await
            .context("Failed to write checkpoint data")?;

        Ok(())
    }

    async fn send_checkpoint_to_target(
        &self,
        target_node_id: NodeId,
        migration_id: Uuid,
        instance_id: Uuid,
        checkpoint_data: Vec<u8>,
    ) -> Result<()> {
        if let Some(network_sender) = &self.network_sender {
            // Send migration start message with checkpoint data
            let migration_message = MigrationMessage::MigrationComplete {
                migration_id,
                success: true,
                error: None,
            };

            // Send the migration message
            let network_message = NetworkMessage::Migration(migration_message);
            network_sender.send(network_message)
                .context("Failed to send migration message")?;

            // Send checkpoint data as data stream
            let stream_message = DataStreamMessage {
                sender_id: self.local_node_id,
                instance_id,
                stream_type: StreamType::Checkpoint,
                data: checkpoint_data,
                sequence_number: 0, // Migration checkpoint is sequence 0
                timestamp: Utc::now(),
            };

            let data_message = NetworkMessage::DataStream(stream_message);
            network_sender.send(data_message)
                .context("Failed to send checkpoint data")?;
        }

        Ok(())
    }

    async fn send_migration_complete(
        &self,
        source_node_id: NodeId,
        migration_id: Uuid,
        instance_id: Uuid,
    ) -> Result<()> {
        if let Some(network_sender) = &self.network_sender {
            let migration_message = MigrationMessage::MigrationComplete {
                migration_id,
                success: true,
                error: None,
            };

            let network_message = NetworkMessage::Migration(migration_message);
            network_sender.send(network_message)
                .context("Failed to send migration complete message")?;
        }

        Ok(())
    }

    async fn stop_source_process(&self, instance_id: Uuid) -> Result<()> {
        let mut instance_manager = self.instance_manager.write().await;
        instance_manager.stop_instance(&instance_id.to_string(), self.process_manager.clone()).await
            .context("Failed to stop source process")?;

        info!("Stopped source process for instance {}", instance_id);
        Ok(())
    }

    async fn update_instance_after_migration(&self, instance_id: Uuid, new_pid: u32) -> Result<()> {
        let mut instance_manager = self.instance_manager.write().await;

        // Update instance status and PID
        if let Some(instance) = instance_manager.get_instance_by_id_mut(&instance_id.to_string()) {
            instance.pid = Some(new_pid);
            instance.status = InstanceStatus::Running;
            instance.source_node_id = None; // No longer a shadow

            // Save updated metadata
            if let Err(e) = instance.save_metadata() {
                warn!("Failed to save instance metadata after migration: {}", e);
            }
        }

        info!("Updated instance {} after migration with PID {}", instance_id, new_pid);
        Ok(())
    }
}
