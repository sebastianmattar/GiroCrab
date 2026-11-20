//! Local socket for the read-only interface (#136): a Unix domain socket, no
//! network port. One JSON request per line, one JSON response per line.
//!
//! - The socket lives in a directory only the user can enter (0700: created so
//!   here, an existing one must already be private), the socket itself is 0600,
//!   and a client running as another user is closed at once.
//! - Every request carries the access token (`"token": "gcapi_…"`); only its
//!   hash is stored in the vault, and the scope is read on every request.
//! - The server holds the vault weakly: once the app drops it (locked, swapped),
//!   requests answer `locked`. The app also stops the server when it locks.
//! - Bounded: at most `MAX_CLIENTS` connections (more get `busy`), `MAX_LINE`
//!   bytes per request, a request must arrive within `REQUEST_DEADLINE`, idle
//!   connections close after `IDLE`, a client that does not read its answer is
//!   dropped after `WRITE_TIMEOUT`.
//! - Paced: requests take turns, and the next one starts only after a pause as
//!   long as the previous one took (at most `MAX_PAUSE`; the answer itself goes
//!   out at once), so the app gets the vault at least half the time.
//! - No polling: threads sleep in `poll(2)` on their socket and a stop pipe.
//!   `stop()` never waits: it wakes them, removes the socket file and returns.
//! - Several instances (a debug build next to the app): a lock file next to the
//!   socket (`<socket>.lock`, `flock`) decides who serves, and a server only removes the socket
//!   file it created itself.

use std::fs::File;
use std::io::{BufRead, BufReader, ErrorKind, PipeReader, PipeWriter, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::{AsFd, BorrowedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use super::{ApiError, ApiErrorCode, response};
use crate::error::CoreError;
use crate::i18n::{tr, trf};
use crate::vault::Vault;

const MAX_CLIENTS: usize = 4;
const MAX_LINE: usize = 64 * 1024;
const IDLE: Duration = Duration::from_secs(60);
/// From the first byte of a request to its newline.
const REQUEST_DEADLINE: Duration = if cfg!(test) {
    Duration::from_millis(300)
} else {
    Duration::from_secs(10)
};
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// The `busy` answer is written on the accept thread.
const BUSY_WRITE_TIMEOUT: Duration = Duration::from_millis(100);
const MAX_PAUSE: Duration = Duration::from_millis(250);
/// After a failed `accept()` (e.g. no descriptors left).
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// The running socket; `stop()` (or dropping it) closes it and removes the file.
#[derive(uniffi::Object)]
pub struct LocalApiServer {
    path: PathBuf,
    /// (device, inode) of the socket file this server created.
    file_id: (u64, u64),
    stopper: Arc<Stopper>,
    /// Held (`flock`) while serving; released by `stop()`.
    lock: Mutex<Option<File>>,
}

#[uniffi::export]
impl LocalApiServer {
    /// Listens on `socket_path`. A missing directory is created with mode 0700,
    /// an existing one must already be private to this user (it is never
    /// chmod-ed). Another live server on the same path is an error, a stale
    /// socket there is replaced, any other file is an error.
    /// Errors are `InvalidInput` with a localized message for the status line.
    #[uniffi::constructor]
    pub fn start(vault: Arc<Vault>, socket_path: String) -> Result<Arc<Self>, CoreError> {
        let path = PathBuf::from(&socket_path);
        let stopper = Stopper::new().map_err(io_error)?;
        let (listener, file_id, lock) = bind(&path)?;
        let vault = Arc::downgrade(&vault);
        {
            let stopper = stopper.clone();
            if let Err(e) = std::thread::Builder::new()
                .name("girocrab-local-api".into())
                .spawn(move || accept_loop(listener, vault, stopper))
            {
                remove_own_socket(&path, file_id);
                return Err(io_error(e));
            }
        }
        Ok(Arc::new(Self {
            path,
            file_id,
            stopper,
            lock: Mutex::new(Some(lock)),
        }))
    }

    /// Wakes the threads (they end at once, a request still being written
    /// within `WRITE_TIMEOUT`), removes the socket file and releases the lock.
    /// Returns at once: safe on the main thread.
    pub fn stop(&self) {
        let mut lock = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if lock.is_none() {
            return;
        }
        // Cleanup belongs to the first stop only, while we still hold the
        // instance lock. After it is released, another server may bind this
        // path and the filesystem may even reuse our old socket's inode.
        self.stopper.stop();
        remove_own_socket(&self.path, self.file_id);
        lock.take();
    }
}

impl Drop for LocalApiServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The stop signal every thread waits on: a flag and a pipe whose read end
/// turns (and stays) readable once `stop()` wrote to it.
struct Stopper {
    stopped: AtomicBool,
    wake_rx: PipeReader,
    wake_tx: PipeWriter,
}

enum Wait {
    Ready,
    Stopped,
    TimedOut,
}

impl Stopper {
    fn new() -> std::io::Result<Arc<Self>> {
        // Close-on-exec like every descriptor std opens; closed on drop.
        let (wake_rx, wake_tx) = std::io::pipe()?;
        Ok(Arc::new(Self {
            stopped: AtomicBool::new(false),
            wake_rx,
            wake_tx,
        }))
    }

    fn stop(&self) {
        if !self.stopped.swap(true, Ordering::SeqCst) {
            // The only write, into an empty pipe: it cannot block.
            let _ = (&self.wake_tx).write(&[1]);
        }
    }

    /// Sleeps until `fd` is readable, `stop()` or `timeout` (None = no limit).
    fn wait_readable(&self, fd: BorrowedFd<'_>, timeout: Option<Duration>) -> Wait {
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            if self.stopped.load(Ordering::SeqCst) {
                return Wait::Stopped;
            }
            let ms = match deadline {
                None => -1,
                Some(d) => {
                    let left = d.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Wait::TimedOut;
                    }
                    left.as_millis().clamp(1, i32::MAX as u128) as i32
                }
            };
            match crate::sys::poll_readable(fd, self.wake_rx.as_fd(), ms) {
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) | Ok((_, true)) => return Wait::Stopped,
                Ok((true, false)) => return Wait::Ready,
                Ok((false, false)) => {}
            }
        }
    }
}

