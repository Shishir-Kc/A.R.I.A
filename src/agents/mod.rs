//! Agents: picking a provider/model and talking to it.

pub mod nvidia;

use serde_json::{Map, Value};

use crate::error::{AriaError, Result};
use crate::model_config::ModelConfig;

/// Keys inside a provider's config that are settings, not models.
const IGNORE_LIST: [&str; 5] = ["priority", "installation", "is_installed", "requires", "auth"];

#[derive(Debug, Clone, PartialEq)]
pub struct ModelChoice {
    /// The provider serving the model (the top-level key in the model config).
    pub model_provider: String,
    /// The upstream vendor of the model, when it differs from the server.
    pub provider: String,
    pub model: String,
}

/// Picks a model from `config`. With a `priority_provider` that provider must
/// exist; without one a random provider is used. The model is random.
pub fn pick_model(config: &Map<String, Value>, priority_provider: Option<&str>) -> Result<ModelChoice> {
    let service_provider = match priority_provider.filter(|p| !p.is_empty()) {
        Some(p) if config.contains_key(p) => p.to_owned(),
        Some(p) => return Err(AriaError::ProviderNotFound(p.to_owned())),
        None => {
            let providers: Vec<&String> = config.keys().collect();
            if providers.is_empty() {
                return Err(AriaError::ProviderNotFound("the model config has no providers".into()));
            }
            providers[fastrand::usize(..providers.len())].clone()
        }
    };

    let models: Vec<&Value> = config[&service_provider]
        .as_object()
        .map(|m| {
            m.iter()
                .filter(|(name, meta)| !IGNORE_LIST.contains(&name.as_str()) && meta.is_object())
                .map(|(_, meta)| meta)
                .collect()
        })
        .unwrap_or_default();
    if models.is_empty() {
        return Err(AriaError::ModelNotFound(format!("provider '{service_provider}' has no models")));
    }
    let meta = models[fastrand::usize(..models.len())];

    let model = meta["model_name"]
        .as_str()
        .ok_or_else(|| AriaError::ModelNotFound("model entry has no model_name".into()))?
        .to_owned();
    let provider = meta["model_provider"]
        .as_str()
        .filter(|p| !p.is_empty())
        .unwrap_or(&service_provider)
        .to_owned();
    Ok(ModelChoice { model_provider: service_provider, provider, model })
}

#[derive(Default)]
pub struct LoadAgent {
    model_config: ModelConfig,
}

impl LoadAgent {
    pub fn new(model_config: ModelConfig) -> Self {
        Self { model_config }
    }

    pub fn model_roulette(&self, priority_provider: Option<&str>) -> Result<ModelChoice> {
        pick_model(&self.model_config.load_config(), priority_provider)
    }

    /// The decrypted API key for a model.
    pub fn model_key(&self, provider: &str, model: &str) -> Result<String> {
        Ok(self.model_config.load_model(provider, model)?.api_key)
    }
}

/// The planned agent kinds. None of them exist yet.
#[derive(Default)]
pub struct Agents;

impl Agents {
    pub fn deep_agent(&self) -> Result<()> {
        Err(AriaError::NotImplemented("deep agent"))
    }
    pub fn web_agent(&self) -> Result<()> {
        Err(AriaError::NotImplemented("web agent"))
    }
    pub fn worker_agent(&self) -> Result<()> {
        Err(AriaError::NotImplemented("worker agent"))
    }
    pub fn agents_council(&self) -> Result<()> {
        Err(AriaError::NotImplemented("agents council"))
    }
    pub fn agent_loop(&self) -> Result<()> {
        Err(AriaError::NotImplemented("agent loop"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_config::tests::fixture;

    #[test]
    fn priority_provider_is_honoured() {
        let (_dir, mc) = fixture();
        let choice = LoadAgent::new(mc).model_roulette(Some("nvidia")).unwrap();
        assert_eq!(
            choice,
            ModelChoice { model_provider: "nvidia".into(), provider: "meta".into(), model: "llama".into() }
        );
    }

    #[test]
    fn provider_defaults_to_the_server() {
        let (_dir, mc) = fixture();
        let choice = LoadAgent::new(mc).model_roulette(Some("groq")).unwrap();
        assert_eq!(choice.provider, "groq");
        assert_eq!(choice.model, "mixtral");
    }

    #[test]
    fn random_pick_and_unknown_provider() {
        let (_dir, mc) = fixture();
        let agent = LoadAgent::new(mc);
        let choice = agent.model_roulette(None).unwrap();
        assert!(["nvidia", "groq"].contains(&choice.model_provider.as_str()));
        assert!(matches!(agent.model_roulette(Some("nope")), Err(AriaError::ProviderNotFound(_))));
        assert!(matches!(pick_model(&Map::new(), None), Err(AriaError::ProviderNotFound(_))));
    }
}
