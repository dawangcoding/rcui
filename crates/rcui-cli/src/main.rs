use std::process::Command;

use clap::{Parser, Subcommand};

/// RCUI — A Rust-powered web UI for Claude Code, Cursor, Codex & Gemini
#[derive(Parser)]
#[command(name = "rcui", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Server port
    #[arg(short, long, default_value = "3001")]
    port: u16,

    /// Bind host
    #[arg(long, default_value = "0.0.0.0")]
    host: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the RCUI server
    Start {
        /// Server port
        #[arg(short, long, default_value = "3001")]
        port: u16,
        /// Bind host
        #[arg(long, default_value = "0.0.0.0")]
        host: String,
        /// Path to built frontend (dist/) directory
        #[arg(long)]
        static_dir: Option<String>,
        /// Database path
        #[arg(long)]
        db: Option<String>,
        /// Run in background (daemon mode)
        #[arg(short, long)]
        daemon: bool,
    },
    /// Open the RCUI web interface in the default browser
    Open {
        /// Server port to connect to
        #[arg(short, long, default_value = "3001")]
        port: u16,
    },
    /// Check if the RCUI server is running
    Status {
        /// Server port to check
        #[arg(short, long, default_value = "3001")]
        port: u16,
    },
    /// Stop a running RCUI server
    Stop {
        /// Server port to stop
        #[arg(short, long, default_value = "3001")]
        port: u16,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Start { port, host, static_dir, db, daemon }) => {
            cmd_start(port, &host, static_dir, db, daemon).await;
        }
        Some(Commands::Open { port }) => cmd_open(port),
        Some(Commands::Status { port }) => cmd_status(port).await,
        Some(Commands::Stop { port }) => cmd_stop(port).await,
        None => cmd_start(cli.port, &cli.host, None, None, false).await,
    }
}

async fn cmd_start(
    port: u16,
    host: &str,
    static_dir: Option<String>,
    db: Option<String>,
    daemon: bool,
) {
    let server_bin = find_server_binary();
    println!("Starting RCUI server on {host}:{port}...");
    let mut cmd = Command::new(&server_bin);
    cmd.env("SERVER_PORT", port.to_string());
    cmd.env("HOST", host);
    if let Some(ref dir) = static_dir {
        cmd.env("STATIC_DIR", dir);
    }
    if let Some(ref path) = db {
        cmd.env("DATABASE_PATH", path);
    }
    if daemon {
        match cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(child) => {
                println!("RCUI server started in background (PID: {})", child.id());
                println!("Open http://localhost:{port} in your browser");
            }
            Err(e) => {
                eprintln!("Failed to start server: {e}");
                std::process::exit(1);
            }
        }
    } else {
        println!("Open http://localhost:{port} in your browser");
        println!("Press Ctrl+C to stop\n");
        match cmd.status() {
            Ok(s) if s.success() => {}
            Ok(s) => {
                eprintln!("Server exited with: {s}");
                std::process::exit(s.code().unwrap_or(1));
            }
            Err(e) => {
                eprintln!("Failed to start server: {e}");
                eprintln!("Make sure rcui-server is built (cargo build --release)");
                std::process::exit(1);
            }
        }
    }
}

fn cmd_open(port: u16) {
    let url = format!("http://localhost:{port}");
    println!("Opening {url}...");
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(&url).status();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = Command::new("xdg-open").arg(&url).status();
    }
}

async fn cmd_status(port: u16) {
    let url = format!("http://localhost:{port}/health");
    match reqwest::get(&url).await {
        Ok(resp) if resp.status().is_success() => {
            println!("RCUI server is running on port {port}");
            if let Ok(body) = resp.text().await {
                println!("  Response: {body}");
            }
        }
        Ok(resp) => {
            println!("RCUI server responded with status: {}", resp.status());
        }
        Err(_) => {
            println!("RCUI server is NOT running on port {port}");
            std::process::exit(1);
        }
    }
}

async fn cmd_stop(port: u16) {
    let url = format!("http://localhost:{port}/health");
    match reqwest::get(&url).await {
        Ok(_) => {
            #[cfg(unix)]
            {
                if let Ok(output) = Command::new("lsof")
                    .args(["-ti", &format!(":{port}")])
                    .output()
                {
                    let pids = String::from_utf8_lossy(&output.stdout);
                    for pid in pids.lines() {
                        let pid = pid.trim();
                        if !pid.is_empty() {
                            let _ = Command::new("kill").arg(pid).status();
                        }
                    }
                }
            }
            println!("RCUI server on port {port} stopped");
        }
        Err(_) => {
            println!("No RCUI server running on port {port}");
        }
    }
}

fn find_server_binary() -> String {
    let candidates = [
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("rcui-server")))
            .unwrap_or_default(),
        std::path::PathBuf::from("target/release/rcui-server"),
        std::path::PathBuf::from("target/debug/rcui-server"),
    ];
    for c in &candidates {
        if c.exists() {
            return c.to_string_lossy().to_string();
        }
    }
    "rcui-server".to_string()
}
