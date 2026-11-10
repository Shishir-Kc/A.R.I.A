//! The `aria` command line.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::Value;

use aria::additionals::{Additionals, DownloadStatus};
use aria::cli_config::CliConfig;
use aria::error::{AriaError, Result};
use aria::linux::{Linux, human_size};
use aria::model_config::ModelConfig;
use aria::notifier::{Notification, push_notification};
use aria::paths::{aria_path, prompt, show_aria_paths};
use aria::protocol::BaseProtocol;
use aria::updater::Updater;
use aria::voice::VoiceConfig;
use aria::{logger, repl, server};

#[derive(Parser)]
#[command(name = "aria", version, about = "A.R.I.A - AI-powered home server")]
struct Cli {
    /// Also print log lines to stderr
    #[arg(short, long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Gets the current version of A.R.I.A
    Version,
    /// Gets the current status of A.R.I.A
    Status,
    /// Gets the last development update
    Dev,
    /// Gets the current version name of A.R.I.A
    VersionName,
    /// Checks if the current A.R.I.A is stable or not
    IsStable,
    /// Gets overall info of A.R.I.A
    Info,
    /// Checks if a new version is available
    CheckVersion,
    /// Updates A.R.I.A to the latest release
    Update,
    /// Shows the system RAM and swap usage
    RamInfo,
    /// Shows storage of the root filesystem
    StorageInfo,
    /// Shows your current cache usage
    CacheInfo,
    /// Removes the biggest entries of ~/.cache
    RmCache {
        /// Do not ask for confirmation
        #[arg(short, long)]
        yes: bool,
    },
    /// Lists installed desktop applications
    Apps,
    /// Shows the paths A.R.I.A uses
    Paths,
    /// Sends a desktop notification
    Notify {
        title: String,
        body: String,
        /// How long the notification stays, in milliseconds
        #[arg(long, default_value_t = 10_000)]
        timeout: u32,
    },
    /// Runs the A.R.I.A HTTP server
    Serve {
        #[arg(long, default_value = "127.0.0.1")]
        host: IpAddr,
        #[arg(long, default_value_t = 8000)]
        port: u16,
    },
    /// Chats with an LLM
    Chat,
    /// Starts the interactive shell
    Shell,
    /// Manages the model config and API keys
    #[command(subcommand)]
    Model(ModelCommand),
    /// Manages the CLI's own config
    #[command(subcommand)]
    CliConfig(CliConfigCommand),
    /// Manages additionals (plug-and-play skills and tools)
    #[command(subcommand)]
    Additionals(AdditionalsCommand),
    /// Manages the local voice model config
    #[command(subcommand)]
    Voice(VoiceCommand),
    /// Creates and inspects protocols
    #[command(subcommand)]
    Protocol(ProtocolCommand),
}

#[derive(Subcommand)]
enum ModelCommand {
    /// Downloads the model config (from --url, or the default one)
    DownloadConfig {
        #[arg(long)]
        url: Option<String>,
    },
    /// Downloads the default model config if there is none yet
    Make,
    /// Stores an API key for a model (asks for anything not given)
    InsertApi {
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        api_key: Option<String>,
    },
    /// Lists the providers in the model config
    Providers,
}

#[derive(Subcommand)]
enum CliConfigCommand {
    /// Creates the default config if there is none yet
    Make,
    /// Overwrites the config with the default one
    Override {
        /// Do not ask for confirmation
        #[arg(short, long)]
        yes: bool,
    },
    /// Fills in the model section interactively
    Add,
    /// Prints the config
    Show,
}

#[derive(Subcommand)]
enum AdditionalsCommand {
    /// Lists the available additionals
    List,
    /// Checks installed additionals for updates
    CheckUpdate,
    /// Downloads an additional and its dependencies
    Download { name: String },
    /// Updates every installed additional
    Update,
}

#[derive(Subcommand)]
enum VoiceCommand {
    /// Downloads the voice config
    DownloadConfig,
    /// Shows which voice model fits this machine
    SelectModel,
}

#[derive(Subcommand)]
enum ProtocolCommand {
    /// Writes a sample protocol to PATH
    Create { path: PathBuf },
    /// Validates and prints the protocol at PATH
    Show { path: PathBuf },
}

