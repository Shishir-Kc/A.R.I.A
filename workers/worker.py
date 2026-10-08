import os

from config.path_config import ARIA_PATH

"""

i need to implement a woker json from where the code can actually get info on what to do and load it 
i need to make it so that new work can be implemented dynamicay or by an agent with out any hard coding it . 
also make the code clean ! : ) 

the worker will be worked on next month 
"""


worker_path = f"{ARIA_PATH}/Config/worker"
worker_log = f"{ARIA_PATH}/Logs/worker"

paths = [worker_path, worker_log]
for path in paths:
    os.makedirs(path, exist_ok=True)


class worker:
    def __init__(self) -> None:
        pass

    def check_config(self):
        pass

    def add_config(self):
        pass

    def stats(self):
        pass

    def load_config(self):
        pass
