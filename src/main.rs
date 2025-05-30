use anyhow::Result;
use clap::Parser;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

mod cli;
mod instance;
mod process_manager;
mod criu_manager;
mod types;
mod ui;
mod tty_utils;
mod colors;
// Stage 2: Networking modules
mod message_protocol;
mod network_manager;
mod node_discovery;
mod cluster_state;
mod node_manager;
// Stage 3: Shadow state and migration modules
mod distributed_registry;
mod shadow_manager;
mod migration_manager;
mod streaming_manager;
mod migration_executor;
mod shadow_instance_manager;
mod logger;
mod output;

use cli::{CliCommand, CliState};
use instance::InstanceManager;
use process_manager::ProcessManager;
use criu_manager::CriuManager;

use ui::AttachUI;
use uuid::Uuid;
use colors::ColorScheme;
// Stage 2: Networking imports
use message_protocol::NetworkConfig;
use network_manager::NetworkManager;
use node_manager::NodeManager;
// Stage 3: Shadow state imports
use shadow_instance_manager::ShadowInstanceManager;
// Stage 4: Migration imports
use migration_manager::MigrationManager;

use crate::output::Output;

#[derive(Parser)]
#[command(name = "nhi")]
#[command(about = "Interactive CLI for CRIU process management")]
struct Args {
    #[arg(short, long, default_value = "info")]
    log_level: String,

    /// Network listen address for P2P connections
    #[arg(long, default_value = "0.0.0.0:8080")]
    listen_addr: String,

    /// Node name for cluster identification
    #[arg(long)]
    node_name: Option<String>,

    /// Discovery port for UDP node discovery
    #[arg(long, default_value = "8081")]
    discovery_port: u16,

    /// Disable networking (Stage 1 compatibility mode)
    #[arg(long)]
    no_network: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging system
    let log_dir = logger::default_log_dir();
    if let Err(e) = logger::init_logging(&log_dir) {
        eprintln!("Failed to initialize logging: {}", e);
        std::process::exit(1);
    }

    info!("Starting NHI");
    Output::header("NHI v0.1.0 - Starting Up");

    // Initialize managers
    let process_manager = Arc::new(ProcessManager::new());
    let criu_manager = Arc::new(CriuManager::new());
    let instance_manager = Arc::new(Mutex::new(InstanceManager::new()));

    // Initialize CLI state
    let cli_state = Arc::new(Mutex::new(CliState::new()));

    // Initialize shadow instance manager (Stage 3)
    let shadow_manager = if !args.no_network {
        Some(Arc::new(tokio::sync::RwLock::new(ShadowInstanceManager::new(
            uuid::Uuid::new_v4(), // Will be updated with actual node ID
            instance_manager.clone(),
            process_manager.clone()
        ))))
    } else {
        None
    };

    // Initialize networking (Stage 2)
    let node_manager = if !args.no_network {
        let network_config = NetworkConfig {
            listen_addr: args.listen_addr.parse()
                .map_err(|e| anyhow::anyhow!("Invalid listen address: {}", e))?,
            node_name: args.node_name.unwrap_or_else(|| {
                format!("nhi-node-{}", uuid::Uuid::new_v4().to_string()[..8].to_uppercase())
            }),
            discovery_port: args.discovery_port,
            heartbeat_interval_secs: 5,   // 更频繁的心跳，5秒间隔
            connection_timeout_secs: 10,
            max_connections: 100,
        };

        let node_manager = Arc::new(NodeManager::new(network_config)?);

        // Start networking
        if let Err(e) = node_manager.start().await {
            error!("Failed to start networking: {}", e);
            Output::warning("Failed to start networking, running in standalone mode");
            None
        } else {
            info!("Networking started successfully");

            // Update shadow manager with actual node ID and set up network sender
            if let Some(ref shadow_mgr) = shadow_manager {
                let node_id = node_manager.node_id();
                let mut shadow_mgr_write = shadow_mgr.write().await;
                // Create a new shadow manager with the correct node ID
                let mut new_shadow_mgr = ShadowInstanceManager::new(node_id, instance_manager.clone(), process_manager.clone());

                // Set up network sender for shadow manager
                let network_sender = node_manager.network_manager().get_sender();
                new_shadow_mgr.set_network_sender(network_sender);

                *shadow_mgr_write = new_shadow_mgr;
            }

            Output::network(&format!("Node {} listening on {}",
                node_manager.node_id().to_string()[..8].to_uppercase(),
                node_manager.local_node_info().listen_addr
            ));
            Some(node_manager)
        }
    } else {
        Output::info("Running in standalone mode (networking disabled)");
        None
    };

