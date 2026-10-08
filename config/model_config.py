"""
yo yo yo ! sup , so this model_config file is needed and it only contains " free models "
if you want to add a paid model make sure that it is compatable with langchain primarly
the provider should be compatable ! .


"""

import argparse
import json
import logging
import os

import requests

from config.path_config import ARIA_PATH
from errors.errors import (
    ApiKeyNotGiven,
    ConfigFileMissing,
    ModelNameNotGiven,
    ProviderNotFound,
    ProviderNotGiven,
)
from security.encryption.crypto import decrypt, encrypt, generate_key, getkey

BASEDIR = f"{ARIA_PATH}/Config/Model/"
LOGDIR = f"{ARIA_PATH}/Logs/Model"

paths = [BASEDIR, LOGDIR]

for path in paths:
    os.makedirs(path, exist_ok=True)

logger = logging.getLogger("Config.model_config")

logging.basicConfig(
    level=logging.DEBUG,
    format="%(asctime)s | %(name)s | %(levelname)s |  %(message)s",
    handlers=[logging.FileHandler(f"{LOGDIR}/model_config.log")],
)


class Model_Config:
    def __init__(self) -> None:
        self.config_name = "model_config.json"
        self.default_config_url = "https://raw.githubusercontent.com/Shishir-Kc/Elysium_additionals/main/Configs/Elysium_config/model_config.json"

    def cli_flags(self):
        parser = argparse.ArgumentParser()
        parser.add_argument(
            "--download_config", action="store_true", help="downloads pre-defined model config"
        )
        parser.add_argument("--make", action="store_true", help="make pre-defined model config")
        parser.add_argument("--insert_api", action="store_true", help="update api key")
        args = parser.parse_args()

        if args.download_config:
            logger.info("user started config installation")
            try:
                self.download_config(url=input("Enter Custom Config URL or Leave It Empty => "))
            except KeyboardInterrupt:
                logger.error("User canceled the installation ")
            except Exception as e:
                logger.error(f"something is not right ! {e}")

        if args.make:
            try:
                self.check_model_config_path()
            except ConfigFileMissing:
                self.download_config()

        if args.insert_api:
            self.insert_api_key(
                provider_name=input("provider_name: "),
                model_name=input("model_name: "),
                api_key=input("api_key: "),
            )

    def check_model_config_path(self) -> bool:
        if not os.path.exists(f"{BASEDIR}/{self.config_name}"):
            logger.warning(f"{self.config_name} does not exists !")
            raise ConfigFileMissing("Looks like config is missing !")

        logger.info(f"{self.config_name} exists :) ")
        return True

    def load_config(self) -> dict:
        logger.info("trying to load model_config")
        try:
            with open(f"{BASEDIR}/{self.config_name}") as file:
                data = json.load(file)
                logger.info(f"{self.config_name} Loaded ! ")
            return data
        except FileNotFoundError:
            logger.error(f"{self.config_name} not found in {BASEDIR}")
            logger.info(f" it seems like {self.config_name} has not  been created try ( -make )")
            return {}
        except Exception as e:
            logger.error(f"Looks like something went wrong {e}")
            return {}

    def available_providers(self) -> dict:
        logger.info("getting available providers ")
        providers = {}
        for i, provider in enumerate(self.load_config().keys(), start=1):
            providers[i] = provider
        return providers

    def insert_api_key(self, provider_name: str, model_name: str, api_key: str) -> bool:
        key = generate_key(module=__file__, provider_name=provider_name, model_name=model_name)
        self.check_model_config_path()

        """
            well this method does few things ! 
            1) checks if model_name , api_key , and provider_name are given or not . 
            2) get the provider name and specified model  
            3) check if the model is served by that specified provider (from config)
            4) update or add api key and save it 
         """
        model_config = self.load_config()
        logger.info("Appying Custom API key ")
        if not provider_name:
            logger.error("Provider not Givem")
            raise ProviderNotGiven("Excepted provider name")
        if not api_key:
            logger.error("API key not given")
            raise ApiKeyNotGiven("Excepted API Key")
        if not model_name:
            logger.error("Model_name not given")
            raise ModelNameNotGiven("Excepted Model name ")

        provider = model_config.get(provider_name, {})
        if not provider:
            logger.error(f"provider '{provider_name}' does not exists in config")
            raise ProviderNotFound(
                f"looks like {provider_name} provider  does not exists in config"
            )

        model = provider.get(model_name)
        if not model:
            logger.error(f"{model_name} does not exists in {provider_name} config ")
            raise ModuleNotFoundError(f"Looks like {model_name} is not in {provider_name} config !")
        model["api_key"] = encrypt(item=api_key, key=key)
        with open(f"{BASEDIR}/{self.config_name}", "w") as file:
            json.dump(model_config, file, indent=2)
            logger.info("applied custom api key ! ")
        return True

    def download_config(self, url: str | None = None):
        if not url:
            url = self.default_config_url
            logger.info("Using default config url ")
        logger.info(f"Using custom url '{url}'")
        try:
            response = requests.get(url)
            response.raise_for_status()
            with open(f"{BASEDIR}/{self.config_name}", "w") as data:
                json.dump(response.json(), data, indent=2)
                logger.info(f"downloading config from {url}")
                logger.info("Config Downloaded")
            if not self.check_model_config_path():
                logger.error("Unsucessful to install !")
        except requests.RequestException as e:
            logger.error(f"Request failed {e}")
        except (ConnectionError, ConnectionAbortedError, ConnectionRefusedError) as e:
            logger.error(f"Connection Error checks the logs ! {e}")
        except Exception as e:
            logger.error(f"Something went south ! {e}")

    def load_model(self, required_provider: str, required_model: str):
        config = self.load_config()
        provider_config = config.get(required_provider, {})
        if not provider_config:
            raise ProviderNotFound
        model = provider_config.get(required_model)

        model_apikey = model.get("api_key", "")
        key = getkey(provider_name=required_provider, model_name=required_model)
        return {
            "api_key": decrypt(item=model_apikey, key=key),
            "provider": required_provider,
            "model": required_model,
        }


if __name__ == "__main__":
    el = Model_Config()
    el.cli_flags()
    print("Use -h for more info ")
