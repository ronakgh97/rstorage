use crate::get_storage_path_blocking;
use crate::protocol_v1::start_tcp_server;
use crate::protocol_v2::start_tcp_server as start_tcp_server_v2;
use anyhow::Result;
use std::fs;

pub async fn serve_tcp_v1(port: Option<u16>) -> Result<()> {
    dotenv::dotenv().ok();

    let port = port.unwrap_or_else(|| {
        if let Ok(env_port) = std::env::var("R_STORAGE_PORT") {
            env_port.parse::<u16>().unwrap_or(3000)
        } else {
            3000
        }
    });

    let storage_path = get_storage_path_blocking()?;
    if !storage_path.exists() {
        fs::create_dir_all(&storage_path)?;
    }

    start_tcp_server(port)?;

    Ok(())
}

pub async fn serve_tcp_v2(port: Option<u16>) -> Result<()> {
    dotenv::dotenv().ok();

    let port = port.unwrap_or_else(|| {
        if let Ok(env_port) = std::env::var("R_STORAGE_PORT") {
            env_port.parse::<u16>().unwrap_or(3000)
        } else {
            3000
        }
    });

    let storage_path = get_storage_path_blocking()?;
    if !storage_path.exists() {
        fs::create_dir_all(&storage_path)?;
    }

    start_tcp_server_v2(port)?;

    Ok(())
}
