use akssora_core::vm_manager::{VmManager, VmManagerConfig};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

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
            let config = VmManagerConfig {
                rootfs_path: PathBuf::from(
                    "/home/aks/vs_stuff/Development/rust_devs/akssora/images/rootfs.ext4",
                ),
                kernel_path: PathBuf::from(
                    "/home/aks/vs_stuff/Development/rust_devs/akssora/images/vmlinux",
                ),
                vcpu_count: 1,
                mem_size_mib: 256,
                socket_path: PathBuf::from("/tmp/firecracker.socket"),
                vsock_uds_path: PathBuf::from("/tmp/firecracker.vsock"),
            };

            let vm = VmManager::boot(config).await?;
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let output = vm.exec(&cmd).await?;

            print!("{}", String::from_utf8_lossy(&output.stdout));
            eprint!("{}", String::from_utf8_lossy(&output.stderr));

            vm.destroy().await?;

            std::process::exit(output.exit_code);
        }
    }
}
