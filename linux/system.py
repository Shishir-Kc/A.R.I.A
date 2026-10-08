"""This file will contain all the code to run / manuplate Linux to using it wisely is considered
note for file size operation everything is in GB
"""

import shutil
import subprocess
from pathlib import Path

import psutil

from logger_config import set_up_logger

# i have made this changes using mobile
path = str(Path.home() / "test/test.log")

logging = set_up_logger(name="Linux.system", logpath=path)


class Linux:
    def __init__(self) -> None:
        self.Home = Path.home()
        self.application_dir = "/usr/share/applications/"
        self.cache_dir = self.Home / ".cache"

    def _get_storage(self):
        """Get`s Os Storage Info"""
        total, used, free = shutil.disk_usage("/")
        return {"total": (total // 2**30), "free": (free // 2**30), "used": (used // 2**30)}

    def _get_system_ram(self):
        """Gets Os Ram info"""
        logging.info("Getting Os ram info")
        system_ram = psutil.virtual_memory()
        return {
            "total": system_ram.total / 1024**3,
            "free": system_ram.free / 1024**3,
            "used": system_ram.used / 1024**3,
            "swap": system_ram.available / 1024**3,
        }

    def _get_cache_storage(self):
        """Gets storage occupied by Cache"""
        logging.info("Getting storage occupied by cache")
        used = subprocess.run(["du", "-sh", self.cache_dir], capture_output=True, text=True)
        return {
            "used": used.stdout.split("\t")[0],
        }

    def _get_cahe_storage_usage(self, rangeof: int = 25):
        """Usages bash comannd to get .config files/dir storage occupied by application
        Note the returned value can either be in MB or GB you need to calculate it
        it takes argument reangeof:int by default it is 25
        """
        logging.info(f"Getting application with cache usage of range {rangeof}")
        used = subprocess.run(
            f"du -sh {self.cache_dir}/* | sort -rh | head -{rangeof}",
            shell=True,
            capture_output=True,
            text=True,
        )
        logging.info("Splitting recived info")
        data = used.stdout.split()
        length = len(used.stdout.split())
        logging.info("Arranging recived data")
        storage = [data[i] for i in range(0, length) if i % 2 == 0]
        application = [data[i] for i in range(0, length) if i % 2 != 0]
        usage = {application[i]: storage[i] for i, _ in enumerate(application)}
        return usage

    def get_apps(self):
        """Gets all the insatlled apps from the Os"""
        logging.info("Geting applications from desktop")
        yield from Path(self.application_dir).glob(pattern="*.desktop")

    def get_cache(self):
        """Gets All the Cache that are  piled up and hugs storage"""
        yield from Path(self.cache_dir).glob("*")

    def delete_cache(self):
        """This method will delete the cache that has been piling up !"""
        logging.info("Getting Cache")
        cahche = self._get_cahe_storage_usage()
        try:
            for _, (application, usage) in enumerate(cahche.items()):
                logging.info(f"Removing Cache of {application} | cache hold {usage}")
                subprocess.run(f"sudo -S rm -rf {application}", shell=True)
            return True
        except Exception as e:
            logging.debug(e)
            return False

    def show_cache_info(self):
        """This method will show the info regarding the cache usage"""
        application_cache = self._get_cahe_storage_usage()
        total_cache = self._get_cache_storage()
        return {"application_cache": application_cache, "total_cache": total_cache}

    def show_ram_info(self):
        """This method will show ram info it returns dict"""
        info = self._get_system_ram()
        return info


def test_run():
    linu = Linux()
