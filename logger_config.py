import logging
from pathlib import Path

"""
Note need the logger file will have an aditional filed called DEBUG

"""


def set_up_logger(name: str, logpath: str = "", level: str = "DEBUG") -> logging.Logger:
    """
    Returns a logger with its own dedicated file handler.
    name    -> e.g. "Linux.system"
    logpath -> e.g. "~/logs/system.log"
    """
    logger = logging.getLogger(name)
    logger.setLevel(level.upper())

    if not logger.handlers:
        Path(logpath).parent.mkdir(parents=True, exist_ok=True)

        formatter = logging.Formatter("| %(asctime)s | %(levelname)s | %(name)s | %(message)s |")
        if logpath:
            file_handler = logging.FileHandler(logpath)
            file_handler.setFormatter(formatter)
            logger.addHandler(file_handler)

        stream_handler = logging.StreamHandler()
        stream_handler.setFormatter(formatter)
        logger.addHandler(stream_handler)

    logger.propagate = False
    return logger
