"""
generate_key() -> will generate_key and returns it in bytes !
encrypt() -> will encrypt key , arguments [item,key]
dencrypt() -> will dencrypt key , arguments [item,key]

"""

import json
import logging
import os
from datetime import datetime

from cryptography.fernet import Fernet

from config.path_config import ARIA_PATH
from errors.errors import KeysNotFound

ENCRYPTION_KEYS_PATH = f"{ARIA_PATH}/Config/Security/encryption"
ENCRYPTION_KEYS_LOG_PATH = f"{ARIA_PATH}/Logs/Security/encryption"
KEYS_PATH = f"{ENCRYPTION_KEYS_PATH}/keys.json"
paths = [ENCRYPTION_KEYS_PATH, ENCRYPTION_KEYS_LOG_PATH]
for path in paths:
    if not os.path.exists(path):
        os.makedirs(path, exist_ok=True)
logger = logging.getLogger("cryptography")
logging.basicConfig(
    level=logging.DEBUG,
    format="%(asctime)s | %(name)s | %(levelname)s | %(message)s",
    handlers=[logging.FileHandler(f"{ENCRYPTION_KEYS_LOG_PATH}/cryptography.log")],
)


def generate_key(module: str, provider_name: str = "", model_name: str = ""):
    logging.info(f"creating key for  {module}")
    try:
        key = Fernet.generate_key()
    except ValueError as e:
        logger.error(f"Failed to generate key {e}")
    new_entry = {
        "module": str(module),
        "key": key.decode("utf-8"),
        "saved_at": str(datetime.now()),
        "provider_name": provider_name,
        "model_name": model_name,
    }

    keys_file = KEYS_PATH
    if os.path.exists(keys_file):
        with open(keys_file) as f:
            data = json.load(f)
        for _index, i in enumerate(data, start=0):
            if i["model_name"] == model_name and i["provider_name"] == provider_name:
                logger.warning("module already exists updating old key")
                data[_index]["key"] = key.decode("utf-8")
                data[_index]["saved_at"] = str(datetime.now())
                with open(keys_file, "w") as file:
                    json.dump(data, file, indent=2)
                logger.info("updated old key")
                return key
    else:
        data = []

    data.append(new_entry)
    try:
        with open(keys_file, "w") as f:
            json.dump(data, f, indent=2)
        logger.info("Key saved sucessfully")
        return key
    except Exception as e:
        logger.error(f"failed to save key {e}")


def encrypt(item, key):
    logging.info("encrypting key")
    item = item.encode("utf-8")
    key = Fernet(key)
    encrypted_data = key.encrypt(item)
    return encrypted_data.decode("utf-8")


def decrypt(item, key):
    logger.info("dencrypting key")
    key = Fernet(key)
    return key.decrypt(item).decode()


def getkey(provider_name, model_name):
    with open(KEYS_PATH) as f:
        keys = json.load(f)
    for key in keys:
        if key.get("model_name") == model_name and key.get("provider_name") == provider_name:
            key_info = key.get("key", " ")
            if not key_info:
                raise KeysNotFound
            return key_info
    raise KeysNotFound
