//! http_status - A tiny HTTP health status server
//!
//! Rust port of the original Go implementation.
//! Checks for service status files in /var/run/http_status/ to respond
//! to health check requests for HAProxy, Keepalived, etc.

use actix_web::{web, App, HttpServer, HttpResponse};
use clap::Parser;
use getifaddrs::InterfaceFlags;
use std::collections::HashMap;
use std::path::PathBuf;

const STATUS_DIR: &str = "/var/run/http_status";

/// A tiny HTTP health status server for L7 health checks
#[derive(Parser, Debug)]
#[command(name = "http_status", version, about)]
struct Args {
    /// Port to listen on
    #[arg(short = 'p', long, default_value_t = 8001)]
    port: u16,

    /// IP address to bind
    #[arg(short = 'i', long, default_value = "127.0.0.1")]
    ip: String,

    /// Run as a daemon
    #[arg(short = 'd', long)]
    daemon: bool,

    /// PID file path
    #[arg(long)]
    pidfile: Option<String>,
}

fn check_vip_match(vip: &str) -> bool {
    let vip_addr: std::net::IpAddr = match vip.parse() {
        Ok(addr) => addr,
        Err(_) => return false,
    };

    let interfaces: Vec<_> = getifaddrs::getifaddrs()
        .map(|it| it.collect())
        .unwrap_or_default();

    for iface in interfaces {
        if iface.flags.contains(InterfaceFlags::LOOPBACK) {
            if let getifaddrs::Address::V4(addr) = &iface.address {
                if addr.address == vip_addr {
                    return true;
                }
            }
            if let getifaddrs::Address::V6(addr) = &iface.address {
                if addr.address == vip_addr {
                    return true;
                }
            }
        }
    }
    false
}

async fn handle_status(
    query: web::Query<HashMap<String, String>>,
) -> HttpResponse {
    let params = query.into_inner();

    // Build the status file name (not PathBuf operations on the dir)
    let mut filename = String::from("status.html");

    if let Some(service) = params.get("SERVICE") {
        filename = service.clone();
    }

    if let Some(port) = params.get("PORT") {
        filename.push_str(&format!(".{}", port));
    }

    // Full path to the status file
    let status_file = PathBuf::from(STATUS_DIR).join(&filename);

    // Check if the status file exists
    let _metadata = match std::fs::metadata(&status_file) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return HttpResponse::NotFound()
                .body("Not Found - Service not available!")
        }
        Err(e) => {
            return HttpResponse::InternalServerError().body(format!("Error: {}", e));
        }
    };

    // Check VIP if provided
    if let Some(vip) = params.get("VIP") {
        if check_vip_match(vip) {
            return HttpResponse::Ok()
                .body("OK - service and VIP all fine!");
        } else {
            return HttpResponse::NotFound()
                .body("Not Found - VIP alias not available!");
        }
    } else {
        return HttpResponse::Ok().body("OK - service fine!");
    }
}

async fn run_server(bind: &str) -> std::io::Result<()> {
    HttpServer::new(|| {
        App::new()
            .route("/status", web::get().to(handle_status))
    })
    .bind(bind)?
    .run()
    .await
}

fn main() {
    let args = Args::parse();

    // Initialize logging
    env_logger::init();

    let listen_addr = format!("{}:{}", args.ip, args.port);

    if args.daemon {
        // Fork into background
        match unsafe { nix::unistd::fork() } {
            Ok(nix::unistd::ForkResult::Child) => {
                // Child process - become session leader
                if nix::unistd::setsid().is_err() {
                    eprintln!("Failed to set session ID");
                    std::process::exit(1);
                }

                // Redirect stdin/stdout/stderr to /dev/null
                if let Ok(null_fd) = nix::fcntl::open(
                    std::path::Path::new("/dev/null"),
                    nix::fcntl::OFlag::O_RDWR,
                    nix::sys::stat::Mode::empty(),
                ) {
                    let _ = nix::unistd::dup2(null_fd, 0); // stdin
                    let _ = nix::unistd::dup2(null_fd, 1); // stdout
                    let _ = nix::unistd::dup2(null_fd, 2); // stderr
                    let _ = nix::unistd::close(null_fd);
                }

                // Set umask to 0
                let _ = nix::sys::stat::umask(nix::sys::stat::Mode::empty());

                // Write PID file if specified
                if let Some(ref pidfile) = args.pidfile {
                    let pid = std::process::id();
                    if let Err(e) = std::fs::write(pidfile, format!("{}", pid)) {
                        eprintln!("Failed to write PID file {}: {}", pidfile, e);
                    }
                }

                // Run the server
                println!("Starting http_status on: {}", listen_addr);
                if let Err(e) = actix_web::rt::System::new().block_on(run_server(&listen_addr)) {
                    eprintln!("Server error: {}", e);
                    std::process::exit(1);
                }
            }
            Ok(nix::unistd::ForkResult::Parent { child, .. }) => {
                // Parent process - exit
                println!("Daemon started with PID {}", child);
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("Fork failed: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        // Run in foreground
        println!("Starting http_status on: {}", listen_addr);
        if let Err(e) = actix_web::rt::System::new().block_on(run_server(&listen_addr)) {
            eprintln!("Server error: {}", e);
            std::process::exit(1);
        }
    }
}
