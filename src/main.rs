mod app;
mod config;
mod error;
mod fs;
mod llm;
mod tui;

use std::path::PathBuf;

use clap::Parser;

use app::App;
use config::Config;

#[derive(Parser)]
#[command(
    name = "foxmayn-cowork",
    about = "Local AI assistant for file management"
)]
struct Cli {
    /// Working directory to open at startup
    #[arg(short, long)]
    dir: Option<PathBuf>,

    /// LLM provider to use
    #[arg(long)]
    provider: Option<config::Provider>,

    /// Model name override
    #[arg(long)]
    model: Option<String>,
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let cli = Cli::parse();
    let config = Config::from_env().with_overrides(cli.provider, cli.model);
    let mut app = App::new(config);

    if let Some(dir) = cli.dir {
        app.set_working_dir(dir);
    }

    if let Err(e) = tui::run(app).await {
        eprintln!("Fatal error: {e}");
        std::process::exit(1);
    }
}
