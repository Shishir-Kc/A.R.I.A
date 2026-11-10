//! The interactive shell (`aria shell`) and chat loop (`aria chat`).

use crate::agents::LoadAgent;
use crate::agents::nvidia::NvidiaAgent;
use crate::error::Result;
use crate::model_config::ModelConfig;
use crate::paths::prompt;

const COMMANDS: &str = "
  -chat             chat with an LLM
  -download_config  download the model config
  -insert_api       store an API key for a model
  help              show this list
  e                 exit
";

/// Chat with a model until the user enters `e`.
pub fn chat() -> Result<()> {
    let agent = NvidiaAgent::new(&LoadAgent::default())?;
    println!("model => {}\nprovider => {}\n(enter 'e' to leave)", agent.model, agent.provider);
    loop {
        let input = prompt("prompt:> ")?;
        if input == "e" {
            return Ok(());
        }
        if input.is_empty() {
            continue;
        }
        match agent.chat(&input) {
            Ok(answer) => println!("{answer}"),
            Err(e) => eprintln!("error: {e}"),
        }
    }
}

fn run_command(input: &str) -> Result<()> {
    let model_config = ModelConfig::default();
    match input {
        "-chat" => chat(),
        "-download_config" => {
            let url = prompt("url (empty for default):> ")?;
            model_config.download_config(Some(&url))?;
            println!("model config saved to {}", model_config.config_path().display());
            Ok(())
        }
        "-insert_api" => {
            let provider = prompt("provider_name:> ")?;
            let model = prompt("model_name:> ")?;
            let api_key = prompt("api_key:> ")?;
            model_config.insert_api_key(&provider, &model, &api_key)?;
            println!("API key saved");
            Ok(())
        }
        "help" => {
            println!("{COMMANDS}");
            Ok(())
        }
        "" => Ok(()),
        other => {
            println!("unknown command '{other}', try 'help'");
            Ok(())
        }
    }
}

/// The shell loop. Errors from a command are printed and the shell continues;
/// it ends on `e` or when input is closed.
pub fn shell() -> Result<()> {
    loop {
        let Ok(input) = prompt(":> ") else { return Ok(()) };
        if input == "e" {
            return Ok(());
        }
        if let Err(e) = run_command(&input) {
            eprintln!("error: {e}");
        }
    }
}
