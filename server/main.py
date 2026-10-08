import asyncio
import logging
import os
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI, WebSocket, WebSocketDisconnect, status
from fastapi.responses import StreamingResponse

testdir = Path.home() / ".config" / "A.R.I.A" / "Logs" / "Server" / "server.log"


logger = logging.getLogger("uvicorn.error")
logger.setLevel(logging.DEBUG)
formater = logging.Formatter("| %(asctime)s | %(levelname)s | %(message)s |")
os.makedirs(testdir.parent, exist_ok=True)
filehandlar = logging.FileHandler(testdir)
filehandlar.setFormatter(formater)
logger.addHandler(filehandlar)


@asynccontextmanager
async def lifespan(server: FastAPI):
    logger.info("Booting A.R.I.A FastAPI Server ")
    yield


server = FastAPI(title="A.R.I.A", lifespan=lifespan)


@server.get("/")
async def base():
    return {"status": status.HTTP_200_OK}


async def logstream():
    with open(testdir) as f:
        f.seek(0, 1)
        while True:
            line = f.readline()
            if not line:
                await asyncio.sleep(0.5)
                continue

            yield f"data:{line}\n\n"


@server.get("/read")
async def readLog():
    return StreamingResponse(logstream(), media_type="text/event-stream")


@server.websocket("/ws")
async def test_socket(websocket: WebSocket):
    await websocket.accept()
    try:
        while True:
            data = await websocket.receive_text()
            await websocket.send_text(f"Echo = {data}")

    except WebSocketDisconnect:
        print("disconnected")
