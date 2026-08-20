//! A tiny control socket so a second invocation can talk to the running one.
//!
//! This is what makes `unburn hide` a reliable escape hatch: it needs no
//! window, no compositor cooperation and no global hotkey support.

use std::{
    fs,
    io::{self, BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

/// What a second invocation can ask the running instance to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Hide,
    Show,
    ShowWindow,
    Quit,
    Status,
    TestPattern(String),
}

impl Request {
    pub fn parse(line: &str) -> Option<Request> {
        let line = line.trim();
        let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
        Some(match verb {
            "hide" => Request::Hide,
            "show" => Request::Show,
            "window" => Request::ShowWindow,
            "quit" => Request::Quit,
            "status" => Request::Status,
            "test-pattern" => Request::TestPattern(rest.trim().to_owned()),
            _ => return None,
        })
    }

    pub fn wire(&self) -> String {
        match self {
            Request::Hide => "hide".into(),
            Request::Show => "show".into(),
            Request::ShowWindow => "window".into(),
            Request::Quit => "quit".into(),
            Request::Status => "status".into(),
            Request::TestPattern(p) => format!("test-pattern {p}"),
        }
    }
}

/// Human-readable snapshot the server hands out in response to `status`.
#[derive(Debug, Default, Clone)]
pub struct StatusSnapshot {
    pub text: String,
}

/// The listening half, owned by the running instance.
pub struct Server {
    path: PathBuf,
    requests: Receiver<Request>,
    status: Arc<Mutex<StatusSnapshot>>,
}

impl Server {
    /// Bind the control socket, or report that somebody else already has it.
    pub fn bind() -> Result<Server, BindError> {
        Server::bind_notified(|| {})
    }

    /// Same, but call `notify` whenever a request arrives so an idle main loop
    /// can wake up instead of polling.
    pub fn bind_notified(notify: impl Fn() + Send + 'static) -> Result<Server, BindError> {
        bind_at(socket_path(), notify)
    }

    /// Requests that arrived since the last call. Never blocks.
    pub fn poll(&self) -> Vec<Request> {
        self.requests.try_iter().collect()
    }

    /// Publish what `unburn status` should report.
    pub fn publish_status(&self, text: String) {
        if let Ok(mut status) = self.status.lock() {
            status.text = text;
        }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

fn bind_at(path: PathBuf, notify: impl Fn() + Send + 'static) -> Result<Server, BindError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }

    // A socket file left behind by a crash is not a running instance.
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            return Err(BindError::AlreadyRunning(path));
        }
        fs::remove_file(&path).ok();
    }

    let listener = UnixListener::bind(&path).map_err(BindError::Io)?;
    let (tx, requests) = mpsc::channel();
    let status = Arc::new(Mutex::new(StatusSnapshot::default()));

    let accept_status = Arc::clone(&status);
    thread::Builder::new()
        .name("unburn-ipc".into())
        .spawn(move || serve(listener, tx, accept_status, notify))
        .map_err(BindError::Io)?;

    Ok(Server {
        path,
        requests,
        status,
    })
}

impl Drop for Server {
    fn drop(&mut self) {
        fs::remove_file(&self.path).ok();
    }
}

fn serve(
    listener: UnixListener,
    tx: Sender<Request>,
    status: Arc<Mutex<StatusSnapshot>>,
    notify: impl Fn(),
) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();

        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            continue;
        }

        let reply = match Request::parse(&line) {
            Some(Request::Status) => {
                let text = status.lock().map(|s| s.text.clone()).unwrap_or_default();
                format!("ok {text}")
            }
            Some(request) => {
                if tx.send(request).is_ok() {
                    notify();
                    "ok".to_string()
                } else {
                    "err shutting down".to_string()
                }
            }
            None => format!("err unknown request: {}", line.trim()),
        };

        writeln!(stream, "{reply}").ok();
        stream.flush().ok();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BindError {
    #[error("another unburn instance already owns {0}")]
    AlreadyRunning(PathBuf),
    #[error("control socket: {0}")]
    Io(#[from] io::Error),
}

/// Send one request to a running instance and return its reply.
pub fn send(request: &Request) -> Result<String, io::Error> {
    send_to(&socket_path(), request)
}

fn send_to(path: &Path, request: &Request) -> Result<String, io::Error> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    writeln!(stream, "{}", request.wire())?;
    stream.flush()?;

    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    let reply = reply.trim().to_owned();

    match reply.strip_prefix("err ") {
        Some(message) => Err(io::Error::other(message.to_owned())),
        None => Ok(reply.strip_prefix("ok").unwrap_or(&reply).trim().to_owned()),
    }
}

/// `$XDG_RUNTIME_DIR/unburn-{uid}.sock`, falling back to `/tmp`.
pub fn socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);

    let user = rustix::process::getuid().as_raw();
    base.join(format!("unburn-{user}.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip_through_the_wire_format() {
        for request in [
            Request::Hide,
            Request::Show,
            Request::ShowWindow,
            Request::Quit,
            Request::Status,
            Request::TestPattern("50".into()),
        ] {
            assert_eq!(
                Request::parse(&request.wire()),
                Some(request.clone()),
                "{request:?}"
            );
        }
    }

    #[test]
    fn junk_is_rejected() {
        assert_eq!(Request::parse("rm -rf /"), None);
        assert_eq!(Request::parse(""), None);
    }

    fn unique_socket() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "unburn-ipc-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.join("unburn.sock")
    }

    #[test]
    fn the_socket_is_one_per_user() {
        let path = socket_path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap();
        assert!(name.starts_with("unburn-"), "{name}");
        assert!(name.ends_with(".sock"), "{name}");
        let uid = name.trim_start_matches("unburn-").trim_end_matches(".sock");
        assert!(
            uid.chars().all(|c| c.is_ascii_digit()),
            "socket name must not encode a profile: {name}"
        );
    }

    #[test]
    fn a_client_reaches_the_server() {
        let path = unique_socket();
        let server = bind_at(path.clone(), || {}).unwrap();
        server.publish_status("compensation on".into());

        assert_eq!(send_to(&path, &Request::Status).unwrap(), "compensation on");
        send_to(&path, &Request::Hide).unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            if server.poll().contains(&Request::Hide) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the server did not receive the hide request within one second"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_second_instance_is_refused() {
        let path = unique_socket();
        let _first = bind_at(path.clone(), || {}).unwrap();
        assert!(matches!(
            bind_at(path, || {}),
            Err(BindError::AlreadyRunning(_))
        ));
    }
}
