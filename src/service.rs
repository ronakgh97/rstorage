use crate::controller::start_server;
use anyhow::Result;
use std::path::PathBuf;

pub async fn serve(port: Option<u16>) -> Result<()> {
    dotenv::dotenv().ok();

    let port = port.unwrap_or_else(|| {
        // Check for R_STORAGE_PORT environment variable
        if let Ok(env_port) = std::env::var("R_STORAGE_PORT") {
            env_port.parse::<u16>().unwrap_or(8080)
        } else {
            8080
        }
    });

    // Create a storage volume before server starts
    if !get_storage_path().await?.exists() {
        tokio::fs::create_dir_all(get_storage_path().await?).await?;
    }

    start_server(port).await?;

    Ok(())
}

#[inline]
pub async fn get_storage_path() -> Result<PathBuf> {
    let home_dir =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Failed to get home directory"))?;
    let storage_path = home_dir.join(".r_storage").join("storage");
    Ok(storage_path)
}
