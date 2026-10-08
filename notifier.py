import platform
import subprocess

from logger_config import set_up_logger

logger = set_up_logger(name="notifier", level="DEBUG")

PLATFORM = platform.uname()


def push_notification(title: str, body: str = "", timeout_ms: int = 10000) -> bool:
    if PLATFORM.system != "Linux":
        return False
    try:
        result = subprocess.run(
            ["notify-send", "--app-name=A.R.I.A", f"--expire-time={timeout_ms}", title, body],
            capture_output=True,
            text=True,
            check=False,
        )
    except FileNotFoundError:
        logger.error("notify-send not found, install libnotify")
        return False
    if result.returncode != 0:
        logger.error("notify-send failed: %s", result.stderr.strip())
        return False
    return True


def main():
    push_notification(title="This is a title", body="This is a body ")


if __name__ == "__main__":
    main()
