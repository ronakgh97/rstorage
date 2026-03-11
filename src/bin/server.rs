#![allow(clippy::wildcard_in_or_patterns)]

use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use r_drive::args::{ServerArgs, ServerCommands};
use r_drive::service::{serve_tcp_v1, serve_tcp_v2};

#[tokio::main]
async fn main() -> Result<()> {
    let args = ServerArgs::parse();

    match args.command {
        Some(ServerCommands::Serve { port, protocol }) => match protocol.as_str() {
            "v1" => {
                serve_tcp_v1(port).await?;
            }
            "v2" | _ => {
                serve_tcp_v2(port).await?;
            }
        },
        None => {
            ascii_art();
        }
    }

    Ok(())
}

#[inline]
fn ascii_art() {
    let ascii = r"
               ▄▄
               ██       ▀▀
████▄       ▄████ ████▄ ██ ██ ██ ▄█▀█▄
██ ▀▀ ▀▀▀▀▀ ██ ██ ██ ▀▀ ██ ██▄██ ██▄█▀
██          ▀████ ██    ██▄ ▀█▀  ▀█▄▄▄

    ";

    println!("{}", ascii.bright_blue());

    println!(
        "🔗 Github: {}",
        "https://github.com/ronakgh97/rstorage".magenta().bold()
    );
}
