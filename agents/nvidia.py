from openai import OpenAI

from agents import Load_Agent

agent = Load_Agent()


class NvidiaAgent:
    def __init__(self, agent) -> None:
        roulet = agent.model_roulet(priority_provider="nvidia")
        self.model: str = roulet["model"]
        self.model_provider: str = roulet["model_provider"]
        self.provider: str = roulet["provider"]
        self.api_key: str = agent.model_key(provider=self.model_provider, model=self.model)
        self.baseurl: str = "https://integrate.api.nvidia.com/v1"
        self.client = OpenAI(base_url=self.baseurl, api_key=self.api_key)
        print(f"""
                model => {self.model} \n 
                provider => {self.provider}

              """)

    def chat(self, prompt):
        provider = self.model_provider
        if self.provider:
            provider = self.provider

        model = f"{provider}/{self.model}"
        print(model)
        response = self.client.chat.completions.create(
            model=model, messages=[{"role": "user", "content": prompt}], stream=False
        )
        return response.choices[0].message.content


if __name__ == "__main__":
    agent = Load_Agent()
    agent = NvidiaAgent(agent)
    while True:
        print(agent.chat(prompt=input(":>")))
