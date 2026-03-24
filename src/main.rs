mod cli;
mod output;

use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = cli::Args::parse();
    match args.command {
        cli::Command::Review(_opts) => {
            eprintln!("quorum: review not yet implemented");
            std::process::exit(3);
        }
        cli::Command::Version => {
            println!("quorum {}", env!("CARGO_PKG_VERSION"));
        }
    }
    Ok(())
}
