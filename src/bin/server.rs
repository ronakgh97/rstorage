use anyhow::Result;
use clap::Parser;
use r_storage::args::{ServerArgs, ServerCommands};
use r_storage::service::serve;

#[tokio::main]
async fn main() -> Result<()> {
    let args = ServerArgs::parse();

    match args.command {
        Some(ServerCommands::Serve { port }) => {
            serve(port).await?;
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
}