fn io_error(e: std::io::Error) -> CoreError {
    CoreError::invalid(trf("Lokaler Socket: {0}", &[&e]))
}

/// Writes one error line; the connection is closed or refused right after.
fn send_error(out: &mut impl Write, code: ApiErrorCode, message: &str) {
    let _ = writeln!(out, "{}", response(Err(ApiError::new(code, message))));
}

/// Uses or creates the private directory, takes the lock file and binds the
/// socket (mode 0600); returns the listener, the (device, inode) of the new
/// socket file and the held lock.
fn bind(path: &Path) -> Result<(UnixListener, (u64, u64), File), CoreError> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .ok_or_else(|| CoreError::invalid(tr("Der Socket-Pfad braucht einen Ordner.")))?;
    let created = match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == ErrorKind::NotFound => {
            // 0700 from the start, also for missing parents (no world-readable window).
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .map_err(io_error)?;
            true
        }
        _ => false,
    };
    let meta = std::fs::symlink_metadata(dir).map_err(io_error)?;
    if !meta.is_dir() || meta.uid() != crate::sys::getuid() {
        return Err(CoreError::invalid(tr(
            "Der Socket-Ordner gehört nicht diesem Benutzer.",
        )));
    }
    if created {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(io_error)?;
    } else if meta.mode() & 0o077 != 0 {
        // Never chmod a folder someone else chose (home, a shared folder).
        return Err(CoreError::invalid(trf(
            "Andere Benutzer können den Socket-Ordner öffnen: {0}",
            &[&dir.display()],
        )));
    }
    // Whoever holds the lock serves; with it, a socket file there is stale.
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path(path))
        .map_err(io_error)?;
    // flock(2) on Unix, held until `lock` closes.
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(CoreError::invalid(tr(
                "Eine andere GiroCrab-Instanz verwendet diesen Socket bereits.",
            )));
        }
        Err(std::fs::TryLockError::Error(e)) => return Err(io_error(e)),
    }
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_socket() => std::fs::remove_file(path).map_err(io_error)?,
        Ok(_) => {
            return Err(CoreError::invalid(tr(
                "Am Socket-Pfad liegt bereits eine andere Datei.",
            )));
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(io_error(e)),
    }
    let listener = UnixListener::bind(path).map_err(io_error)?;
    // From here on the file is ours: any later error removes it again.
    let ready = || -> std::io::Result<(u64, u64)> {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let m = std::fs::symlink_metadata(path)?;
        Ok((m.dev(), m.ino()))
    };
    match ready() {
        Ok(file_id) => Ok((listener, file_id, lock)),
        Err(e) => {
            let _ = std::fs::remove_file(path);
            Err(io_error(e))
        }
    }
}

