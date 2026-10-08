import random

from config.model_config import Model_Config as EMC
from errors.errors import ProviderNotFound


class Load_Agent:
    def __init__(self):
        self.ModelConfig = EMC()
        self.model_config = self.ModelConfig.load_config()
        self.ignoreList = ["priority", "installation", "is_installed", "requires", "auth"]

    def model_roulet(self, priority_provider: str = ""):
        providers = self.ModelConfig.available_providers()
        provider = ""
        for _, available_provider in enumerate(providers.values(), start=1):
            if priority_provider == available_provider:
                provider = priority_provider
                break

        if not priority_provider:
            random_provider = random.choice(list(providers))
            provider = providers[random_provider]
        service_provider = provider
        if not provider:
            raise ProviderNotFound("ProviderDoesnotExistOrNotFound!")
        models = self.ModelConfig.load_config()
        provider_models = models.get(provider, {})
        [provider_models.pop(items, None) for items in self.ignoreList]  # removing unwanted stuff !
        model = random.choice(list(provider_models))
        models = models.get(provider, {})
        model_metadata = models.get(model, {})
        if model_metadata.get("model_provider", {}):
            provider = model_metadata.get("model_provider", {})
        return {
            "model_provider": service_provider,
            "provider": provider,
            "model": model_metadata["model_name"],
        }

    def model_key(self, provider: str, model: str):
        return self.ModelConfig.load_model(required_provider=provider, required_model=model)[
            "api_key"
        ]
