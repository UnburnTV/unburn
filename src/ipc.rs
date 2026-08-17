//! A tiny control socket so a second invocation can talk to the running one.
//!
//! This is what makes `unburn --disable` a reliable escape hatch: it needs no
//! window, no compositor cooperation and no global hotkey support.

use std::{
    fs,
    io::{self, BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
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
    Disable,
    Enable,
    ToggleBypass,
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
            "disable" => Request::Disable,
            "enable" => Request::Enable,
            "bypass" => Request::ToggleBypass,
            "show" => Request::ShowWindow,
            "quit" => Request::Quit,
            "status" => Request::Status,
            "test-pattern" => Request::TestPattern(rest.trim().to_owned()),
            _ => return None,
        })
    }

    pub fn wire(&self) -> String {
        match self {
            Request::Disable => "disable".into(),
            Request::Enable => "enable".into(),
            Request::ToggleBypass => "bypass".into(),
            Request::ShowWindow => "show".into(),
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
    pub fn bind(profile: Option<&str>) -> Result<Server, BindError> {
        Server::bind_notified(profile, || {})
    }

    /// Same, but call `notify` whenever a request arrives so an idle main loop
    /// can wake up instead of polling.
    pub fn bind_notified(
        profile: Option<&str>,
        notify: impl Fn() + Send + 'static,
    ) -> Result<Server, BindError> {
        let path = socket_path(profile);
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

    /// Requests that arrived since the last call. Never blocks.
    pub fn poll(&self) -> Vec<Request> {
        self.requests.try_iter().collect()
    }

    /// Publish what `--status` should report.
    pub fn publish_status(&self, text: String) {
        if let Ok(mut status) = self.status.lock() {
            status.text = text;
        }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }
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
pub fn send(profile: Option<&str>, request: &Request) -> Result<String, io::Error> {
    let path = socket_path(profile);
    let mut stream = UnixStream::connect(&path)?;
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

/// `$XDG_RUNTIME_DIR/unburn[-profile].sock`, falling back to `/tmp`.
pub fn socket_path(profile: Option<&str>) -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);

    let user = rustix::process::getuid().as_raw();
    let name = match profile {
        Some(p) if !p.is_empty() => format!("unburn-{user}-{}.sock", sanitize(p)),
        _ => format!("unburn-{user}.sock"),
    };
    base.join(name)
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip_through_the_wire_format() {
        for request in [
            Request::Disable,
            Request::Enable,
            Request::ToggleBypass,
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

    #[test]
    fn the_socket_is_per_user_and_per_profile() {
        let default = socket_path(None);
        let named = socket_path(Some("living-room"));
        assert_ne!(default, named);
        assert!(named.to_str().unwrap().contains("living-room"));
    }

    #[test]
    fn a_client_reaches_the_server() {
        let profile = format!("test-{}", std::process::id());
        let server = Server::bind(Some(&profile)).unwrap();
        server.publish_status("compensation on".into());

        assert_eq!(
            send(Some(&profile), &Request::Status).unwrap(),
            "compensation on"
        );
        send(Some(&profile), &Request::Disable).unwrap();

        // Give the server thread a moment to hand the request over.
        std::thread::sleep(Duration::from_millis(50));
        assert!(server.poll().contains(&Request::Disable));
    }

    #[test]
    fn a_second_instance_is_refused() {
        let profile = format!("dup-{}", std::process::id());
        let _first = Server::bind(Some(&profile)).unwrap();
        assert!(matches!(
            Server::bind(Some(&profile)),
            Err(BindError::AlreadyRunning(_))
        ));
    }

    #[test]
    fn connecting_to_nothing_fails_cleanly() {
        let profile = format!("absent-{}", std::process::id());
        assert!(send(Some(&profile), &Request::Disable).is_err());
    }
}
