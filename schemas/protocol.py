from pydantic import BaseModel


class AdditionalCommandsSchema(BaseModel):
    execution_command: str
    language: str
    ignore_laws: bool
    is_assigned_woker: bool
    woker_id: str


class BaseProtocolSchema(BaseModel):
    """
    This is a BaseProtocolSchema for every protocols,
    """

    name: str
    description: str
    path: str
    commands: list[AdditionalCommandsSchema]
    additionals: dict
