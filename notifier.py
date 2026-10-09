import platform
import subprocess

from logger_config import set_up_logger
from schemas.linux import NotificationSchema

logger = set_up_logger(name="notifier", level="DEBUG")

PLATFORM = platform.uname()


def push_notification(notification: NotificationSchema, timeout_ms: int = 10000) -> bool:
    """
    This is a notification function which will push notification using notify-send .

    It usages Schema from schemas/linux.NotificationSchema,
     where it contains
            title : str
            body : str

    Working >>  This fucntion will check if the platform is Linux or not if it's not then it wont work !
    usinf subprocess it calls notify-send to send notification with title and body .
    and also it checks if notify-send is installed or not !

    Returns >> Bool

    """
    if PLATFORM.system != "Linux":
        return False
    try:
        result = subprocess.run(
            [
                "notify-send",
                "--app-name=A.R.I.A",
                f"--expire-time={timeout_ms}",
                notification.title,
                notification.body,
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        logger.info(f"sending notification ' {notification.title} ' ")
    except FileNotFoundError:
        logger.error("notify-send not found, install libnotify")
        return False
    if result.returncode != 0:
        logger.error("notify-send failed: %s", result.stderr.strip())
        return False
    return True


def main():
    push_notification(
        notification=NotificationSchema(title="This is a title", body="This is a body")
    )


if __name__ == "__main__":
    main()