    // Initialize migration manager (Stage 4)
    let migration_manager = if !args.no_network {
        if let Some(ref node_mgr) = node_manager {
            let mut mgr = MigrationManager::new(
                node_mgr.node_id(),
                node_mgr.network_manager().clone(),
                instance_manager.clone(),
                process_manager.clone(),
            );

            // Set shadow manager if available
            if let Some(ref shadow_mgr) = shadow_manager {
                mgr.set_shadow_manager(shadow_mgr.clone());
            }

            // Start the migration manager
            if let Err(e) = mgr.start().await {
                warn!("Failed to start migration manager: {}", e);
                None
            } else {
                Some(Arc::new(mgr))
            }
        } else {
            // Even in standalone mode, create a basic migration manager for checkpoint functionality
            warn!("Creating standalone migration manager for checkpoint functionality");
            let dummy_node_id = uuid::Uuid::new_v4();
            let dummy_config = NetworkConfig {
                listen_addr: "127.0.0.1:0".parse().unwrap(),
                node_name: "standalone".to_string(),
                discovery_port: 0,
                heartbeat_interval_secs: 30,
                connection_timeout_secs: 10,
                max_connections: 1,
            };
            let dummy_network_manager = Arc::new(NetworkManager::new(dummy_config, dummy_node_id));

            let mgr = MigrationManager::new(
                dummy_node_id,
                dummy_network_manager,
                instance_manager.clone(),
                process_manager.clone(),
            );

            // Start the migration manager (mainly for checkpoint functionality)
            if let Err(e) = mgr.start().await {
                warn!("Failed to start standalone migration manager: {}", e);
                None
            } else {
                Some(Arc::new(mgr))
            }
        }
    } else {
        None
    };

    // Set up shadow manager in node manager and process manager if available
    if let (Some(ref node_mgr), Some(ref shadow_mgr)) = (&node_manager, &shadow_manager) {
        node_mgr.set_shadow_manager(shadow_mgr.clone()).await;
        process_manager.set_shadow_manager(shadow_mgr.clone()).await;
    }

    // Set up migration manager in node manager if available
    if let (Some(ref node_mgr), Some(ref migration_mgr)) = (&node_manager, &migration_manager) {
        node_mgr.set_migration_manager(migration_mgr.clone()).await;
    }

    // Create readline editor
    let mut rl = DefaultEditor::new()?;

    Output::header("NHI v0.1.0 - Ready");
    Output::info("Type 'help' for available commands or 'exit' to quit.");

