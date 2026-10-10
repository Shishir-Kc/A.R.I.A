"""
This file is responsible for creating protocols and loading them !

"""

from schemas.protocol import AdditionalCommandsSchema, BaseProtocolSchema


class Protocol:
    def __init__(self) -> None:
        pass

    def load(self): ...

    def create(self) -> bool:
        with open("test.json", "w") as f:
            f.write(
                BaseProtocolSchema(
                    name="test",
                    description="asdasd",
                    path="adasd",
                    commands=[
                        AdditionalCommandsSchema(
                            execution_command="something",
                            language="python",
                            ignore_laws=True,
                            is_assigned_worker=False,
                            worker_id="N/A",
                        )
                    ],
                    additionals={"something": "something"},
                ).model_dump_json(indent=2),
            )
        return True


pp = Protocol()
pp.create()
