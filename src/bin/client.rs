#![allow(clippy::wildcard_in_or_patterns)]

use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use r_drive::args::{ClientArgs, ClientCommands};
use r_drive::protocol_v1::{
    download_client as download_file_v1, get_status_v1, upload_client as upload_file_v1,
};
use r_drive::protocol_v2::{
    download_client as download_file_v2, get_status_v2, upload_client as upload_file_v2,
};
use r_drive::{Catalog, get_catalog_path};
use std::io;

#[tokio::main]
async fn main() -> Result<()> {
    let args = ClientArgs::parse();

    match args.command {
        Some(ClientCommands::Upload {
            file,
            port,
            protocol,
            file_key,
        }) => {
            if !file.exists() {
                eprintln!("File not found: {}", file.display());
                std::process::exit(1);
            }

            let file_key = if let Some(key) = file_key {
                key
            } else {
                print!("Enter a lock key: ");
                io::Write::flush(&mut io::stdout())?;
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                if input.trim().is_empty() {
                    eprintln!("File key cannot be empty");
                    std::process::exit(1);
                }
                input.trim().to_string()
            };

            if let Some(protocol) = protocol {
                let key = match protocol.as_str() {
                    "v1" => upload_file_v1(file.clone(), file_key, "127.0.0.1", port).await?,
                    "v2" | _ => upload_file_v2(file.clone(), file_key, "localhost", port).await?,
                };

                let catalog_path = get_catalog_path()?;
                let catalog_dir = catalog_path
                    .parent()
                    .ok_or_else(|| anyhow::anyhow!("Invalid catalog path"))?;
                std::fs::create_dir_all(catalog_dir)?;

                // Read existing or new
                let mut catalog = if catalog_path.exists() {
                    Catalog::read(&catalog_path).await?
                } else {
                    Catalog::default()
                };

                catalog
                    .file_map
                    .insert(key, file.file_name().unwrap().to_string_lossy().to_string());

                catalog.write(&catalog_path).await?;
            } else {
                upload_file_v2(file, file_key, "localhost", port).await?;
            }
        }
        Some(ClientCommands::Download {
            output,
            port,
            protocol,
            file_key,
            file_id,
        }) => {
            if output.as_ref().map(|p| p.exists()).unwrap_or(false) {
                print!("File already exists. Do you want to overwrite it? (y/n): ");
                let y: String = {
                    io::Write::flush(&mut io::stdout())?;
                    let mut input = String::new();
                    io::stdin().read_line(&mut input)?;
                    input.trim().to_lowercase()
                };

                // LOL
                if y != "y" {
                    println!("Aborted");
                    std::process::exit(0);
                }
            }

            let (file_id, file_key) = if let (Some(id), Some(key)) = (file_id, file_key) {
                (id, key)
            } else {
                print!("Enter file ID: ");
                let file_id: String = {
                    io::Write::flush(&mut io::stdout())?;
                    let mut id = String::new();
                    io::stdin().read_line(&mut id)?;
                    if id.trim().is_empty() {
                        eprintln!("File ID cannot be empty");
                        std::process::exit(1);
                    }
                    id.trim().to_string()
                };

                print!("Enter file key: ");
                let file_key: String = {
                    io::Write::flush(&mut io::stdout())?;
                    let mut key = String::new();
                    io::stdin().read_line(&mut key)?;
                    if key.trim().is_empty() {
                        eprintln!("File key cannot be empty");
                        std::process::exit(1);
                    }
                    key.trim().to_string()
                };
                (file_id, file_key)
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
        Some(ClientCommands::Ls { .. }) => {
            list_file_map().await?;
        }
        Some(ClientCommands::Status { protocol }) => {
            let protocol = protocol.unwrap_or_else(|| "v2".to_string());
            match protocol.as_str() {
                "v1" => {
                    let (time, uptime, total_c, bandwidth) =
                        get_status_v1("localhost", 3000).await?;

                    println!("Status timestamp: {}", time);
                    println!("Uptime: {} hrs", uptime);
                    println!("Total Connections: {}", total_c);
                    println!("Bandwidth Used: {} gb", bandwidth);
                }
                "v2" | _ => {
                    let (time, uptime, total_c, bandwidth) =
                        get_status_v2("localhost", 3000).await?;

                    println!("Status timestamp: {}", time);
                    println!("Uptime: {} hrs", uptime);
                    println!("Total Connections: {}", total_c);
                    println!("Bandwidth Used: {} gb", bandwidth);
                }
            }
        }
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

    println!("{}", ascii.cyan());

    println!(
        "🔗 Github: {}",
        "https://github.com/ronakgh97/rstorage".magenta().bold()
    );
}

async fn list_file_map() -> Result<()> {
    let file_map = Catalog::read(&get_catalog_path()?).await?;

    for (id, name) in file_map.file_map {
        println!("  {} - {}", id.yellow(), name.cyan());
    }

    Ok(())
}
