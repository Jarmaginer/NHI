use crate::message_protocol::*;
use crate::types::{Instance, InstanceStatus};
use crate::process_manager::ProcessManager;
use anyhow::{Result, Context};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use std::path::PathBuf;
use tokio::sync::{RwLock, mpsc};
use tokio::process::Command;

use tracing::{debug, error, info};
use uuid::Uuid;
use std::os::unix::io::FromRawFd;
use std::fs::File;

/// Real-time streaming manager for shadow state synchronization over network
pub struct StreamingManager {
    local_node_id: NodeId,
    active_streams: Arc<RwLock<HashMap<Uuid, StreamingSession>>>,
    network_sender: Option<mpsc::UnboundedSender<NetworkMessage>>,
    process_manager: Arc<ProcessManager>,
}

/// A streaming session for an instance
#[derive(Debug, Clone)]
pub struct StreamingSession {
    pub instance_id: Uuid,
    pub source_node_id: NodeId,
    pub is_source: bool, // true if this node is the source (running instance)
    pub temp_dir: PathBuf,
    pub last_checkpoint_time: Option<DateTime<Utc>>,
    pub shadow_data: ShadowInstanceData,
}

/// Shadow instance data maintained on non-source nodes
#[derive(Debug, Clone)]
pub struct ShadowInstanceData {
    pub latest_checkpoint: Option<Vec<u8>>,
    pub output_buffer: Vec<u8>,
    pub last_sync_time: DateTime<Utc>,
    pub data_version: u64,
}

impl StreamingManager {
    pub fn new(local_node_id: NodeId, process_manager: Arc<ProcessManager>) -> Self {
        Self {
            local_node_id,
            active_streams: Arc::new(RwLock::new(HashMap::new())),
            network_sender: None,
            process_manager,
        }
    }

    /// Set the network sender for broadcasting streaming data
    pub fn set_network_sender(&mut self, sender: mpsc::UnboundedSender<NetworkMessage>) {
        self.network_sender = Some(sender);
    }

    /// Start streaming for a running instance (source node)
    pub async fn start_source_streaming(&self, instance: &Instance) -> Result<()> {
        if instance.status != InstanceStatus::Running {
            return Err(anyhow::anyhow!("Can only start streaming for running instances"));
        }

        let temp_dir = self.create_temp_dir(&instance.id)?;

        let session = StreamingSession {
            instance_id: instance.id,
            source_node_id: self.local_node_id,
            is_source: true,
            temp_dir,
            last_checkpoint_time: None,
            shadow_data: ShadowInstanceData {
                latest_checkpoint: None,
                output_buffer: Vec::new(),
                last_sync_time: Utc::now(),
                data_version: 0,
            },
        };

        {
            let mut streams = self.active_streams.write().await;
            streams.insert(instance.id, session);
        }

        info!("Started source streaming for instance {}", instance.short_id());
        Ok(())
    }

    /// Start streaming for a shadow instance (target node)
    pub async fn start_shadow_streaming(&self, instance: &Instance, source_node_id: NodeId) -> Result<()> {
        if instance.status != InstanceStatus::Shadow {
            return Err(anyhow::anyhow!("Can only start shadow streaming for shadow instances"));
        }

        let temp_dir = self.create_temp_dir(&instance.id)?;

        let session = StreamingSession {
            instance_id: instance.id,
            source_node_id,
            is_source: false,
            temp_dir,
            last_checkpoint_time: None,
            shadow_data: ShadowInstanceData {
                latest_checkpoint: None,
                output_buffer: Vec::new(),
                last_sync_time: Utc::now(),
                data_version: 0,
            },
        };

        {
            let mut streams = self.active_streams.write().await;
            streams.insert(instance.id, session);
        }

        info!("Started shadow streaming for instance {} from source node {}",
              instance.short_id(), source_node_id);
        Ok(())
    }

    /// Stream checkpoint data from source to all shadow instances
    pub async fn stream_checkpoint_data(&self, instance_id: Uuid, checkpoint_data: Vec<u8>) -> Result<()> {
        if let Some(network_sender) = &self.network_sender {
            let stream_message = DataStreamMessage {
                sender_id: self.local_node_id,
                instance_id,
                stream_type: StreamType::Checkpoint,
                data: checkpoint_data,
                sequence_number: self.get_next_sequence_number(instance_id).await,
                timestamp: Utc::now(),
            };

            let network_message = NetworkMessage::DataStream(stream_message);

            if let Err(e) = network_sender.send(network_message) {
                error!("Failed to send checkpoint data stream: {}", e);
                return Err(anyhow::anyhow!("Failed to send checkpoint data: {}", e));
            }

            // Update last checkpoint time
            {
                let mut streams = self.active_streams.write().await;
                if let Some(session) = streams.get_mut(&instance_id) {
                    session.last_checkpoint_time = Some(Utc::now());
                }
            }
        }

        Ok(())
    }

    /// Stream output data from source to all shadow instances
    pub async fn stream_output_data(&self, instance_id: Uuid, output_data: Vec<u8>, stream_type: StreamType) -> Result<()> {
        if let Some(network_sender) = &self.network_sender {
            let stream_message = DataStreamMessage {
                sender_id: self.local_node_id,
                instance_id,
                stream_type,
                data: output_data,
                sequence_number: self.get_next_sequence_number(instance_id).await,
                timestamp: Utc::now(),
            };

            let network_message = NetworkMessage::DataStream(stream_message);

            if let Err(e) = network_sender.send(network_message) {
                error!("Failed to send output data stream: {}", e);
                return Err(anyhow::anyhow!("Failed to send output data: {}", e));
            }
        }

        Ok(())
    }

