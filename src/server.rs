//! The A.R.I.A HTTP server.
//!
//! * `GET /`     -> `{"status": 200}`
//! * `GET /read` -> the server log as a server-sent event stream (follows the file)
//! * `GET /ws`   -> WebSocket that echoes text back as `Echo = <text>`

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::Stream;
use futures_util::stream;
use serde_json::{Value, json};
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::error::Result;
use crate::logger::Logger;
use crate::paths::log_file;

static LOG: LazyLock<Logger> =
    LazyLock::new(|| Logger::new("Server", Some(server_log_path())));

pub fn server_log_path() -> PathBuf {
    log_file("Server/server.log")
}

/// The app, streaming `log_path` on `/read`.
pub fn app(log_path: PathBuf) -> Router {
    Router::new()
        .route("/", get(base))
        .route("/read", get(read_log))
        .route("/ws", get(ws_handler))
        .with_state(log_path)
}

async fn base() -> Json<Value> {
    LOG.info("GET /");
    Json(json!({"status": 200}))
}

/// Emits each complete line of the log file as one event, then waits for more.
fn log_stream(path: PathBuf) -> impl Stream<Item = std::result::Result<Event, Infallible>> {
    stream::unfold((None::<BufReader<File>>, String::new()), move |(mut reader, mut pending)| {
        let path = path.clone();
        async move {
            loop {
                let Some(r) = reader.as_mut() else {
                    match File::open(&path).await {
                        Ok(file) => reader = Some(BufReader::new(file)),
                        Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
                    }
                    continue;
                };
                match r.read_line(&mut pending).await {
                    Ok(n) if n > 0 && pending.ends_with('\n') => {
                        let event = Event::default().data(pending.trim_end());
                        return Some((Ok(event), (reader, String::new())));
                    }
                    // End of file (possibly mid-line): wait for the writer.
                    _ => tokio::time::sleep(Duration::from_millis(500)).await,
                }
            }
        }
    })
}

async fn read_log(State(path): State<PathBuf>) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    LOG.info("GET /read");
    Sse::new(log_stream(path)).keep_alive(KeepAlive::default())
}

async fn ws_handler(upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(echo).into_response()
}

async fn echo(mut socket: WebSocket) {
    LOG.info("websocket connected");
    while let Some(Ok(message)) = socket.recv().await {
        match message {
            Message::Text(text) => {
                if socket.send(Message::Text(format!("Echo = {text}").into())).await.is_err() {
                    break;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    LOG.info("websocket disconnected");
}

/// Runs the server until the process is stopped.
pub fn serve(addr: SocketAddr) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        LOG.info(format!("Booting A.R.I.A server on http://{}", listener.local_addr()?));
        println!("A.R.I.A server listening on http://{}", listener.local_addr()?);
        axum::serve(listener, app(server_log_path())).await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    async fn spawn(log_path: PathBuf) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app(log_path)).await.unwrap() });
        addr
    }

    async fn request(addr: SocketAddr, path: &str) -> TcpStream {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        stream
    }

    #[tokio::test]
    async fn root_reports_status() {
        let dir = tempfile::tempdir().unwrap();
        let addr = spawn(dir.path().join("server.log")).await;
        let mut response = String::new();
        request(addr, "/").await.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.ends_with(r#"{"status":200}"#));
    }

    #[tokio::test]
    async fn log_stream_follows_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.log");
        std::fs::write(&path, "first\nsecond\npart").unwrap();

        let mut events = Box::pin(log_stream(path.clone()));
        let _first = events.next().await.unwrap();
        let _second = events.next().await.unwrap();

        // "part" has no newline yet, so nothing more is emitted until it is finished.
        let pending = tokio::time::timeout(Duration::from_millis(700), events.next()).await;
        assert!(pending.is_err());

        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, b"ial\n"))
            .unwrap();
        let third = tokio::time::timeout(Duration::from_secs(3), events.next()).await;
        assert!(third.is_ok());
    }

    #[tokio::test]
    async fn read_endpoint_streams_sse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.log");
        std::fs::write(&path, "hello log\n").unwrap();
        let addr = spawn(path).await;

        let mut stream = request(addr, "/read").await;
        let mut seen = String::new();
        let mut buf = [0u8; 1024];
        while !seen.contains("data: hello log") {
            let n = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut buf))
                .await
                .expect("timed out waiting for the event")
                .unwrap();
            assert!(n > 0, "connection closed early: {seen}");
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        assert!(seen.contains("text/event-stream"));
    }
}
