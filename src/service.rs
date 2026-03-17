use crate::protocol_v1::start_tcp_server;
use crate::protocol_v2::start_tcp_server as start_tcp_server_v2;
use crate::{MASTER_KEY, get_storage_path};
use anyhow::Result;
use std::path::PathBuf;

pub async fn serve_tcp_v1(port: Option<u16>) -> Result<()> {
    dotenv::dotenv().ok();

    let master_key = std::env::var("MASTER_KEY").unwrap_or_else(|_| crate::generate_master_key());

    MASTER_KEY.get_or_init(move || master_key);

    let port = port.unwrap_or_else(|| {
        if let Ok(env_port) = std::env::var("PORT") {
            env_port.parse::<u16>().unwrap_or(3000)
        } else {
            3000
        }
    });

    let storage_path: PathBuf = get_storage_path().await?;
    tokio::fs::create_dir_all(&storage_path).await?;

    start_tcp_server(port).await?;

    Ok(())
}

pub async fn serve_tcp_v2(port: Option<u16>) -> Result<()> {
    dotenv::dotenv().ok();

    let master_key = std::env::var("MASTER_KEY").unwrap_or_else(|_| crate::generate_master_key());

    MASTER_KEY.get_or_init(move || master_key);

    let port = port.unwrap_or_else(|| {
        if let Ok(env_port) = std::env::var("PORT") {
            env_port.parse::<u16>().unwrap_or(3000)
        } else {
            3000
        }
    });

    let storage_path = get_storage_path().await?;
    tokio::fs::create_dir_all(&storage_path).await?;

    start_tcp_server_v2(port).await?;

    Ok(())
}