/// A field of the `aria` section of the installed (or bundled) config.
fn aria_field(key: &str) -> Result<String> {
    let config = Updater::default().read_local_config()?;
    Ok(match &config["aria"][key] {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    })
}

fn confirm(question: &str) -> Result<bool> {
    Ok(matches!(prompt(&format!("{question} [y/N] "))?.to_lowercase().as_str(), "y" | "yes"))
}

fn or_prompt(value: Option<String>, label: &str) -> Result<String> {
    match value {
        Some(v) => Ok(v),
        None => prompt(label),
    }
}

fn pretty(value: &impl serde::Serialize) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)?)
}

fn run(command: Command) -> Result<()> {
    let linux = Linux::default();
    match command {
        Command::Version => println!("{}", aria_field("version")?),
        Command::Status => println!("{}", aria_field("status")?),
        Command::Dev => println!("{}", aria_field("last_development_changes")?),
        Command::VersionName => println!("{}", aria_field("version_name")?),
        Command::IsStable => println!("{}", aria_field("stable")?),
        Command::Info => println!(
            "Version : {}\nVersion Name : {}\nStable : {}\nLast Development Changes : {}",
            aria_field("version")?,
            aria_field("version_name")?,
            aria_field("stable")?,
            aria_field("last_development_changes")?,
        ),
        Command::CheckVersion => match Updater::default().check_update()? {
            Some(update) => println!("Update available:\n{}", pretty(&update)?),
            None => println!("A.R.I.A is up to date"),
        },
        Command::Update => match Updater::default().update_aria()? {
            Some(update) => println!("Updated to {}", update.version),
            None => println!("A.R.I.A is up to date"),
        },
        Command::RamInfo => {
            let ram = linux.show_ram_info()?;
            println!(
                "total: {}GB\nused: {}GB\nfree: {}GB\navailable: {}GB\nswap: {}GB used of {}GB",
                ram.total as u64,
                ram.used as u64,
                ram.free as u64,
                ram.available as u64,
                ram.swap_used as u64,
                ram.swap_total as u64,
            );
        }
        Command::StorageInfo => {
            let storage = linux
                .storage()
                .ok_or_else(|| AriaError::Other("could not read the root filesystem".into()))?;
            println!("total: {}GB\nused: {}GB\nfree: {}GB", storage.total, storage.used, storage.free);
        }
        Command::CacheInfo => {
            let info = linux.show_cache_info();
            for entry in &info.application_cache {
                println!("{:>6}  {}", human_size(entry.bytes), entry.path.display());
            }
            println!("\nTotal: {}", human_size(info.total_bytes));
        }
        Command::RmCache { yes } => {
            let entries = linux.cache_storage_usage(25);
            if entries.is_empty() {
                println!("Nothing to remove");
                return Ok(());
            }
            let bytes: u64 = entries.iter().map(|e| e.bytes).sum();
            let question = format!(
                "Delete the {} biggest entries of {} ({})?",
                entries.len(),
                linux.cache_dir.display(),
                human_size(bytes)
            );
            if !yes && !confirm(&question)? {
                println!("Cancelled");
                return Ok(());
            }
            let removed = linux.delete_cache()?;
            let freed: u64 = removed.iter().map(|e| e.bytes).sum();
            println!("Removed {} entries, freed {}", removed.len(), human_size(freed));
            if removed.len() < entries.len() {
                return Err(AriaError::Other(format!(
                    "{} entries could not be removed (run with -v for details)",
                    entries.len() - removed.len()
                )));
            }
        }
        Command::Apps => {
            for app in linux.get_apps() {
                if let Some(name) = app.file_stem() {
                    println!("{}", name.to_string_lossy());
                }
            }
        }
        Command::Paths => {
            println!("root: {}\n{}", aria_path().display(), pretty(show_aria_paths())?);
        }
        Command::Notify { title, body, timeout } => {
            if !push_notification(&Notification { title, body }, timeout) {
                return Err(AriaError::Other(
                    "could not send the notification (is notify-send / libnotify installed?)".into(),
                ));
            }
        }
        Command::Serve { host, port } => server::serve(SocketAddr::new(host, port))?,
        Command::Chat => repl::chat()?,
        Command::Shell => repl::shell()?,
        Command::Model(command) => run_model(command)?,
        Command::CliConfig(command) => run_cli_config(command)?,
        Command::Additionals(command) => run_additionals(command)?,
        Command::Voice(command) => {
            let voice = VoiceConfig::default();
            match command {
                VoiceCommand::DownloadConfig => {
                    voice.download()?;
                    println!("voice config saved to {}", voice.voice_config_path.display());
                }
                VoiceCommand::SelectModel => match voice.select_model()? {
                    Some(model) => println!("{model}"),
                    None => println!("no voice model fits this machine's RAM"),
                },
            }
        }
        Command::Protocol(command) => match command {
            ProtocolCommand::Create { path } => {
                BaseProtocol::sample().save(&path)?;
                println!("sample protocol written to {}", path.display());
            }
            ProtocolCommand::Show { path } => println!("{}", pretty(&BaseProtocol::load(&path)?)?),
        },
    }
    Ok(())
}

