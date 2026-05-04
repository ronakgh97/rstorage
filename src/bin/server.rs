use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use r_drive::args::{ServerArgs, ServerCommands};
use r_drive::service::{serve_tcp_v1, serve_tcp_v2};
use r_drive::{MASTER_KEY, generate_master_key};

#[tokio::main(flavor = "multi_thread", worker_threads = 24)]
async fn main() -> Result<()> {
    let args = ServerArgs::parse();

    match args.command {
        Some(ServerCommands::Serve { port, protocol }) => match protocol.as_str() {
            "v1" => {
                MASTER_KEY.get_or_init(generate_master_key);
                serve_tcp_v1(Some(port)).await?;
            }
            "v2" => {
                MASTER_KEY.get_or_init(generate_master_key);
                serve_tcp_v2(Some(port)).await?;
            }
            _ => {
                println!("Unknown protocol: {}", protocol);
                std::process::exit(1);
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
