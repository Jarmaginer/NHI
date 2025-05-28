use anyhow::Result;
use clap::Parser;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info};

mod cli;
mod instance;
mod process_manager;
mod criu_manager;
mod types;
mod ui;
mod tty_utils;
mod colors;

use cli::{CliCommand, CliState};
use instance::InstanceManager;
use process_manager::ProcessManager;
use criu_manager::CriuManager;
use types::*;
use ui::AttachUI;
use uuid::Uuid;
use colors::ColorScheme;

#[derive(Parser)]
#[command(name = "nhi")]
#[command(about = "Interactive CLI for CRIU process management")]
struct Args {
    #[arg(short, long, default_value = "info")]
    log_level: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(&args.log_level)
        .init();

    info!("Starting NHI");

    // Initialize managers
    let process_manager = Arc::new(ProcessManager::new());
    let criu_manager = Arc::new(CriuManager::new());
    let instance_manager = Arc::new(Mutex::new(InstanceManager::new()));

    // Initialize CLI state
    let cli_state = Arc::new(Mutex::new(CliState::new()));

    // Create readline editor
    let mut rl = DefaultEditor::new()?;

    println!("{}", ColorScheme::header("NHI v0.1.0"));
    println!("{}", ColorScheme::info("Type 'help' for available commands or 'exit' to quit."));

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
                        ).await {
                            Ok(_) => {},
                            Err(e) => {
                                error!("Command error: {}", e);
                                println!("Error: {}", e);
                            }
                        }
                    } else {
                        // Forward input to the attached process
                        let manager = instance_manager.lock().await;
                        if let Ok(uuid) = manager.resolve_instance_id(&instance_id) {
                            if let Err(e) = process_manager.send_input(&uuid, line.to_string()).await {
                                error!("Failed to send input to process: {}", e);
                                println!("Error sending input: {}", e);
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
    Ok(())
}

async fn execute_command(
    input: &str,
    cli_state: &Arc<Mutex<CliState>>,
    instance_manager: &Arc<Mutex<InstanceManager>>,
    process_manager: &Arc<ProcessManager>,
    criu_manager: &Arc<CriuManager>,
) -> Result<bool> {
    let command = CliCommand::parse_from_str(input)?;

    match command {
        CliCommand::Help => {
            print_help();
            Ok(false)
        }
        CliCommand::Exit => {
            println!("{}", ColorScheme::success("Goodbye!"));
            Ok(true)
        }
        CliCommand::Start { program, args } => {
            let mut manager = instance_manager.lock().await;
            let instance_id = manager.start_instance(
                program,
                args,
                process_manager.clone(),
            ).await?;
            println!("{} {}",
                ColorScheme::success_indicator("Started instance:"),
                ColorScheme::instance_id(&instance_id)
            );
            Ok(false)
        }
        CliCommand::StartDetached { program, args } => {
            let mut manager = instance_manager.lock().await;
            let instance_id = manager.start_instance_detached(
                program,
                args,
                process_manager.clone(),
            ).await?;
            println!("{} {} {}",
                ColorScheme::success_indicator("Started detached instance:"),
                ColorScheme::instance_id(&instance_id),
                ColorScheme::info("(optimized for CRIU)")
            );
            println!("{} {}",
                ColorScheme::info_indicator("Note:"),
                ColorScheme::info("Detached instances have limited input capabilities but are CRIU-friendly")
            );
            Ok(false)
        }
        CliCommand::Stop { instance_id } => {
            let mut manager = instance_manager.lock().await;
            manager.stop_instance(&instance_id, process_manager.clone()).await?;
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
                    use crate::tty_utils::{detect_tty_environment, print_tty_analysis, check_process_tty_compatibility};

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
    }
}

async fn enter_attach_mode(
    instance_id: &str,
    uuid: Uuid,
    cli_state: &Arc<Mutex<CliState>>,
    instance_manager: &Arc<Mutex<InstanceManager>>,
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
    println!("{}", ColorScheme::header("Aliases:"));
    println!("  {} = {}", ColorScheme::command("startd"), ColorScheme::command("start-detached"));
    println!("  {} = {}", ColorScheme::command("tty"), ColorScheme::command("analyze-tty"));
    println!();
    println!("{}", ColorScheme::header("Tips:"));
    println!("  {} {}", ColorScheme::info_indicator("•"), "Use 'start-detached' for better CRIU checkpoint/restore compatibility");
    println!("  {} {}", ColorScheme::info_indicator("•"), "Use 'analyze-tty' to check if a process is CRIU-friendly");
    println!("  {} {}", ColorScheme::info_indicator("•"), "Detached instances have limited input capabilities but work better with CRIU");
}
