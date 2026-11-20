//! `girocrab-mcp`: started by an MCP client, talks JSON-RPC over stdin/stdout.
//! Logs go to stderr only (stdout carries the protocol).

use girocrab_mcp::{Adapter, Config, no_token, rpc_error};
use serde_json::Value;
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

const USAGE: &str = "\
girocrab-mcp: MCP server (stdio) for GiroCrab's read-only local interface

Usage: girocrab-mcp [--socket <path>] [--token-file <path>] [--check [--json]]

The access key comes from GiroCrab (Settings › Analysis). Pass it in the
environment as GIROCRAB_API_TOKEN, or as a file with --token-file; never as an
argument (other programs can read the command line).

  --socket <path>      socket of the app (default: GIROCRAB_API_SOCKET, or
                       ~/Library/Application Support/GiroCrab/local-api/girocrab.sock)
  --token-file <path>  file holding the access key (should be readable only by you)
  --check              test the connection and the key, print the result and exit
                       (0 works, 3 not reachable, 4 no key, 5 key not accepted,
                       6 turned off, 7 locked, 8 busy, 1 other)
  --json               with --check: one JSON line {\"problem\": …, \"message\": …}
                       (problem: ok, unreachable, no_key, key_rejected, disabled,
                       locked, busy, other)
  --version            print the version
";

fn main() {
    let (config, check, json_output) = match config(std::env::args().skip(1).collect()) {
        Ok(c) => c,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    if let Err(problem) = &config.token {
        // Still serve: every tool call then tells the user what to set up.
        eprintln!("girocrab-mcp: {problem}");
    }
    let mut adapter = Adapter::new(config);
    if check {
        let (problem, message, code) = match adapter.check() {
            Ok(info) => {
                let accounts = &info["account_count"];
                let message = format!(
                    "OK: GiroCrab answers and accepts the key; shared accounts: {accounts}."
                );
                ("ok", message, 0)
            }
            Err(e) => (e.problem.name(), e.message, e.problem.exit_code()),
        };
        if json_output {
            println!("{}", json!({ "problem": problem, "message": message }));
        } else {
            println!("{message}");
        }
        std::process::exit(code);
    }
    let mut stdin = BufReader::new(std::io::stdin().lock());
    let mut stdout = std::io::stdout().lock();
    let mut line = Vec::new();
    loop {
        // Bytes, not `lines()`: one line that is not UTF-8 gets a parse error
        // instead of ending the adapter.
        line.clear();
        match stdin.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let answer = match serde_json::from_slice::<Value>(&line) {
            Ok(msg) => adapter.handle(msg),
            Err(e) => Some(rpc_error(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        if let Some(answer) = answer
            && writeln!(stdout, "{answer}")
                .and_then(|_| stdout.flush())
                .is_err()
        {
            break; // the client went away
        }
    }
}

/// The configuration, and whether `--check` and `--json` were asked for.
fn config(args: Vec<String>) -> Result<(Config, bool, bool), String> {
    let mut socket = std::env::var_os("GIROCRAB_API_SOCKET").map(PathBuf::from);
    let mut token = std::env::var("GIROCRAB_API_TOKEN").map_err(|_| no_token());
    let (mut check, mut json_output) = (false, false);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => socket = Some(args.next().ok_or("--socket needs a path")?.into()),
            "--token-file" => {
                let path = args.next().ok_or("--token-file needs a path")?;
                token = read_token_file(&path);
            }
            "--check" => check = true,
            "--json" => json_output = true,
            "--version" => {
                println!("girocrab-mcp {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}\n\n{USAGE}")),
        }
    }
    let socket = socket
        .or_else(default_socket)
        .ok_or("no socket path: pass --socket or GIROCRAB_API_SOCKET")?;
    let token = token.and_then(|t| {
        let t = t.trim().to_string();
        if t.is_empty() { Err(no_token()) } else { Ok(t) }
    });
    Ok((Config { socket, token }, check, json_output))
}

/// The key, or a message for the user (the adapter still starts, see `main`).
fn read_token_file(path: &str) -> Result<String, String> {
    use std::os::unix::fs::PermissionsExt;
    let unreadable = |e: std::io::Error| {
        format!(
            "The access key file {path} cannot be read ({e}). Check --token-file in this program's configuration."
        )
    };
    let meta = std::fs::metadata(path).map_err(unreadable)?;
    if meta.permissions().mode() & 0o077 != 0 {
        eprintln!("girocrab-mcp: {path} can be read by other users; chmod 600 it");
    }
    std::fs::read_to_string(path).map_err(unreadable)
}

/// Where the macOS app puts its socket.
fn default_socket() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/GiroCrab/local-api/girocrab.sock"))
}
