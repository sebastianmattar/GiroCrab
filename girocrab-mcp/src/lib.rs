//! MCP adapter for GiroCrab's read-only local interface (#136, PoC part 3).
//!
//! An MCP client starts `girocrab-mcp` and speaks JSON-RPC 2.0 over stdio, one
//! message per line. Each tool call becomes one request on the socket the app
//! serves (`docs/local-api.md`). This program never opens the vault: it only
//! forwards, and whatever the socket refuses (locked, turned off, wrong key)
//! comes back as a tool error the assistant can explain to the user.
//!
//! Only the subset of MCP a read-only tool server needs: `initialize`, `ping`,
//! `tools/list`, `tools/call`; notifications are accepted and ignored.

use serde_json::{Map, Value, json};
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// MCP revisions this adapter speaks, newest first. A client asking for one of
/// them gets it; any other request is answered with the newest.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// From this MCP revision on, results also carry `structuredContent` (MCP
/// revisions are ISO dates, so they compare as text).
const STRUCTURED_FROM: &str = "2025-06-18";

/// Version of the socket protocol (`"v"` of every request); pinned to the
/// core's by a test.
pub const API_VERSION: u32 = 1;
/// `busy` or no answer (app at its connection limit, closing): tries after
/// the first. Every call is read-only, so asking again is harmless.
const RETRIES: u32 = 3;
const RETRY_WAIT: Duration = Duration::from_millis(250);
/// One request on the socket, also while the app paces requests.
const IO_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Config {
    pub socket: PathBuf,
    /// The access key from GiroCrab (Settings › Analysis), or why there is none
    /// (told to the user on every tool call instead of refusing to start).
    pub token: Result<String, String>,
}

/// The message for a missing key.
pub fn no_token() -> String {
    format!(
        "No access key configured. Create one in GiroCrab ({SETTINGS}) and set \
         GIROCRAB_API_TOKEN (or --token-file) in this program's configuration."
    )
}

/// One MCP tool and the socket method behind it.
struct Tool {
    name: &'static str,
    method: &'static str,
    description: &'static str,
    schema: fn() -> Value,
}

const TOOLS: &[Tool] = &[
    Tool {
        name: "get_info",
        method: "info",
        description: "What GiroCrab shares: API version, number of shared accounts, which \
            text fields are shared, and the time of the last bank sync (Unix seconds).",
        schema: no_arguments,
    },
    Tool {
        name: "list_accounts",
        method: "accounts",
        description: "The shared accounts with name, bank, kind, currency, balance in cents \
            (balance_cents) and its date, and the last sync. IBAN and account number only \
            when the user shares them.",
        schema: no_arguments,
    },
    Tool {
        name: "list_categories",
        method: "categories",
        description: "The category tree: id, parent_id, name, full path (\"Housing › Rent\") \
            and depth. Bookings refer to categories by id.",
        schema: no_arguments,
    },
    Tool {
        name: "list_transactions",
        method: "transactions",
        description: "Bookings of the shared accounts, newest first. Amounts in cents \
            (amount_cents, negative = outgoing), dates as YYYY-MM-DD, stable ids. Pass \
            next_cursor back as cursor for the next page; no next_cursor = last page. Text \
            fields the user does not share are missing; IBANs and long numbers in texts are \
            masked unless account numbers are shared. search needs payees, purposes and \
            account numbers to be shared.",
        schema: transaction_schema,
    },
    Tool {
        name: "get_summary",
        method: "summary",
        description: "Totals for a date range of at most three years, calculated by GiroCrab \
            (amounts in cents). totals: income and expenses per currency like the app's \
            tiles (transfers between own accounts are neutral, refunds reduce expenses). \
            by_category: net sum per category like the app's category list (transfers and \
            loan payments included).",
        schema: summary_schema,
    },
];

fn no_arguments() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn date(description: &str) -> Value {
    json!({ "type": "string", "pattern": "^\\d{4}-\\d{2}-\\d{2}$", "description": description })
}

