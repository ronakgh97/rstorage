use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "r-storage",
    version = "1.0.0-gamma",
    about = "RStorage: a super ultra blazingly fast file-storage and sharing with very simple protocol",
    long_about = ""
)]
pub struct ServerArgs {
    #[command(subcommand)]
    pub command: Option<ServerCommands>,
}

#[derive(Subcommand)]
pub enum ServerCommands {
    /// Start the server
    Serve {
        /// Optional port to run the server on
        #[arg(short, long)]
        port: Option<u16>,

        /// Protocol version: v1 (UDP) or v2 (Custom TCP)
        #[arg(long, default_value = "v2")]
        protocol: String,
    },
}

#[derive(Parser)]
#[command(
    name = "r-storage-cli",
    version = "1.0.0-gamma",
    about = "RStorage: a super smol and minimal cli client wrapper for interacting with the r-storage server",
    long_about = ""
)]
pub struct ClientArgs {
    #[command(subcommand)]
    pub command: Option<ClientCommands>,
}

#[derive(Subcommand)]
pub enum ClientCommands {
    /// Upload a file to the server
    Upload {
        /// Path to the file to upload
        #[arg(short, long)]
        file: PathBuf,

        /// Port to connect to the server (default: 3000)
        #[arg(short, long)]
        port: String,

        /// Protocol version: v1 or v2 (Custom TCP)
        #[arg(long)]
        protocol: Option<String>,
    },

    /// Download a file from the server
    Download {
        /// Output path for the downloaded file (default: current directory)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Port to connect to the server (default: 3000)
        #[arg(short, long)]
        port: String,

        /// Protocol version: v1 or v2 (Custom TCP)
        #[arg(long)]
        protocol: Option<String>,
    },
}
