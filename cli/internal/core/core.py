from agents.nvidia import Load_Agent, NvidiaAgent
from config.model_config import Model_Config

elconfig = Model_Config()


class Cli:
    def __init__(self):
        pass

    def available_commands(self):
        commands = """
         -chat -> will ne able to chat with an LLM 
         -download_config -> will download default model_config     
        """
        print(commands)

    def logic(self, user_input):
        if user_input == "-chat":
            agent = Load_Agent()
            agent = NvidiaAgent(agent)
            while True:
                prompt = input("prompt:> ")
                if prompt == "e":
                    break
                print(agent.chat(prompt=prompt))
        if user_input == "-download_config":
            elconfig.download_config(url=input("url:> "))
        if user_input == "help":
            self.available_commands()
        if user_input == "-insert_api":
            elconfig.insert_api_key(
                provider_name=input("provider_name:> "),
                model_name=input("model_name:> "),
                api_key=input("api_key:> "),
            )

    def cli(self):
        while True:
            user_input = input(":> ")
            if user_input == "e":
                break
            self.logic(user_input)


if __name__ == "__main__":
    cli = Cli()
    cli.cli()