fn account_ids() -> Value {
    json!({
        "type": "array", "items": { "type": "integer" },
        "description": "Only these shared accounts (default: all shared accounts)"
    })
}

fn transaction_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "account_ids": account_ids(),
            "date_from": date("First booking date, inclusive"),
            "date_to": date("Last booking date, inclusive"),
            "category_id": { "type": "integer", "description": "Only this category" },
            "include_subcategories": {
                "type": "boolean", "description": "With category_id: also its subcategories (default true)"
            },
            "uncategorized_only": { "type": "boolean", "description": "Only bookings without a category" },
            "direction": { "type": "string", "enum": ["income", "expense", "all"] },
            "search": { "type": "string", "description": "Text in payee, purpose, booking text, IBANs or account numbers" },
            "cursor": { "type": "string", "description": "next_cursor of the previous page" },
            "limit": { "type": "integer", "minimum": 1, "maximum": 500, "description": "Page size (default 100)" }
        },
        "additionalProperties": false
    })
}

fn summary_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "date_from": date("First day, inclusive"),
            "date_to": date("Last day, inclusive (at most three years after date_from)"),
            "account_ids": account_ids()
        },
        "required": ["date_from", "date_to"],
        "additionalProperties": false
    })
}

/// What went wrong, for `girocrab-mcp --check` and the app's connection test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// No socket: the app is not running, locked, or the interface is off.
    Unreachable,
    NoKey,
    KeyRejected,
    Disabled,
    Locked,
    Busy,
    Other,
}

impl Problem {
    /// Exit code of `--check` for shells (0 = works, 2 = wrong arguments).
    pub fn exit_code(self) -> i32 {
        match self {
            Problem::Other => 1,
            Problem::Unreachable => 3,
            Problem::NoKey => 4,
            Problem::KeyRejected => 5,
            Problem::Disabled => 6,
            Problem::Locked => 7,
            Problem::Busy => 8,
        }
    }

    /// Name in `--check --json` (`"problem"`), what the app's settings pane reads.
    pub fn name(self) -> &'static str {
        match self {
            Problem::Unreachable => "unreachable",
            Problem::NoKey => "no_key",
            Problem::KeyRejected => "key_rejected",
            Problem::Disabled => "disabled",
            Problem::Locked => "locked",
            Problem::Busy => "busy",
            Problem::Other => "other",
        }
    }
}

/// A failed tool call, worded for the user; sent as a tool result with `isError`.
#[derive(Debug, PartialEq)]
pub struct ToolError {
    pub problem: Problem,
    pub message: String,
}

impl ToolError {
    fn new(problem: Problem, message: impl Into<String>) -> Self {
        Self {
            problem,
            message: message.into(),
        }
    }

    fn other(message: impl Into<String>) -> Self {
        Self::new(Problem::Other, message)
    }
}

const SETTINGS: &str = "Settings › Analysis, German: Einstellungen › Analyse";
const UNREADABLE: &str = "GiroCrab sent an unreadable answer";

/// The socket answer of a refused request, as something the user can act on.
fn api_error(code: &str, message: &str) -> ToolError {
    let (problem, text) = match code {
        "locked" => (
            Problem::Locked,
            "GiroCrab is locked. Unlock it to continue.".into(),
        ),
        "disabled" => (
            Problem::Disabled,
            format!("External analysis is turned off in GiroCrab ({SETTINGS})."),
        ),
        "unauthorized" => (
            Problem::KeyRejected,
            format!(
                "GiroCrab did not accept the access key. Create a new key in GiroCrab \
                 ({SETTINGS}) and update this program's configuration."
            ),
        ),
        "busy" => (
            Problem::Busy,
            "GiroCrab is busy with other programs. Try again shortly.".into(),
        ),
        _ => (Problem::Other, format!("{code}: {message}")),
    };
    ToolError::new(problem, text)
}

/// Why one exchange brought no answer.
enum NoAnswer {
    /// Closed or reset before an answer line: worth another try.
    Retry,
    /// Told to the user as it is (cannot connect, timeout, unreadable answer).
    Fail(ToolError),
}

