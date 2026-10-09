from pydantic import BaseModel


class NotificationSchema(BaseModel):
    title: str
    body: str
