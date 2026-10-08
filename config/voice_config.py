"""
This file will contain config for voice model (Local)
Local model : KittenTTs
models:
1) kitten-tts-mini - 80M
2) kitten-tts-micro - 40M
3) kitten-tts-nano - 15M
4) kitten-tts-nano (int8) - 15M

Models will be selected based on Device spec

lcoal voice model depende on Internet for some reason

"""

import json
import os
from pathlib import Path

import requests
from kittentts import KittenTTS

from config.path_config import read_json
from errors.errors import ConfigFileMissing
from linux.system import Linux
from logger_config import set_up_logger

logger = set_up_logger(name="Config.voice_config")

HOME = Path.home()
CONFIGPATH = Path.home() / ".config/A.R.I.A/Config/Model"
os.makedirs(CONFIGPATH, exist_ok=True)


class VoiceConfig:
    def __init__(self):
        self.config_download_url = "https://raw.githubusercontent.com/Shishir-Kc/Elysium_additionals/refs/heads/main/Configs/Voice/voice_config.json"
        self.voice_config_path = f"{CONFIGPATH}/{Path(self.config_download_url).name}"

    def _get_model_path(self):
        """This is an internal method which will return model path where it is installed"""
        try:
            logger.info("Reading voice model path ")
            with open(self.voice_config_path) as file:
                config = json.load(file)
            model_path = config.get("path", {})
            if not model_path:
                logger.debug("Path for voice model is missing ! ")
                return
            logger.info(f"Got voice model path {HOME / model_path}")
            return HOME / model_path
        except OSError as e:
            logger.error(e)
            raise ConfigFileMissing("missing file 'Config/Model/voice_config.json' ")

    def download(self) -> bool:
        """

        This method will download voice model configs from the pre-determined config path
        the config file will be stored in .config/A.R.I.A/Config/Model

        """
        try:
            logger.info(f"Sending get request to {self.config_download_url}")
            response = requests.get(self.config_download_url)
            with open(self.voice_config_path, "w") as file:
                """ This small pice of code will open/create the Config path and add the config file name as provided in the download url  """
                json.dump(response.json(), file, indent=2)
            logger.info("Downloaded voice config")
            return True
        except OSError as e:
            logger.error(e)
            return False

    def download_model(self):
        """
        This method will download the appropriate model according to the SYS resources
        first it will create a linux object and call certain method to get to know the sys ram ,
        and after that it will get the voice model config and checks the ram requirement ,
        and it will logically choos e the best model according to the system .

        """
        model_to_be_downloaded = ""
        logger.info("Creating a Linux object ")
        linux = Linux()
        logger.info("Getting ram info ")
        ram_info = linux.show_ram_info()
        total_ram = int(ram_info.get("total", ""))
        logger.info(f"Got {total_ram}GB")
        logger.info("Getting model")
        path = self.voice_config_path
        data = read_json(path=path)
        if data.get("e"):
            raise FileNotFoundError("the file doesnot exists")
        logger.info("Got voice config")
        models: dict = data.get("models", "")
        for _, (model, requirement) in enumerate(models.items()):
            required_ram = requirement.get("required_ram", "")
            if int(required_ram.replace("GB", "")) <= total_ram:
                model_to_be_downloaded = model
        logger.info(f"Downloading {model_to_be_downloaded}")
        try:
            logger.info("Downloading voice model ")
            kittentts = KittenTTS(
                model_name=model_to_be_downloaded,
                cache_dir=self._get_model_path(),
            )
            logger.info("Model downloaded sucessfully")
        except requests.exceptions as e:
            logger.error(e)