    /// Handle incoming data stream from another node
    pub async fn handle_incoming_stream(&self, stream_message: DataStreamMessage) -> Result<()> {
        let streams = self.active_streams.read().await;

        if let Some(session) = streams.get(&stream_message.instance_id) {
            if !session.is_source && session.source_node_id == stream_message.sender_id {
                match stream_message.stream_type {
                    StreamType::Checkpoint => {
                        self.write_checkpoint_data_to_shadow(&stream_message).await?;
                    }
                    StreamType::Stdout | StreamType::Stderr => {
                        self.write_output_data_to_shadow(&stream_message).await?;
                    }
                    StreamType::Stdin => {
                        // Forward input to the running instance
                        self.forward_input_to_source(&stream_message).await?;
                    }
                    StreamType::Memory => {
                        self.write_memory_data_to_shadow(&stream_message).await?;
                    }
                }
            }
        }

        Ok(())
    }

    /// Create a checkpoint using criu-image-streamer and stream it
    pub async fn create_and_stream_checkpoint(&self, instance_id: Uuid, pid: u32) -> Result<()> {
        let streams = self.active_streams.read().await;

        if let Some(session) = streams.get(&instance_id) {
            if session.is_source {
                // Use CRIU to create checkpoint and stream it via criu-image-streamer
                let checkpoint_data = self.create_streaming_checkpoint(pid, &session.temp_dir).await?;
                drop(streams); // Release the lock before streaming

                self.stream_checkpoint_data(instance_id, checkpoint_data).await?;
                info!("Created and streamed checkpoint for instance {}", instance_id);
            }
        }

        Ok(())
    }

    /// Stop streaming for an instance
    pub async fn stop_streaming(&self, instance_id: Uuid) -> Result<()> {
        let mut streams = self.active_streams.write().await;

        if let Some(session) = streams.remove(&instance_id) {
            // Clean up temporary directory
            let _ = std::fs::remove_dir_all(&session.temp_dir);

            info!("Stopped streaming for instance {}", instance_id);
        }

        Ok(())
    }

    /// Get all active streaming sessions
    pub async fn get_active_sessions(&self) -> HashMap<Uuid, StreamingSession> {
        let streams = self.active_streams.read().await;
        streams.clone()
    }

    // Private helper methods

    fn create_temp_dir(&self, instance_id: &Uuid) -> Result<PathBuf> {
        let temp_dir = std::env::temp_dir().join(format!("nhi_streaming_{}", instance_id));
        std::fs::create_dir_all(&temp_dir)?;
        Ok(temp_dir)
    }

    fn create_pipe_pair(&self) -> Result<(File, File)> {
        let (read_fd, write_fd) = nix::unistd::pipe()?;

        // Convert to File objects
        let read_file = unsafe { File::from_raw_fd(read_fd) };
        let write_file = unsafe { File::from_raw_fd(write_fd) };

        Ok((read_file, write_file))
    }

    async fn start_capture_process(&self, images_dir: &PathBuf, output_pipe: File) -> Result<tokio::process::Child> {
        let mut cmd = Command::new("./criu-image-streamer/target/release/criu-image-streamer");
        cmd.arg("--images-dir").arg(images_dir)
           .arg("capture")
           .stdout(std::process::Stdio::from(output_pipe))
           .stderr(std::process::Stdio::piped());

        let child = cmd.spawn()
            .context("Failed to start criu-image-streamer capture process")?;

        Ok(child)
    }

    async fn start_serve_process(&self, images_dir: &PathBuf, input_pipe: File) -> Result<tokio::process::Child> {
        let mut cmd = Command::new("./criu-image-streamer/target/release/criu-image-streamer");
        cmd.arg("--images-dir").arg(images_dir)
           .arg("serve")
           .stdin(std::process::Stdio::from(input_pipe))
           .stderr(std::process::Stdio::piped());

        let child = cmd.spawn()
            .context("Failed to start criu-image-streamer serve process")?;

        Ok(child)
    }

    async fn create_streaming_checkpoint(&self, pid: u32, _temp_dir: &PathBuf) -> Result<Vec<u8>> {
        // This would integrate with CRIU to create a checkpoint and capture it via streaming
        // For now, return a placeholder
        info!("Creating streaming checkpoint for PID {}", pid);
        Ok(vec![0u8; 1024]) // Placeholder checkpoint data
    }

    async fn write_checkpoint_data_to_shadow(&self, stream_message: &DataStreamMessage) -> Result<()> {
        debug!("Writing checkpoint data to shadow instance {}", stream_message.instance_id);
        // Write the checkpoint data to the shadow instance's serve process
        Ok(())
    }

    async fn write_output_data_to_shadow(&self, stream_message: &DataStreamMessage) -> Result<()> {
        debug!("Writing output data to shadow instance {}", stream_message.instance_id);
        // Write the output data to the shadow instance's buffer
        Ok(())
    }

    async fn write_memory_data_to_shadow(&self, stream_message: &DataStreamMessage) -> Result<()> {
        debug!("Writing memory data to shadow instance {}", stream_message.instance_id);
        // Write the memory data to the shadow instance
        Ok(())
    }

    async fn forward_input_to_source(&self, stream_message: &DataStreamMessage) -> Result<()> {
        debug!("Forwarding input to source instance {}", stream_message.instance_id);
        // Forward input to the running instance
        Ok(())
    }

    async fn get_next_sequence_number(&self, _instance_id: Uuid) -> u64 {
        // Simple sequence number generation - in production this should be more sophisticated
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }
}
