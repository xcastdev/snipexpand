use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, watch, Mutex, Semaphore};
use tokio::time::{timeout, Instant};

pub fn socket_path() -> anyhow::Result<PathBuf> {
    let runtime_dir =
        std::env::var("XDG_RUNTIME_DIR").map_err(|_| anyhow!("XDG_RUNTIME_DIR not set"))?;
    Ok(PathBuf::from(runtime_dir).join("snipexpand.sock"))
}

pub enum IpcCmd {
    Prompt {
        connection: u64,
        message: serde_json::Value,
    },
    PromptDisconnected {
        connection: u64,
    },
    Reload,
    Status,
    Enable,
    Disable,
    Toggle,
    Group(crate::groups::Request),
    Paste {
        trigger: String,
        source: Option<String>,
    },
}

#[derive(Deserialize)]
struct PasteRequest {
    trigger: String,
    source: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PasteRequestWire {
    Trigger(String),
    Request(PasteRequest),
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonStatus {
    pub running: bool,
    pub enabled: bool,
    pub version: String,
    pub pid: u32,
    pub injection_backend: String,
    pub match_groups: usize,
    pub triggers: usize,
    pub files: usize,
    pub config_valid: bool,
}

/// A bounded connection-specific response channel. Socket writes occur in workers.
#[derive(Clone)]
pub struct PromptSender {
    connection: u64,
    outgoing: mpsc::Sender<Vec<u8>>,
    max_frame_bytes: usize,
    closed: watch::Sender<bool>,
}

impl PromptSender {
    pub fn connection(&self) -> u64 {
        self.connection
    }

    pub fn close(&self) {
        let _ = self.closed.send(true);
    }

    pub fn try_send(&self, message: &serde_json::Value) -> anyhow::Result<()> {
        if *self.closed.borrow() {
            return Err(anyhow!("connection is closing"));
        }
        let mut wire =
            serde_json::to_vec(message).map_err(|_| anyhow!("response encoding failed"))?;
        if wire.len() > self.max_frame_bytes {
            return Err(anyhow!("response exceeds frame limit"));
        }
        wire.push(b'\n');
        self.outgoing
            .try_send(wire)
            .map_err(|_| anyhow!("connection unavailable or response queue full"))
    }
}

pub struct IpcReply {
    sender: PromptSender,
}
impl IpcReply {
    pub fn sender(&self) -> PromptSender {
        self.sender.clone()
    }

    pub async fn write_all(&mut self, wire: &[u8]) -> std::io::Result<()> {
        if wire.len() > self.sender.max_frame_bytes + 1 {
            return Err(std::io::Error::other("response exceeds frame limit"));
        }
        self.sender
            .outgoing
            .try_send(wire.to_vec())
            .map_err(|_| std::io::Error::other("connection unavailable or response queue full"))
    }
}

type Event = (IpcCmd, IpcReply);

struct QueuedEvent {
    event: Event,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

async fn deliver(
    events: &mpsc::Sender<QueuedEvent>,
    slots: &Arc<Semaphore>,
    event: Event,
) -> Result<(), ()> {
    let permit = slots.clone().acquire_owned().await.map_err(|_| ())?;
    events
        .send(QueuedEvent {
            event,
            _permit: permit,
        })
        .await
        .map_err(|_| ())
}

pub struct IpcServer {
    events: Mutex<mpsc::Receiver<QueuedEvent>>,
    task: tokio::task::JoinHandle<()>,
    path: PathBuf,
    socket_identity: (u64, u64),
}

impl IpcServer {
    #[cfg(test)]
    pub async fn new(path: &Path) -> anyhow::Result<Self> {
        Self::new_with_settings(path, &crate::fields::PromptSettings::default()).await
    }

    pub async fn new_with_settings(
        path: &Path,
        settings: &crate::fields::PromptSettings,
    ) -> anyhow::Result<Self> {
        settings.validate()?;
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        let metadata = std::fs::metadata(path)?;
        let socket_identity = (metadata.dev(), metadata.ino());
        let (events_tx, events) =
            mpsc::channel(settings.max_connections * settings.max_queued_messages);
        let settings = settings.clone();
        let task = tokio::spawn(async move {
            let permits = Arc::new(Semaphore::new(settings.max_connections));
            let mut workers = tokio::task::JoinSet::new();
            let mut next_connection = 0u64;
            loop {
                tokio::select! {
                    Some(_) = workers.join_next(), if !workers.is_empty() => {}
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break; };
                        if !peer_allowed(stream.peer_cred().ok().map(|cred| cred.uid())) { continue; }
                        let Ok(permit) = permits.clone().try_acquire_owned() else { continue; };
                        let Some(connection) = next_connection.checked_add(1) else { break; };
                        next_connection = connection;
                        let events = events_tx.clone();
                        let limits = settings.clone();
                        workers.spawn(async move {
                            let _permit = permit;
                            connection_worker(stream, connection, events, limits).await;
                        });
                    }
                }
            }
        });
        Ok(Self {
            events: Mutex::new(events),
            task,
            path: path.to_path_buf(),
            socket_identity,
        })
    }

    pub async fn accept(&self) -> anyhow::Result<Event> {
        self.events
            .lock()
            .await
            .recv()
            .await
            .map(|queued| queued.event)
            .ok_or_else(|| anyhow!("IPC listener stopped"))
    }
}

fn peer_allowed(uid: Option<u32>) -> bool {
    // SO_PEERCRED authenticates the connected process, independently of socket permissions.
    uid == Some(unsafe { libc::geteuid() })
}

async fn read_frame<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
    limit: usize,
    duration: Duration,
    idle: bool,
) -> std::io::Result<Vec<u8>> {
    // An idle registered handler waits for its lease in the daemon. Once a frame
    // starts, its entire remainder must arrive within one fixed deadline.
    if idle && reader.fill_buf().await?.is_empty() {
        return Ok(Vec::new());
    }
    let deadline = Instant::now() + duration;
    let mut frame = Vec::new();
    loop {
        let bytes = tokio::time::timeout_at(deadline, reader.fill_buf())
            .await
            .map_err(|_| std::io::Error::other("frame timeout"))??;
        if bytes.is_empty() {
            if frame.len() > limit {
                return Err(std::io::Error::other("frame too large"));
            }
            return Ok(frame);
        }
        let used = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|i| i + 1)
            .unwrap_or(bytes.len());
        if frame.len() + used > limit + 1 {
            return Err(std::io::Error::other("frame too large"));
        }
        let complete = bytes[used - 1] == b'\n';
        frame.extend_from_slice(&bytes[..used]);
        reader.consume(used);
        if complete {
            frame.pop();
            return Ok(frame);
        }
    }
}

