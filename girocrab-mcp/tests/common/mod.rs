//! Shared by the integration tests (each test file uses only part of it).
#![allow(dead_code)]

use girocrab_mcp::{Adapter, Config, no_token};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// A fresh, short directory under /tmp (`sun_path` holds only 104 bytes on
/// macOS, the usual temp directory is too deep), removed on drop.
pub struct ShortDir(pub PathBuf);

impl ShortDir {
    pub fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir = PathBuf::from(format!("/tmp/gcm-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    /// Directly in the directory (stand-in apps). The real server is started
    /// one level deeper, in a folder it creates itself with mode 0700.
    pub fn socket(&self) -> PathBuf {
        self.0.join("s.sock")
    }
}

impl Drop for ShortDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn adapter_at(socket: &Path, token: Result<String, String>) -> Adapter {
    Adapter::new(Config {
        socket: socket.to_path_buf(),
        token,
    })
}

/// No app behind it and no key: for everything that never reaches the socket.
pub fn offline() -> Adapter {
    adapter_at(Path::new("/nonexistent/s.sock"), Err(no_token()))
}

pub fn rpc(id: Value, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// `tools/call` and its result.
pub fn call(a: &mut Adapter, tool: &str, arguments: Value) -> Value {
    let msg = rpc(
        json!(7),
        "tools/call",
        json!({ "name": tool, "arguments": arguments }),
    );
    a.handle(msg).unwrap()["result"].clone()
}

pub fn text(result: &Value) -> String {
    result["content"][0]["text"].as_str().unwrap().to_string()
}
