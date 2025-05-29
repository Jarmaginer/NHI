use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StartMode {
    Normal,
    Detached,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    pub id: Uuid,
    pub program: String,
    pub args: Vec<String>,
    pub status: InstanceStatus,
    pub pid: Option<u32>,
    pub created_at: DateTime<Utc>,
    pub working_dir: PathBuf,
    pub checkpoints: HashMap<String, CheckpointInfo>,
    pub start_mode: StartMode,
    pub instance_dir: PathBuf,  // Dedicated folder for this instance
    pub metadata_file: PathBuf, // Path to instance metadata file
    // Shadow state management fields
    pub source_node_id: Option<Uuid>, // Node ID where the running instance is located
    pub shadow_data_version: u64,     // Version counter for shadow data synchronization
    pub last_sync_time: Option<DateTime<Utc>>, // Last time shadow data was synchronized
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum InstanceStatus {
    Starting,
    Running,
    Paused,
    Stopped,
    Failed,
    Shadow,  // Shadow instance - receives real-time data but doesn't execute
}

impl std::fmt::Display for InstanceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstanceStatus::Starting => write!(f, "Starting"),
            InstanceStatus::Running => write!(f, "Running"),
            InstanceStatus::Paused => write!(f, "Paused"),
            InstanceStatus::Stopped => write!(f, "Stopped"),
            InstanceStatus::Failed => write!(f, "Failed"),
            InstanceStatus::Shadow => write!(f, "Shadow"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointInfo {
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub checkpoint_dir: PathBuf,
    pub original_instance_id: Uuid,
}

#[derive(Debug)]
pub struct ProcessInfo {
    pub pid: u32,
    pub child: tokio::process::Child,
    pub output_history: Arc<Mutex<Vec<String>>>,
    pub stdout_handle: Option<tokio::task::JoinHandle<()>>,
    pub stderr_handle: Option<tokio::task::JoinHandle<()>>,
    pub output_sender: Option<tokio::sync::broadcast::Sender<String>>,
    pub stdin_sender: Option<tokio::sync::mpsc::UnboundedSender<String>>,
}

impl Instance {
    pub fn new(program: String, args: Vec<String>, working_dir: PathBuf) -> Self {
        Self::new_with_mode(program, args, working_dir, StartMode::Normal)
    }

    pub fn new_with_mode(program: String, args: Vec<String>, working_dir: PathBuf, start_mode: StartMode) -> Self {
        let id = Uuid::new_v4();
        let instance_dir = Self::create_instance_directory(&id);
        let metadata_file = instance_dir.join("metadata.json");

        Self {
            id,
            program,
            args,
            status: InstanceStatus::Starting,
            pid: None,
            created_at: Utc::now(),
            working_dir,
            checkpoints: HashMap::new(),
            start_mode,
            instance_dir,
            metadata_file,
            source_node_id: None,
            shadow_data_version: 0,
            last_sync_time: None,
        }
    }

    pub fn add_checkpoint(&mut self, name: String, checkpoint_dir: PathBuf) {
        let checkpoint = CheckpointInfo {
            name: name.clone(),
            created_at: Utc::now(),
            checkpoint_dir,
            original_instance_id: self.id,
        };
        self.checkpoints.insert(name, checkpoint);
    }

    pub fn short_id(&self) -> String {
        self.id.to_string()[..8].to_string()
    }

    /// Create instance directory structure
    fn create_instance_directory(id: &Uuid) -> PathBuf {
        let instances_dir = PathBuf::from("instances");
        let short_id = id.to_string()[..8].to_string();
        let instance_dir = instances_dir.join(format!("instance_{}", short_id));

        // Create directory structure
        if let Err(e) = std::fs::create_dir_all(&instance_dir) {
            eprintln!("Warning: Failed to create instance directory {}: {}", instance_dir.display(), e);
        }

        // Create subdirectories
        let subdirs = ["checkpoints", "logs", "scripts", "output"];
        for subdir in &subdirs {
            let subdir_path = instance_dir.join(subdir);
            if let Err(e) = std::fs::create_dir_all(&subdir_path) {
                eprintln!("Warning: Failed to create subdirectory {}: {}", subdir_path.display(), e);
            }
        }

        instance_dir
    }

    /// Get the checkpoints directory for this instance
    pub fn checkpoints_dir(&self) -> PathBuf {
        self.instance_dir.join("checkpoints")
    }

    /// Get the logs directory for this instance
    pub fn logs_dir(&self) -> PathBuf {
        self.instance_dir.join("logs")
    }

    /// Get the scripts directory for this instance
    pub fn scripts_dir(&self) -> PathBuf {
        self.instance_dir.join("scripts")
    }

    /// Get the output directory for this instance
    pub fn output_dir(&self) -> PathBuf {
        self.instance_dir.join("output")
    }

    /// Save instance metadata to file
    pub fn save_metadata(&self) -> Result<()> {
        let metadata_json = serde_json::to_string_pretty(self)
            .map_err(|e| CriuCliError::ParseError(format!("Failed to serialize metadata: {}", e)))?;

        std::fs::write(&self.metadata_file, metadata_json)
            .map_err(|e| CriuCliError::IoError(e))?;

        Ok(())
    }

    /// Load instance metadata from file
    pub fn load_metadata(metadata_file: &PathBuf) -> Result<Self> {
        let metadata_json = std::fs::read_to_string(metadata_file)
            .map_err(|e| CriuCliError::IoError(e))?;

        let instance: Instance = serde_json::from_str(&metadata_json)
            .map_err(|e| CriuCliError::ParseError(format!("Failed to deserialize metadata: {}", e)))?;

        Ok(instance)
    }

    /// Create a shadow instance from an existing instance
    pub fn create_shadow(source_instance: &Instance, source_node_id: Uuid) -> Self {
        let mut shadow = source_instance.clone();
        shadow.status = InstanceStatus::Shadow;
        shadow.pid = None; // Shadow instances don't have actual processes
        shadow.source_node_id = Some(source_node_id);
        shadow.shadow_data_version = 0;
        shadow.last_sync_time = None;
        shadow
    }

    /// Check if this instance is a shadow
    pub fn is_shadow(&self) -> bool {
        self.status == InstanceStatus::Shadow
    }

    /// Check if this instance is running (not shadow)
    pub fn is_running(&self) -> bool {
        self.status == InstanceStatus::Running
    }

    /// Update shadow synchronization data
    pub fn update_shadow_sync(&mut self, version: u64) {
        self.shadow_data_version = version;
        self.last_sync_time = Some(Utc::now());
    }

    /// Convert shadow instance to running instance (for migration)
    pub fn promote_to_running(&mut self, new_pid: u32) {
        self.status = InstanceStatus::Running;
        self.pid = Some(new_pid);
        self.source_node_id = None; // No longer a shadow
    }

    /// Convert running instance to shadow instance (for migration)
    pub fn demote_to_shadow(&mut self, source_node_id: Uuid) {
        self.status = InstanceStatus::Shadow;
        self.pid = None;
        self.source_node_id = Some(source_node_id);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CriuCliError {
    #[error("Instance not found: {0}")]
    InstanceNotFound(String),

    #[error("Instance is not running: {0}")]
    InstanceNotRunning(String),

    #[error("Instance is not paused: {0}")]
    InstanceNotPaused(String),

    #[error("Checkpoint not found: {0}")]
    CheckpointNotFound(String),

    #[error("Process error: {0}")]
    ProcessError(String),

    #[error("CRIU error: {0}")]
    CriuError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Parse error: {0}")]
    ParseError(String),
}

pub type Result<T> = std::result::Result<T, CriuCliError>;