    loop {
        let prompt = {
            let state = cli_state.lock().await;
            if let Some(instance_id) = &state.attached_instance {
                format!("nhi [{}]> ", instance_id)
            } else {
                "nhi> ".to_string()
            }
        };

        let readline = rl.readline(&prompt);
        match readline {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                rl.add_history_entry(line)?;

                // Check if we're in attach mode
                let attached_instance = {
                    let state = cli_state.lock().await;
                    state.attached_instance.clone()
                };

                if let Some(instance_id) = attached_instance {
                    // In attach mode - check if it's a special command or forward input
                    if line == "detach" {
                        // Handle detach command
                        match execute_command(
                            line,
                            &cli_state,
                            &instance_manager,
                            &process_manager,
                            &criu_manager,
                            &node_manager,
                            &shadow_manager,
                            &migration_manager,
                        ).await {
                            Ok(_) => {},
                            Err(e) => {
                                error!("Command error: {}", e);
                                println!("Error: {}", e);
                            }
                        }
                    } else {
                        // Forward input to the attached process or shadow instance
                        let manager = instance_manager.lock().await;
                        if let Ok(uuid) = manager.resolve_instance_id(&instance_id) {
                            if let Some(instance) = manager.get_instance_by_id(&uuid.to_string()) {
                                if instance.status == crate::types::InstanceStatus::Shadow {
                                    // Handle shadow instance input forwarding
                                    if let Some(ref shadow_mgr) = shadow_manager {
                                        let shadow_mgr_read = shadow_mgr.read().await;
                                        if let Err(e) = shadow_mgr_read.forward_input_to_source(uuid, line.to_string()).await {
                                            error!("Failed to forward input to source instance: {}", e);
                                            println!("Error forwarding input: {}", e);
                                        }
                                    } else {
                                        println!("Shadow management not available");
                                    }
                                } else {
                                    // Regular instance input forwarding
                                    if let Err(e) = process_manager.send_input(&uuid, line.to_string()).await {
                                        error!("Failed to send input to process: {}", e);
                                        println!("Error sending input: {}", e);
                                    }
                                }
                            } else {
                                println!("Attached instance not found: {}", instance_id);
                            }
                        } else {
                            println!("Attached instance not found: {}", instance_id);
                        }
                    }
                } else {
                    // Normal CLI mode - parse and execute command
                    match execute_command(
                        line,
                        &cli_state,
                        &instance_manager,
                        &process_manager,
                        &criu_manager,
                        &node_manager,
                        &shadow_manager,
                        &migration_manager,
                    ).await {
                        Ok(should_exit) => {
                            if should_exit {
                                break;
                            }
                        }
                        Err(e) => {
                            error!("Command error: {}", e);
                            println!("Error: {}", e);
                        }
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                println!("CTRL-C");
                continue;
            }
            Err(ReadlineError::Eof) => {
                println!("CTRL-D");
                break;
            }
            Err(err) => {
                error!("Error: {:?}", err);
                break;
            }
        }
    }

    info!("Shutting down NHI");

    // Gracefully shutdown networking if enabled
    if let Some(ref node_mgr) = node_manager {
        if let Err(e) = node_mgr.stop().await {
            warn!("Error during node manager shutdown: {}", e);
        }
    }

    Ok(())
}

async fn execute_command(
    input: &str,
    cli_state: &Arc<Mutex<CliState>>,
    instance_manager: &Arc<Mutex<InstanceManager>>,
    process_manager: &Arc<ProcessManager>,
    criu_manager: &Arc<CriuManager>,
    node_manager: &Option<Arc<NodeManager>>,
    shadow_manager: &Option<Arc<tokio::sync::RwLock<ShadowInstanceManager>>>,
    migration_manager: &Option<Arc<MigrationManager>>,
) -> Result<bool> {
    let command = CliCommand::parse_from_str(input)?;

    match command {
        CliCommand::Help => {
            print_help();
            Ok(false)
        }
        CliCommand::Exit => {
            // Gracefully shutdown networking if enabled
            if let Some(ref node_mgr) = node_manager {
                if let Err(e) = node_mgr.stop().await {
                    warn!("Error during node manager shutdown: {}", e);
                }
            }
            Output::success("Goodbye!");
            Ok(true)
        }
        CliCommand::Start { program, args } => {
            let (instance_id, instance) = {
                let mut manager = instance_manager.lock().await;
                let instance_id = manager.start_instance(
                    program,
                    args,
                    process_manager.clone(),
                ).await?;

                // Get the instance for shadow creation
                let instance = manager.get_instance_by_id(&instance_id)
                    .ok_or_else(|| anyhow::anyhow!("Failed to get started instance"))?
                    .clone();

                (instance_id, instance)
            };

            Output::instance(&format!("Started instance: {}", instance_id));

            // Broadcast instance creation to other nodes if networking is enabled
            if let Some(ref shadow_mgr) = shadow_manager {
                let shadow_mgr_read = shadow_mgr.read().await;
                if let Err(e) = shadow_mgr_read.broadcast_instance_creation(&instance).await {
                    warn!("Failed to broadcast instance creation: {}", e);
                    Output::warning("Failed to notify other nodes about instance creation");
                } else {
                    Output::network("Instance creation broadcasted to cluster");
                }
            }

            Ok(false)
        }
        CliCommand::StartDetached { program, args } => {
            let (instance_id, instance) = {
                let mut manager = instance_manager.lock().await;
                let instance_id = manager.start_instance_detached(
                    program,
                    args,
                    process_manager.clone(),
                ).await?;

                // Get the instance for shadow creation
                let instance = manager.get_instance_by_id(&instance_id)
                    .ok_or_else(|| anyhow::anyhow!("Failed to get started instance"))?
                    .clone();

                (instance_id, instance)
            };

            println!("{} {} {}",
                ColorScheme::success_indicator("Started detached instance:"),
                ColorScheme::instance_id(&instance_id),
                ColorScheme::info("(optimized for CRIU)")
            );
            println!("{} {}",
                ColorScheme::info_indicator("Note:"),
                ColorScheme::info("Detached instances have limited input capabilities but are CRIU-friendly")
            );

            // Broadcast instance creation to other nodes if networking is enabled
            if let Some(ref shadow_mgr) = shadow_manager {
                let shadow_mgr_read = shadow_mgr.read().await;
                if let Err(e) = shadow_mgr_read.broadcast_instance_creation(&instance).await {
                    warn!("Failed to broadcast instance creation: {}", e);
                    println!("{} {}",
                        ColorScheme::warning_indicator("Warning:"),
                        ColorScheme::warning("Failed to notify other nodes about instance creation")
                    );
                } else {
                    println!("{} {}",
                        ColorScheme::info_indicator("Broadcast:"),
                        ColorScheme::info("Instance creation broadcasted to cluster")
                    );
                }
            }

            Ok(false)
        }
        CliCommand::Stop { instance_id } => {
            // Get the instance UUID before stopping
            let instance_uuid = {
                let manager = instance_manager.lock().await;
                manager.resolve_instance_id(&instance_id)?
            };

            // Stop the instance
            {
                let mut manager = instance_manager.lock().await;
                manager.stop_instance(&instance_id, process_manager.clone()).await?;
            }

            // Broadcast instance stop to shadow instances if shadow manager is available
            if let Some(ref shadow_mgr) = shadow_manager {
                let shadow_mgr_read = shadow_mgr.read().await;
                if let Err(e) = shadow_mgr_read.broadcast_instance_stop(instance_uuid).await {
                    warn!("Failed to broadcast instance stop: {}", e);
                } else {
                    info!("Broadcasted instance stop for {}", instance_uuid);
                }
            }

            println!("{} {}",
                ColorScheme::success_indicator("Stopped instance:"),
                ColorScheme::instance_id(&instance_id)
            );
            Ok(false)
        }
        CliCommand::Pause { instance_id } => {
            let mut manager = instance_manager.lock().await;
            manager.pause_instance(&instance_id, process_manager.clone()).await?;
            println!("Paused instance: {}", instance_id);
            Ok(false)
        }
        CliCommand::Resume { instance_id } => {
            let mut manager = instance_manager.lock().await;
            manager.resume_instance(&instance_id, process_manager.clone()).await?;
            println!("Resumed instance: {}", instance_id);
            Ok(false)
        }
        CliCommand::List => {
            let manager = instance_manager.lock().await;
            manager.list_instances();
            Ok(false)
        }
        CliCommand::Attach { instance_id } => {
            let manager = instance_manager.lock().await;
            if manager.has_instance(&instance_id) {
                let uuid = manager.resolve_instance_id(&instance_id)?;

                if let Some(instance) = manager.get_instance_by_id(&uuid.to_string()) {
                    if instance.status == crate::types::InstanceStatus::Shadow {
                        // Handle shadow instance attach
                        drop(manager); // Release the lock before entering attach mode
                        match enter_shadow_attach_mode(
                            &instance_id,
                            uuid,
                            &cli_state,
                            &instance_manager,
                            &shadow_manager,
                        ).await {
                            Ok(_) => {},
                            Err(e) => {
                                error!("Failed to enter shadow attach mode: {}", e);
                                println!("Error entering shadow attach mode: {}", e);
                            }
                        }
                    } else {
                        // Handle regular instance attach
                        // Check if the process is actually running
                        if let Some(pid) = process_manager.get_process_pid(&uuid).await {
                            let proc_path = format!("/proc/{}", pid);
                            if !std::path::Path::new(&proc_path).exists() {
                                println!("Error: Instance {} (PID {}) is no longer running", instance_id, pid);
                                println!("Use 'list' to see current instance status");
                                return Ok(false);
                            }
                        } else {
                            println!("Error: Instance {} has no associated process", instance_id);
                            return Ok(false);
                        }

                        drop(manager); // Release the lock before entering attach mode
                        // Enter attach UI mode
                        match enter_attach_mode(
                            &instance_id,
                            uuid,
                            &cli_state,
                            &instance_manager,
                            &process_manager,
                        ).await {
                            Ok(_) => {},
                            Err(e) => {
                                error!("Failed to enter attach mode: {}", e);
                                println!("Error entering attach mode: {}", e);
                            }
                        }
                    }
                } else {
                    println!("Instance not found: {}", instance_id);
                }
            } else {
                println!("Instance not found: {}", instance_id);
            }
            Ok(false)
        }
        CliCommand::Detach => {
            let mut state = cli_state.lock().await;
            if let Some(instance_id) = &state.attached_instance {
                println!("Detached from instance: {}", instance_id);

                // Stop the output monitoring task
                if let Some(task) = state.output_task.take() {
                    task.abort();
                }

                state.attached_instance = None;
            } else {
                println!("Not attached to any instance");
            }
            Ok(false)
        }
        CliCommand::Logs { instance_id, lines } => {
            let target_instance = if let Some(id) = instance_id {
                id
            } else {
                let state = cli_state.lock().await;
                if let Some(attached_id) = &state.attached_instance {
                    attached_id.clone()
                } else {
                    println!("No instance specified and not attached to any instance");
                    return Ok(false);
                }
            };

            let manager = instance_manager.lock().await;
            if let Ok(uuid) = manager.resolve_instance_id(&target_instance) {
                if let Some(history) = process_manager.get_output_history(&uuid).await {
                    let lines_to_show = lines.unwrap_or(20);
                    let start_idx = if history.len() > lines_to_show {
                        history.len() - lines_to_show
                    } else {
                        0
                    };

                    println!("=== Last {} lines of output for instance {} ===",
                             std::cmp::min(lines_to_show, history.len()), target_instance);
                    for line in &history[start_idx..] {
                        println!("{}", line);
                    }
                    println!("=== End of logs ===");
                } else {
                    println!("No output history available for instance: {}", target_instance);
                }
            } else {
                println!("Instance not found: {}", target_instance);
            }
            Ok(false)
        }
        CliCommand::Checkpoint { instance_id, name } => {
            let mut manager = instance_manager.lock().await;
            manager.checkpoint_instance(
                &instance_id,
                &name,
                criu_manager.clone(),
                process_manager.clone(),
            ).await?;
            println!("{} {} {} {}",
                ColorScheme::success_indicator("Created checkpoint"),
                ColorScheme::checkpoint(&name),
                ColorScheme::info("for instance:"),
                ColorScheme::instance_id(&instance_id)
            );
            Ok(false)
        }
        CliCommand::Restore { instance_id, checkpoint_name } => {
            let mut manager = instance_manager.lock().await;
            manager.restore_instance_to_existing(
                &instance_id,
                &checkpoint_name,
                criu_manager.clone(),
                process_manager.clone(),
            ).await?;
            println!("{} {} {} {}",
                ColorScheme::success_indicator("Restored instance"),
                ColorScheme::instance_id(&instance_id),
                ColorScheme::info("from checkpoint:"),
                ColorScheme::checkpoint(&checkpoint_name)
            );
            Ok(false)
        }
        CliCommand::Cd { directory } => {
            std::env::set_current_dir(&directory)?;
            println!("Changed directory to: {}", directory);
            Ok(false)
        }
        CliCommand::AnalyzeTty { instance_id } => {
            let manager = instance_manager.lock().await;
            if let Ok(uuid) = manager.resolve_instance_id(&instance_id) {
                if let Some(pid) = process_manager.get_process_pid(&uuid).await {
                    use crate::tty_utils::check_process_tty_compatibility;

                    println!("Analyzing TTY environment for instance {} (PID: {})...", instance_id, pid);

                    match check_process_tty_compatibility(pid) {
                        Ok(is_compatible) => {
                            if is_compatible {
                                println!("✅ Process has good CRIU compatibility");
                            } else {
                                println!("⚠️  Process may have CRIU compatibility issues");
                                println!("💡 Consider using 'start-detached' for better CRIU compatibility");
                            }
                        }
                        Err(e) => {
                            println!("❌ Failed to analyze TTY environment: {}", e);
                        }
                    }
                } else {
                    println!("Instance {} is not running", instance_id);
                }
            } else {
                println!("Instance not found: {}", instance_id);
            }
            Ok(false)
        }
        // Cluster management commands (Stage 2)
        CliCommand::ClusterListNodes => {
            if let Some(ref node_mgr) = node_manager {
                let node_list = node_mgr.get_node_list().await;
                println!("{}", node_list);
            } else {
                println!("{} {}",
                    ColorScheme::warning_indicator("Warning:"),
                    ColorScheme::warning("Networking is disabled. Use --help to see networking options.")
                );
            }
            Ok(false)
        }
        CliCommand::ClusterNodeInfo { node_id } => {
            if let Some(ref node_mgr) = node_manager {
                if let Some(node_id_str) = node_id {
                    // Parse node ID and show specific node info
                    match uuid::Uuid::parse_str(&node_id_str) {
                        Ok(uuid) => {
                            if let Some(node_info) = node_mgr.cluster_state().get_node_info(&uuid).await {
                                println!("Node Information:");
                                println!("  ID: {}", node_info.node_id);
                                println!("  Name: {}", node_info.name);
                                println!("  Address: {}", node_info.listen_addr);
                                println!("  Status: {:?}", node_info.status);
                                println!("  Version: {}", node_info.version);
                                println!("  Joined: {}", node_info.joined_at.format("%Y-%m-%d %H:%M:%S UTC"));
                                println!("  Last Seen: {}", node_info.last_seen.format("%Y-%m-%d %H:%M:%S UTC"));
                                println!("  Capabilities: {}", node_info.capabilities.join(", "));
                            } else {
                                println!("Node not found: {}", node_id_str);
                            }
                        }
                        Err(_) => {
                            println!("Invalid node ID format: {}", node_id_str);
                        }
                    }
                } else {
                    // Show local node info
                    let local_info = node_mgr.local_node_info();
                    println!("Local Node Information:");
                    println!("  ID: {}", local_info.node_id);
                    println!("  Name: {}", local_info.name);
                    println!("  Address: {}", local_info.listen_addr);
                    println!("  Status: {:?}", local_info.status);
                    println!("  Version: {}", local_info.version);
                    println!("  Capabilities: {}", local_info.capabilities.join(", "));
                }
            } else {
                println!("{} {}",
                    ColorScheme::warning_indicator("Warning:"),
                    ColorScheme::warning("Networking is disabled. Use --help to see networking options.")
                );
            }
            Ok(false)
        }
        CliCommand::ClusterConnect { address } => {
            if let Some(ref node_mgr) = node_manager {
                match address.parse::<std::net::SocketAddr>() {
                    Ok(addr) => {
                        println!("Connecting to {}...", addr);
                        match node_mgr.connect_to_peer(addr).await {
                            Ok(_) => {
                                println!("{} {}",
                                    ColorScheme::success_indicator("Success:"),
                                    ColorScheme::success(&format!("Connection initiated to {}", addr))
                                );
                            }
                            Err(e) => {
                                println!("{} {}",
                                    ColorScheme::error_indicator("Error:"),
                                    ColorScheme::error(&format!("Failed to connect to {}: {}", addr, e))
                                );
                            }
                        }
                    }
                    Err(e) => {
                        println!("{} {}",
                            ColorScheme::error_indicator("Error:"),
                            ColorScheme::error(&format!("Invalid address format: {}", e))
                        );
                    }
                }
            } else {
                println!("{} {}",
                    ColorScheme::warning_indicator("Warning:"),
                    ColorScheme::warning("Networking is disabled. Use --help to see networking options.")
                );
            }
            Ok(false)
        }
        CliCommand::ClusterDisconnect { node_id } => {
            if let Some(ref node_mgr) = node_manager {
                match uuid::Uuid::parse_str(&node_id) {
                    Ok(uuid) => {
                        match node_mgr.disconnect_peer(&uuid).await {
                            Ok(_) => {
                                println!("{} {}",
                                    ColorScheme::success_indicator("Success:"),
                                    ColorScheme::success(&format!("Disconnected from node {}", node_id))
                                );
                            }
                            Err(e) => {
                                println!("{} {}",
                                    ColorScheme::error_indicator("Error:"),
                                    ColorScheme::error(&format!("Failed to disconnect from {}: {}", node_id, e))
                                );
                            }
                        }
                    }
                    Err(_) => {
                        println!("{} {}",
                            ColorScheme::error_indicator("Error:"),
                            ColorScheme::error("Invalid node ID format")
                        );
                    }
                }
            } else {
                println!("{} {}",
                    ColorScheme::warning_indicator("Warning:"),
                    ColorScheme::warning("Networking is disabled. Use --help to see networking options.")
                );
            }
            Ok(false)
        }
        CliCommand::ClusterStatus => {
            if let Some(ref node_mgr) = node_manager {
                let cluster_info = node_mgr.get_cluster_info().await;
                println!("{}", cluster_info);

                // Also show connected peers
                let peers = node_mgr.get_connected_peers().await;
                if !peers.is_empty() {
                    println!("\nActive Connections:");
                    for (peer_id, addr) in peers {
                        println!("  {} - {}",
                            peer_id.to_string()[..8].to_uppercase(),
                            addr
                        );
                    }
                } else {
                    println!("\nNo active connections");
                }
            } else {
                println!("{} {}",
                    ColorScheme::warning_indicator("Warning:"),
                    ColorScheme::warning("Networking is disabled. Use --help to see networking options.")
                );
            }
            Ok(false)
        }
        CliCommand::Migrate { instance_id, target_node_id } => {
            if let Some(ref node_mgr) = node_manager {
                match uuid::Uuid::parse_str(&target_node_id) {
                    Ok(target_uuid) => {
                        // Check if instance exists
                        if !instance_manager.lock().await.has_instance(&instance_id) {
                            println!("{} {}",
                                ColorScheme::error_indicator("Error:"),
                                ColorScheme::error(&format!("Instance '{}' not found", instance_id))
                            );
                            return Ok(false);
                        }

                        // Check if target node exists in cluster
                        let cluster_state = node_mgr.cluster_state();
                        let nodes = cluster_state.get_online_nodes().await;

                        if !nodes.iter().any(|node| node.node_id == target_uuid) {
                            println!("{} {}",
                                ColorScheme::error_indicator("Error:"),
                                ColorScheme::error(&format!("Target node '{}' not found in cluster", target_node_id))
                            );
                            return Ok(false);
                        }

                        println!("{} {} {} {}",
                            ColorScheme::info_indicator("Migration:"),
                            ColorScheme::info("Starting migration of instance"),
                            ColorScheme::instance_id(&instance_id),
                            ColorScheme::info(&format!("to node {}", target_node_id))
                        );

                        // Use migration manager to initiate migration
                        if let Some(ref migration_mgr) = migration_manager {
                            // Use default migration options
                            let options = crate::migration_manager::MigrationOptions::default();

                            // Initiate migration
                            match migration_mgr.migrate_instance(&instance_id, target_uuid, options).await {
                                Ok(migration_id) => {
                                    println!("{} {} {}",
                                        ColorScheme::success_indicator("Success:"),
                                        ColorScheme::success("Migration request initiated with ID:"),
                                        ColorScheme::info(&migration_id.to_string()[..8])
                                    );
                                    println!("{} {}",
                                        ColorScheme::info_indicator("Note:"),
                                        ColorScheme::info("Migration is running in the background. Use 'list' to check status.")
                                    );
                                }
                                Err(e) => {
                                    println!("{} {}",
                                        ColorScheme::error_indicator("Error:"),
                                        ColorScheme::error(&format!("Failed to initiate migration: {}", e))
                                    );
                                }
                            }
                        } else {
                            println!("{} {}",
                                ColorScheme::warning_indicator("Warning:"),
                                ColorScheme::warning("Migration manager is not available.")
                            );
                        }
                    }
                    Err(_) => {
                        println!("{} {}",
                            ColorScheme::error_indicator("Error:"),
                            ColorScheme::error("Invalid target node ID format")
                        );
                    }
                }
            } else {
                println!("{} {}",
                    ColorScheme::warning_indicator("Warning:"),
                    ColorScheme::warning("Networking is disabled. Migration requires networking.")
                );
            }
            Ok(false)
        }
        CliCommand::ShadowView { instance_id: _ } => {
            println!("{} {}",
                ColorScheme::info_indicator("Shadow View:"),
                ColorScheme::info("Shadow view functionality will be implemented after core sync is working")
            );
            Ok(false)
        }
    }
}

async fn enter_attach_mode(
    instance_id: &str,
    uuid: Uuid,
    cli_state: &Arc<Mutex<CliState>>,
    _instance_manager: &Arc<Mutex<InstanceManager>>,
    process_manager: &Arc<ProcessManager>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut ui = AttachUI::new()?;
    ui.enter_attach_mode(instance_id)?;

    // Get historical output and display it
    if let Some(history) = process_manager.get_output_history(&uuid).await {
        for line in &history {
            ui.add_output_line(line.clone())?;
        }
    }

    // Subscribe to real-time output
    let mut output_receiver = process_manager.subscribe_to_output(&uuid).await;

    // Set attached state
    {
        let mut state = cli_state.lock().await;
        state.attached_instance = Some(instance_id.to_string());
    }

    // Main attach loop
    loop {
        // Handle real-time output
        if let Some(ref mut receiver) = output_receiver {
            match receiver.try_recv() {
                Ok(output) => {
                    ui.add_output_line(output)?;
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    // No new output, continue
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                    // We're lagging behind, continue
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    // Output stream closed, process might have ended
                    ui.add_output_line("[Process output stream closed]".to_string())?;
                }
            }
        }

        // Handle user input
        if let Some(input) = ui.handle_input()? {
            if input == "detach" {
                break;
            } else {
                // Forward input to process
                if let Err(e) = process_manager.send_input(&uuid, input).await {
                    ui.add_output_line(format!("[Error sending input: {}]", e))?;
                }
            }
        }

        // Small delay to prevent busy waiting
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    // Clean up
    ui.exit_attach_mode()?;

    // Clear attached state
    {
        let mut state = cli_state.lock().await;
        state.attached_instance = None;
        if let Some(task) = state.output_task.take() {
            task.abort();
        }
    }

    println!("Detached from instance: {}", instance_id);
    Ok(())
}

async fn enter_shadow_attach_mode(
    instance_id: &str,
    uuid: Uuid,
    cli_state: &Arc<Mutex<CliState>>,
    _instance_manager: &Arc<Mutex<InstanceManager>>,
    shadow_manager: &Option<Arc<tokio::sync::RwLock<ShadowInstanceManager>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let shadow_mgr = match shadow_manager {
        Some(mgr) => mgr,
        None => {
            println!("Shadow management is not available (networking disabled)");
            return Ok(());
        }
    };

    let mut ui = AttachUI::new()?;
    ui.enter_attach_mode(&format!("{} (Shadow)", instance_id))?;

    // Get shadow instance info and display historical output
    {
        let shadow_mgr_read = shadow_mgr.read().await;
        if let Some(shadow_info) = shadow_mgr_read.get_shadow_instance(uuid).await {
            ui.add_output_line(format!("[Shadow Instance - Source Node: {}]",
                shadow_info.source_node_id.to_string()[..8].to_uppercase()))?;
            ui.add_output_line(format!("[Last Sync: {}]",
                shadow_info.last_sync_time.format("%H:%M:%S")))?;

            // Display historical output from buffer
            if !shadow_info.output_buffer.is_empty() {
                if let Ok(output_str) = String::from_utf8(shadow_info.output_buffer.clone()) {
                    for line in output_str.lines() {
                        if !line.is_empty() {
                            ui.add_output_line(line.to_string())?;
                        }
                    }
                }
            }
        }
    }

    // Set attached state
    {
        let mut state = cli_state.lock().await;
        state.attached_instance = Some(instance_id.to_string());
    }

    // Track what we've already displayed to show only new content
    let mut last_displayed_size = 0;

    // Main loop for shadow attach mode
    loop {
        // Check for new shadow sync data and display new output
        {
            let shadow_mgr_read = shadow_mgr.read().await;
            if let Some(shadow_info) = shadow_mgr_read.get_shadow_instance(uuid).await {
                // Check if there's new output data
                if shadow_info.output_buffer.len() > last_displayed_size {
                    // Get the new content
                    let new_content = &shadow_info.output_buffer[last_displayed_size..];
                    if let Ok(new_output) = String::from_utf8(new_content.to_vec()) {
                        for line in new_output.lines() {
                            if !line.is_empty() {
                                ui.add_output_line(line.to_string())?;
                            }
                        }
                    }
                    last_displayed_size = shadow_info.output_buffer.len();
                }
            }
        }

        // Handle user input
        if let Some(input) = ui.handle_input()? {
            if input == "detach" {
                break;
            } else {
                // Forward input to the source node through shadow manager
                let shadow_mgr_read = shadow_mgr.read().await;
                if let Err(e) = shadow_mgr_read.forward_input_to_source(uuid, input.clone()).await {
                    ui.add_output_line(format!("[Shadow] Failed to forward input: {}", e))?;
                } else {
                    ui.add_output_line(format!("[Shadow] Forwarded input: {}", input))?;
                }
            }
        }

        // Small delay to prevent busy waiting
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }

    // Clean up
    ui.exit_attach_mode()?;

    // Clear attached state
    {
        let mut state = cli_state.lock().await;
        state.attached_instance = None;
        if let Some(task) = state.output_task.take() {
            task.abort();
        }
    }

    println!("Detached from shadow instance: {}", instance_id);
    Ok(())
}

fn print_help() {
    println!("{}", ColorScheme::header("Available commands:"));
    println!("  {} {} - {}", ColorScheme::command("start"), ColorScheme::info("<program> [args...]"), "Start a new program instance");
    println!("  {} {} - {}", ColorScheme::command("start-detached"), ColorScheme::info("<program> [args...]"), "Start a detached instance (CRIU-optimized)");
    println!("  {} {} - {}", ColorScheme::command("stop"), ColorScheme::info("<instance_id>"), "Stop an instance");
    println!("  {} {} - {}", ColorScheme::command("pause"), ColorScheme::info("<instance_id>"), "Pause an instance");
    println!("  {} {} - {}", ColorScheme::command("resume"), ColorScheme::info("<instance_id>"), "Resume a paused instance");
    println!("  {} - {}", ColorScheme::command("list"), "List all instances");
    println!("  {} {} - {}", ColorScheme::command("attach"), ColorScheme::info("<instance_id>"), "Enter instance mode (shows historical output)");
    println!("  {} - {}", ColorScheme::command("detach"), "Exit instance mode");
    println!("  {} {} - {}", ColorScheme::command("logs"), ColorScheme::info("[instance_id] [lines]"), "Show recent output (default: current instance, 20 lines)");
    println!("  {} {} - {}", ColorScheme::command("checkpoint"), ColorScheme::info("<instance_id> <name>"), "Create a checkpoint");
    println!("  {} {} - {}", ColorScheme::command("restore"), ColorScheme::info("<instance_id> <checkpoint_name>"), "Restore instance from checkpoint");
    println!("  {} {} - {}", ColorScheme::command("analyze-tty"), ColorScheme::info("<instance_id>"), "Analyze TTY environment for CRIU compatibility");
    println!("  {} {} - {}", ColorScheme::command("cd"), ColorScheme::info("<directory>"), "Change working directory");
    println!("  {} - {}", ColorScheme::command("help"), "Show this help");
    println!("  {} - {}", ColorScheme::command("exit"), "Exit the CLI");
    println!();
    println!("{}", ColorScheme::header("Cluster Commands (Stage 2):"));
    println!("  {} {} - {}", ColorScheme::command("cluster list-nodes"), ColorScheme::info(""), "List all nodes in the cluster");
    println!("  {} {} - {}", ColorScheme::command("cluster node-info"), ColorScheme::info("[node_id]"), "Show node information (local if no ID)");
    println!("  {} {} - {}", ColorScheme::command("cluster connect"), ColorScheme::info("<address>"), "Connect to a peer node");
    println!("  {} {} - {}", ColorScheme::command("cluster disconnect"), ColorScheme::info("<node_id>"), "Disconnect from a peer node");
    println!("  {} {} - {}", ColorScheme::command("cluster status"), ColorScheme::info(""), "Show cluster status and connections");
    println!();
    println!("{}", ColorScheme::header("Migration Commands (Stage 3):"));
    println!("  {} {} - {}", ColorScheme::command("migrate"), ColorScheme::info("<instance_id> <target_node_id>"), "Migrate instance to another node");
    println!("  {} {} - {}", ColorScheme::command("shadow-view"), ColorScheme::info("<instance_id>"), "View shadow instance output and status");
    println!();
    println!("{}", ColorScheme::header("Aliases:"));
    println!("  {} = {}", ColorScheme::command("startd"), ColorScheme::command("start-detached"));
    println!("  {} = {}", ColorScheme::command("tty"), ColorScheme::command("analyze-tty"));
    println!("  {} = {}", ColorScheme::command("cluster nodes"), ColorScheme::command("cluster list-nodes"));
    println!("  {} = {}", ColorScheme::command("cluster info"), ColorScheme::command("cluster node-info"));
    println!();
    println!("{}", ColorScheme::header("Tips:"));
    println!("  {} {}", ColorScheme::info_indicator("•"), "Use 'start-detached' for better CRIU checkpoint/restore compatibility");
    println!("  {} {}", ColorScheme::info_indicator("•"), "Use 'analyze-tty' to check if a process is CRIU-friendly");
    println!("  {} {}", ColorScheme::info_indicator("•"), "Detached instances have limited input capabilities but work better with CRIU");
    println!("  {} {}", ColorScheme::info_indicator("•"), "Use --no-network to disable P2P networking (Stage 1 compatibility mode)");
    println!("  {} {}", ColorScheme::info_indicator("•"), "Nodes auto-discover each other on the local network");
}