fn run_model(command: ModelCommand) -> Result<()> {
    let config = ModelConfig::default();
    match command {
        ModelCommand::DownloadConfig { url } => {
            config.download_config(url.as_deref())?;
            println!("model config saved to {}", config.config_path().display());
        }
        ModelCommand::Make => {
            if config.check_model_config_path().is_ok() {
                println!("model config already exists at {}", config.config_path().display());
            } else {
                config.download_config(None)?;
                println!("model config saved to {}", config.config_path().display());
            }
        }
        ModelCommand::InsertApi { provider, model, api_key } => {
            let provider = or_prompt(provider, "provider_name: ")?;
            let model = or_prompt(model, "model_name: ")?;
            let api_key = or_prompt(api_key, "api_key: ")?;
            config.insert_api_key(&provider, &model, &api_key)?;
            println!("API key saved for {provider}/{model}");
        }
        ModelCommand::Providers => {
            config.check_model_config_path()?;
            for (i, provider) in config.available_providers().iter().enumerate() {
                println!("{} {provider}", i + 1);
            }
        }
    }
    Ok(())
}

fn run_cli_config(command: CliConfigCommand) -> Result<()> {
    let config = CliConfig::default();
    let path = config.base_config_path.display().to_string();
    match command {
        CliConfigCommand::Make => {
            if config.make_config(false)? {
                println!("default config created at {path}");
            } else {
                println!("config already exists at {path}");
            }
        }
        CliConfigCommand::Override { yes } => {
            if yes || confirm(&format!("Overwrite {path} with the default config?"))? {
                config.make_config(true)?;
                println!("default config written to {path}");
            } else {
                println!("Cancelled");
            }
        }
        CliConfigCommand::Add => {
            config.input_config()?;
            println!("config updated");
        }
        CliConfigCommand::Show => println!("{}", pretty(&config.load()?)?),
    }
    Ok(())
}

fn run_additionals(command: AdditionalsCommand) -> Result<()> {
    let additionals = Additionals::new()?;
    match command {
        AdditionalsCommand::List => {
            let installed = additionals.read_downloaded();
            for (name, info) in additionals.additionals()? {
                let version = match &info["version"] {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                let mark = if installed.contains(&name) { " (installed)" } else { "" };
                println!("{name} {version}{mark}");
            }
        }
        AdditionalsCommand::CheckUpdate => {
            let updates = additionals.check_update()?;
            if updates.is_empty() {
                println!("Additionals are up to date");
            }
            for (name, info) in updates {
                println!("update available: {name} -> {}", info["version"]);
            }
        }
        AdditionalsCommand::Download { name } => match additionals.download(&name, false)? {
            DownloadStatus::Downloaded(path) => println!("{name} downloaded to {}", path.display()),
            DownloadStatus::AlreadyDownloaded => println!("{name} is already downloaded"),
        },
        AdditionalsCommand::Update => {
            let updated = additionals.update()?;
            if updated.is_empty() {
                println!("Additionals are up to date");
            } else {
                println!("updated: {}", updated.join(", "));
            }
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    logger::set_verbose(cli.verbose);
    match run(cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
