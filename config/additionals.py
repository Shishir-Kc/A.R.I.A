"""
This file contains code for doewnloading config / checking config  version / updating config
Mainly the config is for A.R.I.A Additionals (  ' plug and play ' ) architecture .
Furter more EL can automatically / independently call these methods / function as tools ,
so it can decide when to update .
The additionals code is in sepereate repo of the developer called Elysium_additionals
Link : https://github.com/Shishir-Kc/Elysium_additionals , developer might move the code to
personal hosted server for the additionals.

What is additionals ?

well in simple terms Additionals is a way for EL to learn and controll new skills or tools to improve its performance,
Additionals can we configured according to user liking , base version of E.Y.S.I.U.M will not contain all the functionality of Additionals
but it can be downloaded manually by cli or just ask el to download it

"""

import json
import logging
import os
from pathlib import Path

from errors.errors import (
    AdditionalsNotFound,
    AdditionalsNotInstalled,
    ConfigFileMissing,
)

from .path_config import (
    BASEDIR,
    check_for_aria_path,
    download_config,
    show_aria_paths,
)

logger = logging.getLogger("Config.download_additionals")

CONFIGPATH = f"{BASEDIR}/config.json"  # Path for config.json of the shared config package
HOMEDIR = Path.home()

logging.basicConfig(
    level=logging.DEBUG,
    format="| %(levelname)s | %(asctime)s | %(name)s | %(message)s |",
    handlers=[logging.StreamHandler()],
)
logger.info(f"Config path {CONFIGPATH}")


def load_additiosnals_config():
    logger.info("Loading additionals config")
    try:
        with open(CONFIGPATH) as f:
            data = json.load(f)
        return data
    except FileNotFoundError:
        logger.error("Config file doesnot exists! ")
        raise FileNotFoundError


logger.info("Getting ADDITIONALSROOTPATH")
load_config = load_additiosnals_config()
config = load_config.get("aria_additionals_config", {})
if not config:
    raise ConfigFileMissing
all_paths = show_aria_paths(all=True)
additionals_paths = all_paths.get("aria_additionals_paths", {})
additionals_root = additionals_paths.get("Root_path", {})

ADDITIONALSROOTPATH = f"{HOMEDIR}/{additionals_root}"
logger.info("Got ADDITIONALSROOTPATH")


def download_additionals_config(download: bool = True):
    os.makedirs(ADDITIONALSROOTPATH, exist_ok=True)
    download_url = config.get("download_url", {})
    logger.info("Downloading Additionals config")
    try:
        configs = download_config(url=download_url, dir=ADDITIONALSROOTPATH, download=download)
        if not download:
            logger.info("Got Additionals CLOUDCONFIG ")
            return configs
        logger.info("Additionals downloaded sucessfully")
        return {"status": "sucessfull"}
    except Exception as e:
        logger.error(e)
        return {"status": e}


logger.info("Getting Additionals Config ")
ADDITIONALSCONFIG = f"{HOMEDIR}/{all_paths.get('additionals_config', {}).get('Config_path', {})}"
logger.info("Checking Additionals Config File")
if not Path(ADDITIONALSCONFIG).exists():
    logger.warning("Additionals Config File Missing ")
    logger.info("Auto Downloading Config File")
    download_additionals_config()
logger.info(f"Additionals Config: : : {ADDITIONALSCONFIG}")

ADDITIONALSSETTING = f"{ADDITIONALSROOTPATH}/settings.json"


logger.info("Checking additionals path")
if not check_for_aria_path(path=""):
    os.makedirs("asd", exist_ok=True)


class Additionals:
    def __init__(self) -> None:
        pass

    def additionals(self):
        with open(ADDITIONALSCONFIG) as f:
            data = json.load(f)
        return data

    def check_update(self):
        updates = {}
        LocalConfig = self.additionals()
        localconfig = list(LocalConfig)
        CloudConfig = download_additionals_config(download=False)
        logger.info("Checking for updates ")
        for _, additional in enumerate(LocalConfig, start=0):
            if LocalConfig[additional]["version"] < CloudConfig[additional]["version"]:  # type:ignore
                if localconfig[_] not in self._read_downloaded_additionals():
                    continue
                updates[localconfig[_]] = CloudConfig[additional]  # type:ignore
                logger.info(f"update available for {localconfig[_]}")
        if not updates:
            updates = {"status": "Up_to_date"}
            logger.info("Additionals are up to date")
        return updates

    def _write_downloaded_additionals(self, additional: str):
        try:
            try:
                with open(ADDITIONALSSETTING) as f:
                    data: list = json.load(f)
            except (FileNotFoundError, json.JSONDecodeError):
                data = []

            data.append(additional)

            with open(ADDITIONALSSETTING, "w") as f:
                json.dump(data, f, indent=2)

            logger.info(f"Additional '{additional}' saved to settings")
        except Exception as e:
            logger.error(f"Failed to save additional: {e}")

    def _read_downloaded_additionals(self):
        try:
            with open(ADDITIONALSSETTING) as f:
                setting = json.load(f)
            logger.info("Reading setting")
            return list(setting)
        except Exception as e:
            logger.error(e)
            return []

    def _update_config(self, additional):
        cloud_config = download_additionals_config(download=False)
        local_config = self.additionals()
        cloud_additional_info = cloud_config.get(additional, {})  # type:ignore
        local_config[additional] = cloud_additional_info
        with open(ADDITIONALSCONFIG, "w") as file:
            logger.info("Downloading New Config ")
            json.dump(local_config, file, indent=2)

    def download(self, update: bool = False, additional: str = ""):
        if update:
            try:
                if additional not in self._read_downloaded_additionals():
                    raise AdditionalsNotInstalled(additional)
                self._update_config(additional=additional)
            except Exception as e:
                logger.error(e)
                return
        if not additional:
            return {"status": "No_additionals_provided"}
        if additional in self._read_downloaded_additionals():
            return {"status": "Already_downloaded"}
        self._update_config(additional=additional)
        logger.info("Downloaded New Config")
        logger.info(f"Updating {additional}")
        additionals = self.additionals()
        additionalinfo = additionals.get(additional, {})
        if not additionalinfo:
            raise AdditionalsNotFound
        additional_dir = HOMEDIR / additionalinfo.get("path", "")
        os.makedirs(additional_dir, exist_ok=True)
        additional_download_url = additionalinfo.get("download_url", "")
        response = download_config(url=additional_download_url, dir=additional_dir)
        dependencys = additionalinfo.get("dependency", {})
        dependencylist = list(dependencys)
        if dependencys:
            logger.info(f"Found dependencys for {additional}")
            for _, dependency in enumerate(dependencys):
                download_config(dir=additional_dir, url=dependencys.get(dependency)["download_url"])
                logger.info(f"Downloading Dependency: {dependencylist[_]}")
        self._write_downloaded_additionals(additional=additional)  # type:ignore
        return response

    def update(self):
        updates = self.check_update()
        if updates.get("status", "") == "Up_to_date":
            return updates
        for _, additional in enumerate(updates, start=1):
            logger.info(f"Updating:  {_} | {additional}")
            download_update = self.download(additional=additional, update=True)


additionals = Additionals()
print(additionals.download(additional="Sentinel"))
# additionals.update()
# additionals.check_update()
