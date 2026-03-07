use clap::{Parser, Subcommand};

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
    },
}