fn connect_error(socket: &Path, e: std::io::Error) -> ToolError {
    let socket = socket.display();
    let (problem, text) = match e.kind() {
        ErrorKind::NotFound | ErrorKind::ConnectionRefused => (
            Problem::Unreachable,
            format!(
                "GiroCrab is not reachable: it is not running, the vault is locked, or \
                 external analysis is turned off ({SETTINGS}). Socket: {socket}"
            ),
        ),
        ErrorKind::PermissionDenied => (
            Problem::Other,
            format!(
                "GiroCrab's socket cannot be opened (permission denied): it belongs to \
                 another user, or --socket points to the wrong place. Socket: {socket}"
            ),
        ),
        // sun_path holds 104 bytes on macOS (108 on Linux).
        ErrorKind::InvalidInput => (
            Problem::Other,
            format!("The socket path is too long for a Unix socket: {socket}"),
        ),
        _ => (
            Problem::Other,
            format!("Cannot connect to GiroCrab ({socket}): {e}"),
        ),
    };
    ToolError::new(problem, text)
}

/// One request on a fresh connection: the protocol keeps no state on a
/// connection, and the app closes idle ones anyway. The answer is read even
/// when the write failed, because the app answers `busy` without reading.
fn exchange(socket: &Path, line: &str) -> Result<Value, NoAnswer> {
    let stream =
        UnixStream::connect(socket).map_err(|e| NoAnswer::Fail(connect_error(socket, e)))?;
    // Fails (EINVAL on macOS) only on a connection the app already closed:
    // the read below then returns at once.
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let _ = writeln!(&stream, "{line}");
    let mut answer = String::new();
    let fail = |text: String| NoAnswer::Fail(ToolError::other(text));
    match BufReader::new(&stream).read_line(&mut answer) {
        Ok(n) if n > 0 => {
            serde_json::from_str(&answer).map_err(|e| fail(format!("{UNREADABLE}: {e}")))
        }
        Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
            Err(fail(format!(
                "GiroCrab did not answer within {} seconds.",
                IO_TIMEOUT.as_secs()
            )))
        }
        // An answer that is not UTF-8 arrived; asking again would not help.
        Err(e) if e.kind() == ErrorKind::InvalidData => Err(fail(format!("{UNREADABLE}: {e}"))),
        // EOF or reset before an answer line.
        _ => Err(NoAnswer::Retry),
    }
}

pub struct Adapter {
    config: Config,
    /// Negotiated in `initialize`: results carry `structuredContent`.
    structured: bool,
}