/// `<socket>.lock` next to the socket (never the socket path itself). It stays
/// after `stop()`: removing a lock file others may be waiting on would race.
fn lock_path(socket: &Path) -> PathBuf {
    let mut name = socket.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// Removes the socket file only if it is still the one this server created.
fn remove_own_socket(path: &Path, file_id: (u64, u64)) {
    if std::fs::symlink_metadata(path)
        .is_ok_and(|m| m.file_type().is_socket() && (m.dev(), m.ino()) == file_id)
    {
        let _ = std::fs::remove_file(path);
    }
}

/// Counts a connection while it lives, also if its request panics.
struct ClientSlot(Arc<AtomicUsize>);

impl ClientSlot {
    fn take(clients: &Arc<AtomicUsize>) -> Self {
        clients.fetch_add(1, Ordering::SeqCst);
        Self(clients.clone())
    }
}

impl Drop for ClientSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn accept_loop(listener: UnixListener, vault: Weak<Vault>, stopper: Arc<Stopper>) {
    let clients = Arc::new(AtomicUsize::new(0));
    // One request at a time across all connections, and when the next may start (see `serve`).
    let turn = Arc::new(Mutex::new(Instant::now()));
    loop {
        // No time limit: only `stop()` ends the wait other than a connection.
        let Wait::Ready = stopper.wait_readable(listener.as_fd(), None) else {
            return;
        };
        let Ok((mut stream, _)) = listener.accept() else {
            // Out of descriptors or similar: the listener stays readable, so
            // back off instead of spinning.
            std::thread::sleep(ACCEPT_BACKOFF);
            continue;
        };
        // BSD hands the listener's O_NONBLOCK on to the accepted socket.
        if stream.set_nonblocking(false).is_err() || !same_user(&stream) {
            continue;
        }
        if clients.load(Ordering::SeqCst) >= MAX_CLIENTS {
            if stream.set_write_timeout(Some(BUSY_WRITE_TIMEOUT)).is_ok() {
                send_error(
                    &mut stream,
                    ApiErrorCode::Busy,
                    "too many connections, try again",
                );
            }
            continue;
        }
        if stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_err() {
            continue;
        }
        let slot = ClientSlot::take(&clients);
        let (vault, stopper, turn) = (vault.clone(), stopper.clone(), turn.clone());
        // On a spawn error the closure (and with it `slot`) is dropped.
        let _ = std::thread::Builder::new()
            .name("girocrab-local-api-client".into())
            .spawn(move || {
                let _slot = slot;
                serve(stream, &vault, &stopper, &turn);
            });
    }
}

/// The peer runs as this user (the socket's mode already keeps others out).
fn same_user(stream: &UnixStream) -> bool {
    // Unknown (unsupported platform, failed call) counts as someone else.
    crate::sys::peer_uid(stream).is_ok_and(|uid| uid == crate::sys::getuid())
}

/// Answers the requests of one connection until EOF, an error, a request over
/// `MAX_LINE` or slower than `REQUEST_DEADLINE`, `IDLE` without a request or `stop`.
fn serve(stream: UnixStream, vault: &Weak<Vault>, stopper: &Stopper, turn: &Mutex<Instant>) {
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let mut line: Vec<u8> = Vec::new();
    // `IDLE` after an answer, `REQUEST_DEADLINE` from a request's first byte.
    let mut deadline = Instant::now() + IDLE;
    loop {
        if reader.buffer().is_empty() {
            match stopper.wait_readable(
                reader.get_ref().as_fd(),
                Some(deadline.saturating_duration_since(Instant::now())),
            ) {
                Wait::Ready => {}
                Wait::Stopped => return,
                Wait::TimedOut => {
                    if !line.is_empty() {
                        send_error(&mut out, ApiErrorCode::InvalidParams, "request too slow");
                    }
                    return;
                }
            }
        }
        let starting = line.is_empty();
        // EOF ends the connection; a last request without a newline is still
        // answered (the next round sees EOF with nothing pending).
        let (used, complete) = match reader.fill_buf() {
            Ok([]) if line.is_empty() => return,
            Ok([]) => (0, true),
            Ok(chunk) => match chunk.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    line.extend_from_slice(&chunk[..i]);
                    (i + 1, true)
                }
                None => {
                    line.extend_from_slice(chunk);
                    (chunk.len(), false)
                }
            },
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return,
        };
        reader.consume(used);
        if starting {
            deadline = Instant::now() + REQUEST_DEADLINE;
        }
        if line.len() > MAX_LINE {
            send_error(&mut out, ApiErrorCode::InvalidParams, "request too large");
            return;
        }
        if !complete {
            continue;
        }
        let answer = {
            let mut next = turn.lock().unwrap_or_else(|e| e.into_inner());
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
            let started = Instant::now();
            let answer = answer(vault, stopper, &line);
            *next = Instant::now() + started.elapsed().min(MAX_PAUSE);
            answer
        };
        // Stopped (locked) meanwhile: nothing more goes out.
        let Some(answer) = answer else { return };
        line.clear();
        deadline = Instant::now() + IDLE;
        if writeln!(out, "{answer}").is_err() {
            return;
        }
    }
}

