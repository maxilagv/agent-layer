//! A small Chrome DevTools Protocol client over one WebSocket.
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<std::result::Result<Value, String>>>>>;

pub struct Cdp {
    out: mpsc::Sender<String>,
    pending: Pending,
    events: broadcast::Sender<Value>,
    next: AtomicU64,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Cdp {
    #[cfg(not(unix))]
    pub async fn connect(ws_url: &str) -> Result<Self> {
        let (socket, _) = tokio::time::timeout(
            Duration::from_secs(10),
            tokio_tungstenite::connect_async(ws_url),
        )
        .await
        .context("DevTools connection timed out")?
        .context("DevTools connection failed")?;
        Self::from_socket(socket)
    }

    #[cfg(unix)]
    pub async fn connect_unix(ws_url: &str, path: &std::path::Path) -> Result<Self> {
        #[cfg(target_os = "linux")]
        let transport = {
            use std::os::{
                fd::{AsRawFd, FromRawFd, OwnedFd},
                unix::{ffi::OsStrExt, fs::FileTypeExt},
            };
            let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
            // Open without following symlinks, then connect through the anchored
            // inode. This removes the check/use race on Chromium's writable profile.
            let fd = unsafe {
                libc::open(
                    path.as_ptr(),
                    libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            let file = std::fs::File::from(fd);
            anyhow::ensure!(
                file.metadata()?.file_type().is_socket(),
                "browser control endpoint must be a UNIX socket, never a symlink"
            );
            tokio::net::UnixStream::connect(format!("/proc/self/fd/{}", file.as_raw_fd()))
                .await
                .context("connecting confined browser control socket")?
        };
        #[cfg(not(target_os = "linux"))]
        let transport = tokio::net::UnixStream::connect(path).await?;
        let (socket, _) = tokio::time::timeout(
            Duration::from_secs(10),
            tokio_tungstenite::client_async(ws_url, transport),
        )
        .await
        .context("confined DevTools connection timed out")??;
        Self::from_socket(socket)
    }

    fn from_socket<S>(socket: tokio_tungstenite::WebSocketStream<S>) -> Result<Self>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (mut sink, mut stream) = socket.split();
        let pending: Pending = Arc::default();
        let (events, _) = broadcast::channel(2048);
        let (out, mut outbox) = mpsc::channel::<String>(256);
        let writer = tokio::spawn(async move {
            while let Some(text) = outbox.recv().await {
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
        });
        let (reader_pending, reader_events) = (pending.clone(), events.clone());
        let reader = tokio::spawn(async move {
            while let Some(Ok(message)) = stream.next().await {
                let Message::Text(text) = message else {
                    continue;
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if let Some(id) = value["id"].as_u64() {
                    let waiter = reader_pending.lock().ok().and_then(|mut p| p.remove(&id));
                    if let Some(waiter) = waiter {
                        let _ = waiter.send(match value.get("error") {
                            Some(error) => Err(error["message"]
                                .as_str()
                                .unwrap_or("protocol error")
                                .to_string()),
                            None => Ok(value["result"].clone()),
                        });
                    }
                } else if value["method"].is_string() {
                    let _ = reader_events.send(value);
                }
            }
            // Connection gone: fail everything still waiting.
            if let Ok(mut p) = reader_pending.lock() {
                for (_, waiter) in p.drain() {
                    let _ = waiter.send(Err("browser connection closed".into()));
                }
            }
        });
        Ok(Self {
            out,
            pending,
            events,
            next: AtomicU64::new(1),
            tasks: vec![writer, reader],
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
    }

    /// Sends a command (optionally to a flattened target session) and waits for its reply.
    pub async fn call(&self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| anyhow::anyhow!("pending table poisoned"))?
            .insert(id, tx);
        let mut message = json!({"id":id,"method":method,"params":params});
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        if self.out.send(message.to_string()).await.is_err() {
            if let Ok(mut p) = self.pending.lock() {
                p.remove(&id);
            }
            bail!("browser connection closed");
        }
        match tokio::time::timeout(Duration::from_secs(30), rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(message))) => bail!("{method}: {message}"),
            Ok(Err(_)) => bail!("{method}: browser connection closed"),
            Err(_) => {
                if let Ok(mut p) = self.pending.lock() {
                    p.remove(&id);
                }
                bail!("{method}: timed out")
            }
        }
    }
}

impl Drop for Cdp {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
