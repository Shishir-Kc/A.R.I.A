"""
its logs will be under .config/Elysim/logs/cli

where ARIA_PATH -> $HOME/.config/A.R.I.A/

"""

import argparse
import json
import logging
import os

import requests
from pydantic import BaseModel

from cli.internal import ConfigNotFound, InvalidArgsFound
from config.path_config import ARIA_PATH, BASEDIR
from security.encryption.crypto import encrypt, generate_key

LOGDIR = f"{ARIA_PATH}/Logs/cli"
BASEDIR = f"{ARIA_PATH}/Config/cli"
paths = [BASEDIR, LOGDIR]


for path in paths:
    """
     this loop creates path if it doesnot exists of it does then it will not shoe error ! :) 
    """
    os.makedirs(path, exist_ok=True)

logger = logging.getLogger("A.R.I.A.Cli.Config.cli_config")


def logs(debug: bool = False):
    try:
        if not debug:
            logs = str(input("Logs ? y/n => "))
            user_choice = list(logs.lower())
            user_choice = user_choice[0]
            if user_choice == "y":
                debug = True
            elif user_choice == "n":
                debug = False
    except Exception as e:
        raise Exception(e)

    handlers: List[logging.Handler] = [logging.FileHandler(f"{LOGDIR}/config.log")]
    if debug:
        handlers.append(logging.StreamHandler())

    logging.basicConfig(
        level=logging.DEBUG if debug else logging.INFO,
        format="%(asctime)s - %(name)s - %(levelname)s - %(message)s",
        handlers=handlers,
    )


logs()


class Model_Schema(BaseModel):
    provider: str | None
    model_name: str | None
    api_key: str | None
    model_type: str | None


class Config:
    def __init__(self) -> None:
        self.base_config_path = f"{BASEDIR}/config.json"
        self.sad_face = ". . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . :("
        self.happy_face = ". . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . . :)"
        self.default_config_url = "https://raw.githubusercontent.com/Shishir-Kc/Elysium_additionals/main/Configs/Elysium_cli/cli_config.json"
        self.parser = argparse.ArgumentParser()
        self.parser.add_argument("-make", action="store_true", help="Create fresh config ")
        self.parser.add_argument(
            "-over_ride", action="store_true", help="over_ride default config "
        )
        self.parser.add_argument("-add_config", action="store_true", help="add config")
        self.args = self.parser.parse_args()

        if self.args.make:
            # here if there is not premade config then it will make a default config .
            # all the values will be null !
            logger.info("Creating Default config" + self.happy_face)

            self.make_config()

        if self.args.over_ride:
            """
                 so basically it over writes the config.json file with the predefine 
                 self.default_config . in short u run this your config goo boom ! 
            """
            logger.warning(
                "Warning Your Config.json file will be over_rided with default config.json"
            )
            try:
                user_permission = input(" Are You Sure ? Anything/n => ")
                if not user_permission == "n":
                    logger.info("Creating default config.json ")
                    self.make_config(over_ride=True)
                else:
                    logger.info(" Operation (over_ride) Canceled")

            except KeyboardInterrupt as e:
                logger.info("Stopped (over_ride) Process", e)

        if self.args.add_config:
            logger.info("Adding Values " + self.happy_face)
            self.input_config()
        self.check_config()

    def check_config(self) -> None:
        """
        this is function checks of the config is there is not if there is no config
        it shows a custome Exception where it says ConfigNotFound try creating config

        """

        if not os.path.exists(self.base_config_path):
            raise ConfigNotFound(
                "Config not found !  \n Try creating the config ! with ( -make )  "
            )

    def make_config(self, over_ride: bool = False) -> None:
        if not os.path.exists(self.base_config_path) or (over_ride == True):
            try:
                response = requests.get(self.default_config_url)
                with open(self.base_config_path, "w") as data:
                    json.dump(response.json(), data, indent=2)
                print(" Default config.json has been created ")

            except requests.RequestException as e:
                logger.error(f"Request failed {e}")
            except (ConnectionError, ConnectionAbortedError, ConnectionRefusedError) as e:
                logger.error(f"Connection error {e}")
            except Exception as e:
                logger.error(f"Something went south {e}")

    def input_config(self):
        _key = generate_key(module=__file__)
        config: dict[str, str] = {}
        try:
            model = self.load()["model"]

            for _, key in enumerate(model, start=1):
                user_input = str(input(f"Enter {key} => "))
                if key == "api_key":
                    user_input = encrypt(item=user_input, key=_key)
                    print(user_input)
                config[key] = user_input
            self.update_config(**config)
        except KeyboardInterrupt as e:
            logger.info(f"Adding (config key) Interrupted ! {e}" + self.sad_face)
            print(f"Stopped {self.sad_face}")

    def update_config(self, **kwargs):
        try:
            validate = Model_Schema(**kwargs)
        except ValueError as e:
            logger.warning(f"Invalid Args or missing \n Check this {e}")
            raise InvalidArgsFound(f"Invalid args ! {self.sad_face} ")
        config = self.load()
        with open(self.base_config_path, "w") as data:
            config["model"] = validate.model_dump(exclude_none=True)
            try:
                json.dump(config, data, indent=2)
            except Exception as e:
                logger.warning(
                    f"Something Terriable has gone wrong ! check this out ! {e}" + self.sad_face
                )

    def load(self) -> dict:
        try:
            with open(self.base_config_path) as data:
                config = json.load(data)
            return config
        except json.JSONDecodeError as e:
            logger.warning(e)
            print(
                f"Failed to Load content from config.json \n Try overriding the config.json to default {self.sad_face}"
            )
            print(" Command ? \n \n here =>  uv run config.py -make -over_ride \n")
            return {"Error": e}

    def load_model_data(self) -> dict:
        config = self.load()
        model = config.get("model", {})
        model_name = model.get("model_name")
        provider = model.get("provider")
        api_key = model.get("api_key")
        model_type = model.get("model_type")
        if model_type == "Cloud" and api_key == "":
            logger.warning(f" Api key not provided for the Model : {model_name}  {self.sad_face} ")
            try:
                api_key = str(input("API_KEY: "))
                self.update_config(
                    provider=provider, api_key=api_key, model_name=model_name, model_type=model_type
                )
            except KeyboardInterrupt:
                logger.warning(f"API_KEY not saved!  {self.sad_face} ")
        return model


if __name__ == "__main__":
    config = Config()