async fn connection_worker(
    stream: UnixStream,
    connection: u64,
    events: mpsc::Sender<QueuedEvent>,
    settings: crate::fields::PromptSettings,
) {
    let inbound_slots = Arc::new(Semaphore::new(settings.max_queued_messages));
    let duration = Duration::from_millis(settings.ack_timeout_ms);
    let mut reader = BufReader::new(stream);
    let Ok(frame) = read_frame(&mut reader, settings.max_frame_bytes, duration, false).await else {
        return;
    };
    let Ok(command) = std::str::from_utf8(&frame) else {
        return;
    };
    let command = command.trim();
    let (outgoing, mut responses) = mpsc::channel(settings.max_queued_messages);
    let (closed, mut closing) = watch::channel(false);
    let sender = PromptSender {
        connection,
        outgoing,
        max_frame_bytes: settings.max_frame_bytes,
        closed,
    };
    if let Some(json) = command.strip_prefix("prompt\t") {
        let Ok(message) = serde_json::from_str(json) else {
            let _ = timeout(
                duration,
                reader.get_mut().write_all(
                    b"{\"version\":1,\"type\":\"error\",\"code\":\"invalid_message\"}\n",
                ),
            )
            .await;
            return;
        };
        if deliver(
            &events,
            &inbound_slots,
            (
                IpcCmd::Prompt {
                    connection,
                    message,
                },
                IpcReply {
                    sender: sender.clone(),
                },
            ),
        )
        .await
        .is_err()
        {
            return;
        }
        let buffered = reader.buffer().to_vec();
        let (read, mut write) = reader.into_inner().into_split();
        // Preserve bytes read ahead with the first frame, including coalesced JSON.
        let mut reader = BufReader::new(tokio::io::AsyncReadExt::chain(
            std::io::Cursor::new(buffered),
            read,
        ));
        let read_events = events.clone();
        let read_sender = sender.clone();
        let read_loop = async {
            loop {
                let frame =
                    read_frame(&mut reader, settings.max_frame_bytes, duration, true).await?;
                if frame.is_empty() {
                    return Ok::<(), std::io::Error>(());
                }
                let message = serde_json::from_slice(&frame)
                    .map_err(|_| std::io::Error::other("invalid message"))?;
                deliver(
                    &read_events,
                    &inbound_slots,
                    (
                        IpcCmd::Prompt {
                            connection,
                            message,
                        },
                        IpcReply {
                            sender: read_sender.clone(),
                        },
                    ),
                )
                .await
                .map_err(|_| std::io::Error::other("daemon stopped"))?;
            }
        };
        let invalid_frame = {
            let mut writing_closed = closing.clone();
            let write_loop = async {
                loop {
                    if *writing_closed.borrow() && responses.is_empty() {
                        break;
                    }
                    let wire = tokio::select! {
                        wire = responses.recv() => wire,
                        _ = writing_closed.changed() => continue,
                    };
                    let Some(wire) = wire else {
                        break;
                    };
                    timeout(duration, write.write_all(&wire))
                        .await
                        .map_err(|_| std::io::Error::other("write timeout"))??;
                }
                Ok::<(), std::io::Error>(())
            };
            tokio::pin!(write_loop);
            tokio::select! {
                result = read_loop => result.is_err(),
                _ = &mut write_loop => false,
                _ = closing.changed() => {
                    // A terminal error followed by Close must reach a responsive
                    // client. Drain queued frames with a bounded total deadline.
                    let _ = timeout(duration, &mut write_loop).await;
                    false
                },
            }
        };
        if invalid_frame {
            let _ = timeout(
                duration,
                write.write_all(b"{\"version\":1,\"type\":\"error\",\"code\":\"invalid_frame\"}\n"),
            )
            .await;
        }
        let _ = deliver(
            &events,
            &inbound_slots,
            (
                IpcCmd::PromptDisconnected { connection },
                IpcReply { sender },
            ),
        )
        .await;
        return;
    }
    let parsed = match command {
        "reload" => Some(IpcCmd::Reload),
        "status" => Some(IpcCmd::Status),
        "enable" => Some(IpcCmd::Enable),
        "disable" => Some(IpcCmd::Disable),
        "toggle" => Some(IpcCmd::Toggle),
        value if value.starts_with("group\t") => {
            serde_json::from_str(&value[6..]).ok().map(IpcCmd::Group)
        }
        value if value.starts_with("paste\t") => {
            serde_json::from_str(&value[6..]).ok().map(|request| {
                let request = match request {
                    PasteRequestWire::Trigger(trigger) => PasteRequest {
                        trigger,
                        source: None,
                    },
                    PasteRequestWire::Request(request) => request,
                };
                IpcCmd::Paste {
                    trigger: request.trigger,
                    source: request.source,
                }
            })
        }
        _ => None,
    };
    let Some(cmd) = parsed else {
        if command.starts_with("group\t") {
            let _ = timeout(
                duration,
                reader
                    .get_mut()
                    .write_all(b"{\"status\":\"error\",\"error\":\"invalid group request\"}\n"),
            )
            .await;
        }
        return;
    };
    if deliver(&events, &inbound_slots, (cmd, IpcReply { sender }))
        .await
        .is_err()
    {
        return;
    }
    if let Ok(Some(wire)) = timeout(duration, responses.recv()).await {
        let _ = timeout(duration, reader.get_mut().write_all(&wire)).await;
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.task.abort();
        if std::fs::metadata(&self.path)
            .is_ok_and(|metadata| (metadata.dev(), metadata.ino()) == self.socket_identity)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[allow(dead_code)]
pub async fn send_cmd(path: &Path, cmd: &str) -> anyhow::Result<String> {
    let mut stream = UnixStream::connect(path).await?;
    stream.write_all(format!("{}\n", cmd).as_bytes()).await?;
    // Signal that we are done writing.
    stream.shutdown().await?;
    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).await?;
    Ok(response.trim_end_matches('\n').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn stalled_reader_does_not_block_management() {
        let dir = TempDir::new().unwrap();
        let path = tmp_sock(&dir);
        let server = IpcServer::new(&path).await.unwrap();
        let _stalled = UnixStream::connect(&path).await.unwrap();
        let client = tokio::spawn(async move { send_cmd(&path, "status").await.unwrap() });
        let (cmd, mut reply) = timeout(Duration::from_millis(500), server.accept())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(cmd, IpcCmd::Status));
        reply.write_all(b"ok\n").await.unwrap();
        assert_eq!(client.await.unwrap(), "ok");
    }

    #[tokio::test]
    async fn coalesced_and_split_prompt_frames_preserve_connection() {
        let dir = TempDir::new().unwrap();
        let path = tmp_sock(&dir);
        let server = IpcServer::new(&path).await.unwrap();
        let mut client = UnixStream::connect(&path).await.unwrap();
        client.write_all(b"prompt\t{\"version\":1,\"type\":\"register\"}\n{\"version\":1,\"type\":\"renew\"}\n").await.unwrap();
        let (first, reply) = server.accept().await.unwrap();
        let connection = match first {
            IpcCmd::Prompt {
                connection,
                message,
            } => {
                assert_eq!(message["type"], "register");
                connection
            }
            _ => panic!("expected registration"),
        };
        let (second, _) = server.accept().await.unwrap();
        assert!(
            matches!(second, IpcCmd::Prompt { connection: id, message } if id == connection && message["type"] == "renew")
        );
        client.write_all(b"{\"version\":1,").await.unwrap();
        client.write_all(b"\"type\":\"status\"}\n").await.unwrap();
        let (third, _) = server.accept().await.unwrap();
        assert!(
            matches!(third, IpcCmd::Prompt { connection: id, message } if id == connection && message["type"] == "status")
        );
        reply.sender().close();
        let (disconnected, _) = server.accept().await.unwrap();
        assert!(
            matches!(disconnected, IpcCmd::PromptDisconnected { connection: id } if id == connection)
        );
    }

    #[tokio::test]
    async fn frame_limit_is_enforced_before_event_delivery() {
        let dir = TempDir::new().unwrap();
        let path = tmp_sock(&dir);
        let settings = crate::fields::PromptSettings {
            max_frame_bytes: 64,
            ..Default::default()
        };
        let server = IpcServer::new_with_settings(&path, &settings)
            .await
            .unwrap();
        let mut client = UnixStream::connect(&path).await.unwrap();
        client.write_all(&[b'x'; 66]).await.unwrap();
        assert!(timeout(Duration::from_millis(50), server.accept())
            .await
            .is_err());
        let mut bytes = [0u8; 1];
        assert_eq!(
            tokio::io::AsyncReadExt::read(&mut client, &mut bytes)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn accepted_management_progresses_despite_prompt_flood() {
        let dir = TempDir::new().unwrap();
        let path = tmp_sock(&dir);
        let settings = crate::fields::PromptSettings {
            max_queued_messages: 2,
            ..Default::default()
        };
        let server = IpcServer::new_with_settings(&path, &settings)
            .await
            .unwrap();
        let mut prompt = UnixStream::connect(&path).await.unwrap();
        prompt
            .write_all(b"prompt\t{\"version\":1,\"type\":\"register\"}\n")
            .await
            .unwrap();
        let (_, prompt_reply) = server.accept().await.unwrap();
        let flood = b"{\"version\":1,\"type\":\"status\"}\n".repeat(100);
        prompt.write_all(&flood).await.unwrap();
        let client = tokio::spawn(async move { send_cmd(&path, "status").await.unwrap() });
        timeout(Duration::from_secs(1), async {
            loop {
                let (cmd, mut reply) = server.accept().await.unwrap();
                if matches!(cmd, IpcCmd::Status) {
                    reply.write_all(b"ok\n").await.unwrap();
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(client.await.unwrap(), "ok");
        prompt_reply.sender().close();
    }

    #[tokio::test]
    async fn terminal_response_is_flushed_before_explicit_close() {
        let dir = TempDir::new().unwrap();
        let path = tmp_sock(&dir);
        let server = IpcServer::new(&path).await.unwrap();
        let mut client = UnixStream::connect(&path).await.unwrap();
        client
            .write_all(b"prompt\t{\"version\":1,\"type\":\"register\"}\n")
            .await
            .unwrap();
        let (_, reply) = server.accept().await.unwrap();
        let sender = reply.sender();
        sender
            .try_send(&serde_json::json!({"version":1,"type":"error","code":"busy"}))
            .unwrap();
        sender.close();
        assert!(sender
            .try_send(&serde_json::json!({"version":1,"type":"status"}))
            .is_err());
        let mut reader = BufReader::new(client);
        let mut line = String::new();
        timeout(Duration::from_secs(1), reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert!(line.contains("busy"));
        line.clear();
        assert_eq!(reader.read_line(&mut line).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn connection_cap_refuses_excess_and_releases_after_disconnect() {
        let dir = TempDir::new().unwrap();
        let path = tmp_sock(&dir);
        let settings = crate::fields::PromptSettings {
            max_connections: 1,
            ..Default::default()
        };
        let server = IpcServer::new_with_settings(&path, &settings)
            .await
            .unwrap();
        let mut handler = UnixStream::connect(&path).await.unwrap();
        handler
            .write_all(b"prompt\t{\"version\":1,\"type\":\"register\"}\n")
            .await
            .unwrap();
        let (_, reply) = server.accept().await.unwrap();
        let mut excess = UnixStream::connect(&path).await.unwrap();
        let mut bytes = [0u8; 1];
        assert_eq!(
            timeout(
                Duration::from_secs(1),
                tokio::io::AsyncReadExt::read(&mut excess, &mut bytes)
            )
            .await
            .unwrap()
            .unwrap(),
            0
        );
        reply.sender().close();
        let (cmd, _) = server.accept().await.unwrap();
        assert!(matches!(cmd, IpcCmd::PromptDisconnected { .. }));
        drop(handler);
        tokio::task::yield_now().await;
        let client = tokio::spawn(async move { send_cmd(&path, "status").await.unwrap() });
        let (cmd, mut reply) = timeout(Duration::from_secs(1), server.accept())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(cmd, IpcCmd::Status));
        reply.write_all(b"ok\n").await.unwrap();
        assert_eq!(client.await.unwrap(), "ok");
    }

    #[tokio::test]
    async fn partial_frame_has_fixed_deadline_and_accept_is_cancel_safe() {
        let dir = TempDir::new().unwrap();
        let path = tmp_sock(&dir);
        let settings = crate::fields::PromptSettings {
            ack_timeout_ms: 20,
            ..Default::default()
        };
        let server = IpcServer::new_with_settings(&path, &settings)
            .await
            .unwrap();
        let mut stalled = UnixStream::connect(&path).await.unwrap();
        stalled.write_all(b"sta").await.unwrap();
        assert!(timeout(Duration::from_millis(40), server.accept())
            .await
            .is_err());
        let mut bytes = [0u8; 1];
        assert_eq!(
            tokio::io::AsyncReadExt::read(&mut stalled, &mut bytes)
                .await
                .unwrap(),
            0
        );
        let client = tokio::spawn(async move { send_cmd(&path, "status").await.unwrap() });
        let (cmd, mut reply) = server.accept().await.unwrap();
        assert!(matches!(cmd, IpcCmd::Status));
        reply.write_all(b"ok\n").await.unwrap();
        assert_eq!(client.await.unwrap(), "ok");
    }

    #[tokio::test]
    async fn replacing_socket_does_not_let_old_server_unlink_new_socket() {
        let dir = TempDir::new().unwrap();
        let path = tmp_sock(&dir);
        let old = IpcServer::new(&path).await.unwrap();
        let new = IpcServer::new(&path).await.unwrap();
        drop(old);
        assert!(path.exists());
        drop(new);
        assert!(!path.exists());
    }

    #[test]
    fn peer_uid_and_outgoing_queue_are_bounded() {
        let uid = unsafe { libc::geteuid() };
        assert!(peer_allowed(Some(uid)));
        assert!(!peer_allowed(Some(uid.wrapping_add(1))));
        assert!(!peer_allowed(None));
        let (outgoing, _receiver) = mpsc::channel(1);
        let (closed, _closing) = watch::channel(false);
        let sender = PromptSender {
            connection: 1,
            outgoing,
            max_frame_bytes: 64,
            closed,
        };
        let message = serde_json::json!({"version":1,"type":"status"});
        sender.try_send(&message).unwrap();
        assert!(sender.try_send(&message).is_err());
        assert!(sender
            .try_send(&serde_json::json!({"secret":"x".repeat(100)}))
            .is_err());
    }

    #[tokio::test]
    async fn malformed_prompt_is_sanitized_and_socket_is_private() {
        let dir = TempDir::new().unwrap();
        let path = tmp_sock(&dir);
        let _server = IpcServer::new(&path).await.unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let response = send_cmd(&path, "prompt\t{\"SECRET-DISTINCTIVE\": invalid}")
            .await
            .unwrap();
        assert!(response.contains("invalid_message"));
        assert!(!response.contains("SECRET"));
    }

    /// Helper: create a temporary socket path inside a TempDir.
    fn tmp_sock(dir: &TempDir) -> PathBuf {
        dir.path().join("test.sock")
    }

    #[tokio::test]
    async fn group_protocol_reports_bad_requests_and_keeps_accepting() {
        let dir = TempDir::new().unwrap();
        let path = tmp_sock(&dir);
        let server = IpcServer::new(&path).await.unwrap();
        let client = tokio::spawn(async move {
            let response = send_cmd(&path, r#"group	{"operation":"toggle"}"#)
                .await
                .unwrap();
            assert!(matches!(
                serde_json::from_str::<crate::groups::Response>(&response).unwrap(),
                crate::groups::Response::Error { .. }
            ));
            send_cmd(&path, r#"group	{"operation":"toggle","name":"work"}"#)
                .await
                .unwrap()
        });
        let (cmd, mut stream) = server.accept().await.unwrap();
        assert!(
            matches!(cmd, IpcCmd::Group(crate::groups::Request::Toggle { name }) if name == "work")
        );
        stream
            .write_all(b"{\"status\":\"ok\",\"groups\":[]}\n")
            .await
            .unwrap();
        assert!(client.await.unwrap().contains("groups"));
    }

    #[tokio::test]
    async fn test_server_receives_reload_command() {
        let dir = TempDir::new().unwrap();
        let path = tmp_sock(&dir);

        let server = IpcServer::new(&path).await.unwrap();

        // Spawn a client task that sends "reload\n".
        let path_clone = path.clone();
        tokio::spawn(async move {
            send_cmd(&path_clone, "reload").await.unwrap();
        });

        let (cmd, _) = server.accept().await.unwrap();
        assert!(matches!(cmd, IpcCmd::Reload));
    }

    #[tokio::test]
    async fn test_server_receives_status_command() {
        let dir = TempDir::new().unwrap();
        let path = tmp_sock(&dir);

        let server = IpcServer::new(&path).await.unwrap();

        let path_clone = path.clone();
        tokio::spawn(async move {
            send_cmd(&path_clone, "status").await.unwrap();
        });

        let (cmd, _) = server.accept().await.unwrap();
        assert!(matches!(cmd, IpcCmd::Status));
    }

    #[tokio::test]
    async fn test_server_receives_paste_command() {
        let dir = TempDir::new().unwrap();
        let path = tmp_sock(&dir);
        let server = IpcServer::new(&path).await.unwrap();
        let path_clone = path.clone();
        tokio::spawn(async move {
            send_cmd(
                &path_clone,
                "paste\t{\"trigger\":\";mail\",\"source\":null}",
            )
            .await
            .unwrap();
        });
        let (cmd, _) = server.accept().await.unwrap();
        assert!(matches!(cmd, IpcCmd::Paste { trigger, source: None } if trigger == ";mail"));
    }

    #[tokio::test]
    async fn test_server_accepts_legacy_paste_command() {
        let dir = TempDir::new().unwrap();
        let path = tmp_sock(&dir);
        let server = IpcServer::new(&path).await.unwrap();
        let path_clone = path.clone();
        tokio::spawn(async move {
            send_cmd(&path_clone, "paste\t\";mail\"").await.unwrap();
        });
        let (cmd, _) = server.accept().await.unwrap();
        assert!(matches!(cmd, IpcCmd::Paste { trigger, source: None } if trigger == ";mail"));
    }

    #[tokio::test]
    async fn test_stale_socket_is_removed_on_startup() {
        let dir = TempDir::new().unwrap();
        let path = tmp_sock(&dir);

        // Create a stale file at the socket path.
        std::fs::write(&path, b"stale").unwrap();
        assert!(path.exists());

        // IpcServer::new should remove the stale file and bind successfully.
        let _server = IpcServer::new(&path).await.unwrap();
        // If we get here without error the stale-removal logic worked.
    }

    #[test]
    fn daemon_status_round_trips_as_json() {
        let status = DaemonStatus {
            running: true,
            enabled: true,
            version: env!("CARGO_PKG_VERSION").to_string(),
            pid: 42,
            injection_backend: "wayland".to_string(),
            match_groups: 3,
            triggers: 4,
            files: 2,
            config_valid: true,
        };
        let encoded = serde_json::to_string(&status).unwrap();
        let decoded: DaemonStatus = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, status);
    }

    #[tokio::test]
    async fn test_server_receives_state_commands() {
        for (wire, expected) in [
            ("enable", IpcCmd::Enable),
            ("disable", IpcCmd::Disable),
            ("toggle", IpcCmd::Toggle),
        ] {
            let dir = TempDir::new().unwrap();
            let path = tmp_sock(&dir);
            let server = IpcServer::new(&path).await.unwrap();
            let path_clone = path.clone();
            tokio::spawn(async move {
                send_cmd(&path_clone, wire).await.unwrap();
            });
            let (actual, _) = server.accept().await.unwrap();
            assert_eq!(
                std::mem::discriminant(&actual),
                std::mem::discriminant(&expected)
            );
        }
    }
}
