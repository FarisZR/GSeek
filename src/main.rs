use anyhow::Result;
use clap::{Parser, Subcommand};
use gseek::{config::Paths, service};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Find your files by what you remember, from GNOME Search"
)]
struct Args {
    #[command(subcommand)]
    command: Option<Action>,
}
#[derive(Subcommand)]
enum Action {
    /// Run background indexing and the D-Bus search provider (no display required).
    Service,
    /// Register this installation with the current user's GNOME session.
    Integrate {
        #[arg(long)]
        launcher: Option<PathBuf>,
    },
    /// Remove owned desktop integration and restore the previous Files provider setting.
    Unintegrate,
    /// Print indexing and model status.
    Status,
    /// Search the library from a terminal.
    Search { query: String },
    /// Verify/download the pinned model (for CI and diagnostics).
    PrepareModel,
    /// Test real local inference without a desktop session.
    Embed {
        text: String,
        #[arg(long)]
        image: Option<PathBuf>,
        #[arg(long)]
        audio: Option<PathBuf>,
    },
}
fn main() {
    if let Err(e) = run() {
        eprintln!("GSeek: {e:#}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    let args = Args::parse();
    let paths = Paths::from_env();
    paths.init()?;
    match args.command {
        Some(Action::Service) => service::run(paths),
        Some(Action::Integrate { launcher }) => {
            service::stop_running();
            let launcher = match launcher {
                Some(path) => path,
                None => service::launcher()?,
            };
            gseek::integration::install(&paths, &launcher)
        }
        Some(Action::Unintegrate) => {
            service::stop_running();
            gseek::integration::uninstall(&paths)
        }
        Some(Action::Status) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&service::read_status()?)?
            );
            Ok(())
        }
        Some(Action::Search { query }) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&service::search(&query, 100)?)?
            );
            Ok(())
        }
        Some(Action::PrepareModel) => gseek::model::ensure(&paths, |_, _, _| Ok(())),
        Some(Action::Embed { text, image, audio }) => {
            let settings = paths.load()?;
            let mut runtime = gseek::inference::Runtime::new(
                &paths,
                &settings,
                image.is_none() && audio.is_none(),
            );
            let input = if let Some(path) = image {
                gseek::extract::Input::Image { path }
            } else if let Some(path) = audio {
                gseek::extract::Input::Audio { path }
            } else {
                gseek::extract::Input::Text { text }
            };
            let vector = runtime.embed(&input)?;
            println!("{}", serde_json::to_string(&vector)?);
            Ok(())
        }
        None => {
            #[cfg(feature = "gui")]
            {
                gseek::ui::run(paths)
            }
            #[cfg(not(feature = "gui"))]
            {
                anyhow::bail!("This build has no GTK interface; build with default features");
            }
        }
    }
}
