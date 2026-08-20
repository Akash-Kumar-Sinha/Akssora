use clap::{Parser, Subcommand};
use session::Session;

#[derive(Parser)]
#[command(name = "akssora")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Run { cmd: String },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run { cmd } => {
            let session = Session::new().await?;

            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let output = session.vm_manager.exec(&cmd).await?;

            print!("{}", String::from_utf8_lossy(&output.stdout));
            eprint!("{}", String::from_utf8_lossy(&output.stderr));

            session.close().await?;

            std::process::exit(output.exit_code);
        }
    }
}