/// None once `stop()` ran: checked after taking the vault and again after the
/// request, so an answer computed while the app locked is never sent.
fn answer(vault: &Weak<Vault>, stopper: &Stopper, line: &[u8]) -> Option<String> {
    let stopped = || stopper.stopped.load(Ordering::SeqCst);
    if stopped() {
        return None;
    }
    // The vault is held only for this one request.
    let Some(vault) = vault.upgrade() else {
        return Some(response(Err(ApiError::new(
            ApiErrorCode::Locked,
            "GiroCrab is locked",
        ))));
    };
    let answer = match std::str::from_utf8(line) {
        Ok(text) => vault.local_api_socket_request(text.trim_end_matches('\r')),
        Err(_) => response(Err(ApiError::invalid("request is not UTF-8"))),
    };
    (!stopped()).then_some(answer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_api::ShareScope;
    use serde_json::{Value, json};
    use std::io::{BufRead, BufReader, Write};

    /// Upper bound for "`stop()` returns at once". `stop()` takes no lock a
    /// worker holds, but on a busy CI runner the test thread itself can lose
    /// the CPU for tens of milliseconds (a 50 ms bound failed at 62 ms). A
    /// `stop()` that waited for a worker would take `WRITE_TIMEOUT` or `IDLE`.
    const STOP_AT_ONCE: Duration = Duration::from_secs(1);

    /// A short directory: `sun_path` holds only 104 bytes on macOS, the usual
    /// temp directory is too deep for it.
    struct ShortDir(PathBuf);

    impl ShortDir {
        fn new() -> Self {
            let id = uuid::Uuid::new_v4().simple().to_string();
            Self(PathBuf::from(format!("/tmp/gc-{}", &id[..12])))
        }
        fn socket(&self) -> PathBuf {
            self.0.join("api").join("s.sock")
        }
        /// The socket's directory, made beforehand with `mode`.
        fn make_dir(&self, mode: u32) -> PathBuf {
            let path = self.socket();
            let dir = path.parent().unwrap();
            std::fs::create_dir_all(dir).unwrap();
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).unwrap();
            path
        }
    }

    fn start(d: &ShortDir, vault: &Arc<Vault>) -> Arc<LocalApiServer> {
        LocalApiServer::start(vault.clone(), d.socket().to_string_lossy().into()).unwrap()
    }

    impl Drop for ShortDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn vault() -> (crate::test_support::TempDir, Arc<Vault>, String) {
        let dir = crate::test_support::TempDir::new("local-api-socket");
        let v = Vault::open(
            dir.join("v.db").to_string_lossy().to_string(),
            vec![6u8; 32],
        )
        .unwrap();
        v.seed_demo_data().unwrap();
        let ids = v.list_accounts().unwrap().iter().map(|a| a.id).collect();
        v.set_local_api_scope(ShareScope {
            enabled: true,
            account_ids: ids,
            ..Default::default()
        })
        .unwrap();
        let token = v.create_local_api_token().unwrap();
        (dir, v, token)
    }

    struct Client {
        reader: BufReader<UnixStream>,
        writer: UnixStream,
    }

    impl Client {
        fn connect(path: &Path) -> Self {
            let s = UnixStream::connect(path).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            Self {
                writer: s.try_clone().unwrap(),
                reader: BufReader::new(s),
            }
        }
        fn send_raw(&mut self, line: &str) -> Value {
            // A line over the limit is answered and the connection closed while
            // the client is still writing, so `writeln!` may fail with EPIPE or
            // ECONNRESET. The answer is already queued then: ignore the write
            // error. If a normal request fails to send, `read` below fails.
            let _ = writeln!(self.writer, "{line}");
            self.read()
        }
        fn read(&mut self) -> Value {
            let mut resp = String::new();
            self.reader.read_line(&mut resp).unwrap();
            serde_json::from_str(&resp).unwrap()
        }
        fn assert_closed(&mut self) {
            let mut rest = String::new();
            assert_eq!(self.reader.read_line(&mut rest).unwrap_or(0), 0, "closed");
        }
        fn call(&mut self, token: &str, method: &str) -> Value {
            self.send_raw(&json!({ "v": 1, "token": token, "method": method }).to_string())
        }
    }

    #[test]
    fn answers_with_the_token_and_refuses_without() {
        let d = ShortDir::new();
        let (_v, vault, token) = vault();
        let server = start(&d, &vault);
        let mut c = Client::connect(&d.socket());
        let info = c.call(&token, "info");
        assert_eq!(info["result"]["api_version"], 1, "{info}");
        // several requests on one connection
        assert!(c.call(&token, "accounts")["result"].is_array());
        assert_eq!(
            c.call("gcapi_wrong", "info")["error"]["code"],
            "unauthorized"
        );
        assert_eq!(
            c.send_raw(r#"{"method":"info"}"#)["error"]["code"],
            "unauthorized"
        );
        // revoked: the old token stops working at once
        vault.revoke_local_api_token().unwrap();
        assert_eq!(c.call(&token, "info")["error"]["code"], "unauthorized");
        let token = vault.create_local_api_token().unwrap();
        assert!(c.call(&token, "info")["result"].is_object());
        // saving the scope keeps the token
        let scope = vault.local_api_scope().unwrap();
        vault
            .set_local_api_scope(ShareScope {
                token_sha256: String::new(),
                ..scope
            })
            .unwrap();
        assert!(c.call(&token, "info")["result"].is_object());
        // switching external access off revokes the key: on again does not bring it back
        let on = vault.local_api_scope().unwrap();
        assert!(on.enabled);
        vault
            .set_local_api_scope(ShareScope {
                enabled: false,
                ..on.clone()
            })
            .unwrap();
        assert!(vault.local_api_scope().unwrap().token_sha256.is_empty());
        vault.set_local_api_scope(on).unwrap();
        assert_eq!(c.call(&token, "info")["error"]["code"], "unauthorized");
        server.stop();
    }

    #[test]
    fn socket_is_private_and_removed_on_stop() {
        let d = ShortDir::new();
        let (_v, vault, _) = vault();
        let path = d.socket();
        let server = LocalApiServer::start(vault, path.to_string_lossy().into()).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        server.stop();
        assert!(!path.exists());
        assert!(UnixStream::connect(&path).is_err());
    }

    #[test]
    fn never_replaces_another_file() {
        let d = ShortDir::new();
        let (_v, vault, _) = vault();
        let path = d.make_dir(0o700);
        std::fs::write(&path, b"keep me").unwrap();
        assert!(LocalApiServer::start(vault, path.to_string_lossy().into()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"keep me");
    }

    #[test]
    fn a_dropped_vault_reads_as_locked() {
        let d = ShortDir::new();
        let (_v, vault, token) = vault();
        let server = start(&d, &vault);
        let mut c = Client::connect(&d.socket());
        assert!(c.call(&token, "info")["result"].is_object());
        drop(vault);
        assert_eq!(c.call(&token, "info")["error"]["code"], "locked");
        server.stop();
    }

    #[test]
    fn oversized_and_malformed_requests_are_answered_and_bounded() {
        let d = ShortDir::new();
        let (_v, vault, token) = vault();
        // The server holds the vault weakly: the test keeps it open.
        let server = start(&d, &vault);
        let mut c = Client::connect(&d.socket());
        assert_eq!(c.send_raw("not json")["error"]["code"], "invalid_params");
        assert!(c.call(&token, "info")["result"].is_object());
        let big = format!(
            r#"{{"method":"info","pad":"{}"}}"#,
            "x".repeat(MAX_LINE + 10)
        );
        assert_eq!(c.send_raw(&big)["error"]["code"], "invalid_params");
        c.assert_closed();
        server.stop();
    }

    #[test]
    fn stop_returns_at_once_even_with_a_client_that_never_reads() {
        let d = ShortDir::new();
        let (_v, vault, token) = vault();
        let server = start(&d, &vault);
        let s = UnixStream::connect(d.socket()).unwrap();
        let mut w = s.try_clone().unwrap();
        // Many large answers, never read: the worker blocks in its write.
        let req = json!({ "token": token, "method": "transactions", "params": { "limit": 500 } });
        for _ in 0..20 {
            let _ = writeln!(w, "{req}");
        }
        std::thread::sleep(Duration::from_millis(300));
        let t = std::time::Instant::now();
        server.stop();
        assert!(t.elapsed() < STOP_AT_ONCE, "{:?}", t.elapsed());
        assert!(!d.socket().exists());
        drop(s);
    }

    #[test]
    fn a_live_socket_of_another_instance_is_left_alone() {
        let d = ShortDir::new();
        let (_v, vault, _) = vault();
        let path: String = d.socket().to_string_lossy().into();
        let first = LocalApiServer::start(vault.clone(), path.clone()).unwrap();
        assert!(LocalApiServer::start(vault.clone(), path.clone()).is_err());
        assert!(
            UnixStream::connect(&path).is_ok(),
            "first one still reachable"
        );
        first.stop();
        let second = LocalApiServer::start(vault.clone(), path.clone()).unwrap();
        // The first one stopping (again) or going away must not take the second's file.
        first.stop();
        drop(first);
        assert!(UnixStream::connect(&path).is_ok());
        second.stop();
    }

    #[test]
    fn a_stopped_server_never_unlinks_a_replacement_with_a_reused_inode() {
        for stop_again in [false, true] {
            let d = ShortDir::new();
            let (_v, vault, _) = vault();
            let path: String = d.socket().to_string_lossy().into();
            let mut first = LocalApiServer::start(vault.clone(), path.clone()).unwrap();
            first.stop();
            let second = LocalApiServer::start(vault.clone(), path.clone()).unwrap();
            // Model inode reuse deterministically, independent of the host
            // filesystem's allocation policy. The old server no longer owns
            // this path even when the replacement has the same file identity.
            Arc::get_mut(&mut first).unwrap().file_id = second.file_id;
            if stop_again {
                first.stop();
                assert!(UnixStream::connect(&path).is_ok(), "repeated stop");
            }
            drop(first);
            assert!(UnixStream::connect(&path).is_ok(), "drop after stop");
            second.stop();
        }
    }

    #[test]
    fn a_fifth_connection_is_told_to_wait() {
        let d = ShortDir::new();
        let (_v, vault, token) = vault();
        let server = start(&d, &vault);
        let mut open: Vec<Client> = (0..MAX_CLIENTS)
            .map(|_| Client::connect(&d.socket()))
            .collect();
        for c in &mut open {
            assert!(c.call(&token, "info")["result"].is_object());
        }
        let mut extra = Client::connect(&d.socket());
        assert_eq!(extra.read()["error"]["code"], "busy");
        drop(open);
        std::thread::sleep(Duration::from_millis(300));
        let mut again = Client::connect(&d.socket());
        assert!(again.call(&token, "info")["result"].is_object());
        server.stop();
    }

    #[test]
    fn without_the_token_nothing_tells_whether_access_is_on() {
        let d = ShortDir::new();
        let (_v, vault, token) = vault();
        let server = start(&d, &vault);
        let scope = vault.local_api_scope().unwrap();
        vault
            .set_local_api_scope(ShareScope {
                enabled: false,
                ..scope
            })
            .unwrap();
        let mut c = Client::connect(&d.socket());
        assert_eq!(c.call("", "info")["error"]["code"], "unauthorized");
        // switching off revoked the old key ...
        assert_eq!(c.call(&token, "info")["error"]["code"], "unauthorized");
        // ... a key created while it is off is valid but the interface says "disabled"
        let fresh = vault.create_local_api_token().unwrap();
        assert_eq!(c.call(&fresh, "info")["error"]["code"], "disabled");
        server.stop();
    }

    #[test]
    fn an_existing_open_folder_is_refused_and_left_as_it_is() {
        let d = ShortDir::new();
        let (_v, vault, _) = vault();
        let path = d.make_dir(0o755);
        assert!(LocalApiServer::start(vault, path.to_string_lossy().into()).is_err());
        let mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755, "never chmod-ed");
        assert!(!path.exists());
    }

    #[test]
    fn a_request_that_never_ends_is_cut_off() {
        let d = ShortDir::new();
        let (_v, vault, _) = vault();
        let server = start(&d, &vault);
        let mut c = Client::connect(&d.socket());
        write!(c.writer, "{{").unwrap();
        assert_eq!(c.read()["error"]["code"], "invalid_params");
        c.assert_closed();
        server.stop();
    }

    #[test]
    fn stop_wakes_idle_connections_at_once() {
        let d = ShortDir::new();
        let (_v, vault, token) = vault();
        let server = start(&d, &vault);
        let mut c = Client::connect(&d.socket());
        assert!(c.call(&token, "info")["result"].is_object());
        let t = std::time::Instant::now();
        server.stop();
        c.assert_closed();
        assert!(t.elapsed() < STOP_AT_ONCE, "{:?}", t.elapsed());
    }

    #[test]
    fn a_stale_socket_is_replaced() {
        let d = ShortDir::new();
        let (_v, vault, token) = vault();
        let path = d.make_dir(0o700);
        drop(UnixListener::bind(&path).unwrap()); // left behind by a crash
        let server = start(&d, &vault);
        assert!(Client::connect(&path).call(&token, "info")["result"].is_object());
        server.stop();
    }

    #[test]
    fn a_last_request_without_newline_is_answered() {
        let d = ShortDir::new();
        let (_v, vault, token) = vault();
        let server = start(&d, &vault);
        let mut c = Client::connect(&d.socket());
        let req = json!({ "v": 1, "token": token, "method": "info" });
        write!(c.writer, "{req}").unwrap();
        c.writer.shutdown(std::net::Shutdown::Write).unwrap();
        assert!(c.read()["result"].is_object());
        c.assert_closed();
        server.stop();
    }

    #[test]
    fn missing_folders_are_created_private() {
        let d = ShortDir::new();
        let (_v, vault, _) = vault();
        let server = start(&d, &vault);
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&d.0), 0o700, "a created parent too");
        assert_eq!(mode(d.socket().parent().unwrap()), 0o700);
        assert!(lock_path(&d.socket()).ends_with("s.sock.lock"));
        server.stop();
    }
}
