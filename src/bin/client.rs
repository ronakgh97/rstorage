#![allow(clippy::wildcard_in_or_patterns)]

use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use r_storage::args::{ClientArgs, ClientCommands};
use r_storage::protocol_v1::{
    download_client as download_file_v1, upload_client as upload_file_v1,
};
use r_storage::protocol_v2::{
    download_client as download_file_v2, upload_client as upload_file_v2,
};
use std::io;

#[tokio::main]
async fn main() -> Result<()> {
    let args = ClientArgs::parse();

    match args.command {
        Some(ClientCommands::Upload {
            file,
            port,
            protocol,
        }) => {
            let port: u16 = port.parse().unwrap_or(3000);
            if let Some(protocol) = protocol {
                match protocol.as_str() {
                    "v1" => {
                        upload_file_v1(file, "127.0.0.1", port).await?;
                    }
                    "v2" | _ => {
                        upload_file_v2(file, "localhost", port).await?;
                    }
                }
            } else {
                upload_file_v2(file, "localhost", port).await?;
            }
        }
        Some(ClientCommands::Download {
            output,
            port,
            protocol,
        }) => {
            let port: u16 = port.parse().unwrap_or(3000);

            print!("Enter file ID: ");
            let file_id: String = {
                io::Write::flush(&mut io::stdout())?;
                let mut id = String::new();
                io::stdin().read_line(&mut id)?;
                id.trim().to_string()
            };

            print!("Enter file key: ");
            let file_key: String = {
                io::Write::flush(&mut io::stdout())?;
                let mut file_key = String::new();
                io::stdin().read_line(&mut file_key)?;
                file_key.trim().to_string()
            };

            if let Some(protocol) = protocol {
                match protocol.as_str() {
                    "v1" => {
                        download_file_v1(file_id, file_key, output, "127.0.0.1", port).await?;
                    }
                    "v2" | _ => {
                        download_file_v2(file_id, file_key, output, "localhost", port).await?;
                    }
                }
            } else {
                download_file_v2(file_id, file_key, output, "localhost", port).await?;
            }
        }
        None => {
            ascii_art();
        }
    }

    Ok(())
}

fn ascii_art() {
    let ascii = r"                                                 
                    ██                                 
 ████▄       ▄█▀▀▀ ▀██▀▀ ▄███▄ ████▄  ▀▀█▄ ▄████ ▄█▀█▄ 
 ██ ▀▀ ▀▀▀▀▀ ▀███▄  ██   ██ ██ ██ ▀▀ ▄█▀██ ██ ██ ██▄█▀ 
 ██          ▄▄▄█▀  ██   ▀███▀ ██    ▀█▄██ ▀████ ▀█▄▄▄ 
                                              ██       
                                            ▀▀▀
    ";

    println!("{}", ascii);

    println!(
        "🔗 Github: {}",
        "https://github.com/ronakgh97/rstorage".magenta().bold()
    );
}