impl Adapter {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            structured: true,
        }
    }

    /// Answers one JSON-RPC message; None for notifications and responses.
    pub fn handle(&mut self, msg: Value) -> Option<Value> {
        let Value::Object(mut msg) = msg else {
            // Batches were dropped from MCP in 2025-06-18.
            return Some(rpc_error(
                Value::Null,
                -32600,
                "expected one JSON-RPC object",
            ));
        };
        let id = msg.remove("id");
        let Some(method) = msg.get("method").and_then(Value::as_str).map(str::to_owned) else {
            // A response (result or error) is never answered; we send no
            // requests, so it is dropped. Anything else is no valid message.
            if msg.contains_key("result") || msg.contains_key("error") {
                return None;
            }
            return id.map(|id| rpc_error(id, -32600, "invalid request"));
        };
        let id = id?; // notifications (initialized, cancelled, …) need no answer
        let params = msg.remove("params").unwrap_or(Value::Null);
        Some(match method.as_str() {
            "initialize" => {
                let (result, version) = initialize(&params);
                self.structured = version >= STRUCTURED_FROM;
                rpc_result(id, result)
            }
            "ping" => rpc_result(id, json!({})),
            "tools/list" => rpc_result(id, tools_list()),
            "tools/call" => match self.call_tool(params) {
                Ok(result) => rpc_result(id, result),
                Err((code, message)) => rpc_error(id, code, &message),
            },
            _ => rpc_error(id, -32601, &format!("method not found: {method}")),
        })
    }

    /// A tool result (also for refused requests); Err only for a call that
    /// names no known tool.
    fn call_tool(&mut self, params: Value) -> Result<Value, (i64, String)> {
        let Value::Object(mut params) = params else {
            return Err((-32602, "tools/call needs params".into()));
        };
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let tool = TOOLS
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| (-32602, format!("unknown tool: {name}")))?;
        let arguments = match params.remove("arguments") {
            None | Some(Value::Null) => Value::Object(Map::new()),
            Some(a) => a,
        };
        Ok(match self.request(tool.method, arguments) {
            Ok(result) => {
                // Compact: the text block is what most clients hand to the model.
                let mut out = tool_result(result.to_string(), false);
                if self.structured {
                    // Lists are wrapped: structured content must be an object.
                    out["structuredContent"] = match result {
                        Value::Object(_) => result,
                        other => json!({ "items": other }),
                    };
                }
                out
            }
            Err(e) => tool_result(e.message, true),
        })
    }

    /// One request on the socket; `busy` and a dropped connection are tried
    /// again after a short wait.
    pub fn request(&mut self, method: &str, params: Value) -> Result<Value, ToolError> {
        let token = self
            .config
            .token
            .clone()
            .map_err(|e| ToolError::new(Problem::NoKey, e))?;
        self.request_with(&token, method, params)
    }

    /// `--check`: the info call with the configured key. Without one, the app
    /// is still asked (with no key, which it always refuses), so the answer
    /// tells whether it is reachable.
    pub fn check(&mut self) -> Result<Value, ToolError> {
        let Err(missing) = &self.config.token else {
            return self.request("info", json!({}));
        };
        let reachable = format!("GiroCrab is reachable. {missing}");
        match self.request_with("", "info", json!({})) {
            Err(e) if e.problem == Problem::KeyRejected => {
                Err(ToolError::new(Problem::NoKey, reachable))
            }
            other => other,
        }
    }

    fn request_with(
        &mut self,
        token: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, ToolError> {
        let line = json!({ "v": API_VERSION, "token": token, "method": method, "params": params })
            .to_string();
        let mut retries = RETRIES;
        loop {
            let refused = match exchange(&self.config.socket, &line) {
                Ok(Value::Object(mut answer)) => match answer.remove("error") {
                    None => return Ok(answer.remove("result").unwrap_or(Value::Null)),
                    Some(err) => {
                        let code = err
                            .get("code")
                            .and_then(Value::as_str)
                            .unwrap_or("internal");
                        let message = err.get("message").and_then(Value::as_str).unwrap_or("");
                        let error = api_error(code, message);
                        if code != "busy" {
                            return Err(error);
                        }
                        error
                    }
                },
                Ok(_) => return Err(ToolError::other(format!("{UNREADABLE}."))),
                Err(NoAnswer::Fail(e)) => return Err(e),
                Err(NoAnswer::Retry) => {
                    ToolError::other("GiroCrab closed the connection without an answer.")
                }
            };
            if retries == 0 {
                return Err(refused);
            }
            retries -= 1;
            std::thread::sleep(RETRY_WAIT);
        }
    }
}

fn tool_result(text: String, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

/// The answer to `initialize` and the MCP revision it settles on.
fn initialize(params: &Value) -> (Value, &'static str) {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = PROTOCOL_VERSIONS
        .iter()
        .find(|v| Some(**v) == asked)
        .copied()
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    let result = json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "girocrab-mcp", "title": "GiroCrab", "version": env!("CARGO_PKG_VERSION") },
        "instructions": "Read-only access to the accounts, bookings, categories and totals the \
            user shares in GiroCrab. Nothing can be changed or paid. Amounts are in cents. \
            If a tool reports that GiroCrab is locked or not reachable, ask the user to \
            unlock or start GiroCrab."
    });
    (result, version)
}

fn tools_list() -> Value {
    let tools: Vec<Value> = TOOLS
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": (t.schema)(),
                "annotations": { "readOnlyHint": true, "openWorldHint": false }
            })
        })
        .collect();
    json!({ "tools": tools })
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}
