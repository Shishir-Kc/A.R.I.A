class ProviderNotGiven(Exception):
    pass


class ModelNameNotGiven(Exception):
    pass


class ApiKeyNotGiven(Exception):
    pass


class ConfigFileMissing(Exception):
    pass


class ProviderNotFound(Exception):
    pass


class DirectoryNotGiven(Exception):
    pass


class KeysNotFound(Exception):
    pass


class AdditionalsNotFound(Exception):
    pass


class AdditionalsNotInstalled(Exception):
    def __init__(self, additional: str = ""):
        super().__init__(f"Additional '{additional}' is not installed")


class MalformedLog(Exception):
    pass
