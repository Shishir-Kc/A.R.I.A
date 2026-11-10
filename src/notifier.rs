//! Desktop notifications through `notify-send`.

use std::io::ErrorKind;
use std::process::Command;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use crate::logger::Logger;

static LOG: LazyLock<Logger> = LazyLock::new(|| Logger::new("notifier", None));

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub title: String,
    pub body: String,
}

/// Pushes a desktop notification. Returns `false` when not on Linux, when
/// `notify-send` is not installed (install libnotify), or when it fails.
pub fn push_notification(notification: &Notification, timeout_ms: u32) -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    LOG.info(format!("sending notification '{}'", notification.title));
    let output = Command::new("notify-send")
        .arg("--app-name=A.R.I.A")
        .arg(format!("--expire-time={timeout_ms}"))
        .arg("--")
        .arg(&notification.title)
        .arg(&notification.body)
        .output();
    match output {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            LOG.error(format!("notify-send failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
            false
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {
            LOG.error("notify-send not found, install libnotify");
            false
        }
        Err(e) => {
            LOG.error(format!("could not run notify-send: {e}"));
            false
        }
    }
}
