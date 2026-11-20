//! The protocol side of the adapter against a stand-in for the app, and the
//! built binary over stdio.

mod common;

use common::{ShortDir, adapter_at, call, offline, rpc, text};
use girocrab_mcp::{Adapter, PROTOCOL_VERSIONS};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};

/// Stands in for the app: answers each request line with the raw line
/// `answer(request)`; `None` closes the connection without an answer.
/// Records every request.
fn fake_app(
    dir: &ShortDir,
    answer: impl Fn(&Value) -> Option<String> + Send + 'static,
) -> Arc<Mutex<Vec<Value>>> {
    let listener = UnixListener::bind(dir.socket()).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let mut out = stream.try_clone().unwrap();
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                let req: Value = serde_json::from_str(&line).unwrap();
                log.lock().unwrap().push(req.clone());
                match answer(&req) {
                    Some(a) => writeln!(out, "{a}").unwrap(),
                    None => break,
                }
            }
        }
    });
    seen
}

fn adapter(dir: &ShortDir) -> Adapter {
    adapter_at(&dir.socket(), Ok("gcapi_test".into()))
}

fn initialize(a: &mut Adapter, version: &str) -> Value {
    let params = json!({ "protocolVersion": version, "capabilities": {},
                         "clientInfo": { "name": "t", "version": "1" } });
    a.handle(rpc(json!(1), "initialize", params)).unwrap()
}

#[test]
fn initialize_answers_the_asked_version_or_the_newest() {
    let mut a = offline();
    let r = initialize(&mut a, "2025-03-26");
    assert_eq!(r["jsonrpc"], "2.0");
    assert_eq!(r["id"], 1);
    assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(r["result"]["capabilities"]["tools"]["listChanged"], false);
    assert_eq!(r["result"]["serverInfo"]["name"], "girocrab-mcp");
    assert_eq!(
        initialize(&mut a, "1999-01-01")["result"]["protocolVersion"],
        PROTOCOL_VERSIONS[0]
    );
}

#[test]
fn notifications_get_no_answer_and_unknown_requests_an_error() {
    let mut a = offline();
    let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    assert_eq!(a.handle(note), None);
    let r = a
        .handle(rpc(json!("x"), "resources/list", json!({})))
        .unwrap();
    assert_eq!(r["error"]["code"], -32601);
    assert_eq!(r["id"], "x");
    let r = a.handle(json!([rpc(json!(1), "ping", json!({}))])).unwrap();
    assert_eq!(r["error"]["code"], -32600, "no batches");
    let r = a.handle(rpc(json!(2), "ping", json!({}))).unwrap();
    assert_eq!(r["result"], json!({}));
}

#[test]
fn responses_from_the_client_get_no_answer() {
    let mut a = offline();
    assert_eq!(
        a.handle(json!({ "jsonrpc": "2.0", "id": 5, "result": {} })),
        None
    );
    let error = json!({ "jsonrpc": "2.0", "id": 6, "error": { "code": 1, "message": "x" } });
    assert_eq!(a.handle(error), None);
    let r = a.handle(json!({ "jsonrpc": "2.0", "id": 7 })).unwrap();
    assert_eq!(r["error"]["code"], -32600, "neither request nor response");
}

#[test]
fn tools_are_listed_read_only_with_object_schemas() {
    let r = offline()
        .handle(rpc(json!(1), "tools/list", json!({})))
        .unwrap();
    let tools = r["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "get_info",
            "list_accounts",
            "list_categories",
            "list_transactions",
            "get_summary"
        ]
    );
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
        assert_eq!(t["annotations"]["readOnlyHint"], true);
        assert!(!t["description"].as_str().unwrap().is_empty());
    }
}

#[test]
fn an_unknown_tool_is_a_protocol_error() {
    let r = offline()
        .handle(rpc(json!(3), "tools/call", json!({ "name": "pay" })))
        .unwrap();
    assert_eq!(r["error"]["code"], -32602);
}

#[test]
fn a_call_forwards_token_method_and_arguments() {
    let d = ShortDir::new();
    let seen = fake_app(&d, |_| {
        Some(r#"{"v":1,"result":{"transactions":[],"next_cursor":null}}"#.into())
    });
    let r = call(
        &mut adapter(&d),
        "list_transactions",
        json!({ "limit": 5, "direction": "income" }),
    );
    assert_eq!(r["isError"], false);
    assert_eq!(r["structuredContent"]["transactions"], json!([]));
    assert_eq!(
        text(&r),
        r#"{"next_cursor":null,"transactions":[]}"#,
        "compact text"
    );
    let req = seen.lock().unwrap()[0].clone();
    assert_eq!(
        req,
        json!({ "v": 1, "token": "gcapi_test", "method": "transactions",
                "params": { "limit": 5, "direction": "income" } })
    );
}

#[test]
fn lists_are_wrapped_for_structured_content() {
    let d = ShortDir::new();
    fake_app(&d, |_| Some(r#"{"v":1,"result":[{"id":1}]}"#.into()));
    let r = call(&mut adapter(&d), "list_accounts", Value::Null);
    assert_eq!(r["structuredContent"], json!({ "items": [{ "id": 1 }] }));
}

#[test]
fn structured_content_only_from_2025_06_18() {
    let d = ShortDir::new();
    fake_app(&d, |_| Some(r#"{"v":1,"result":{"a":1}}"#.into()));
    for (version, structured) in [
        ("2025-06-18", true),
        ("2025-03-26", false),
        ("2024-11-05", false),
    ] {
        let mut a = adapter(&d);
        initialize(&mut a, version);
        let r = call(&mut a, "get_info", json!({}));
        assert_eq!(r["structuredContent"].is_object(), structured, "{version}");
        assert_eq!(text(&r), r#"{"a":1}"#);
    }
}

#[test]
fn refusals_become_readable_tool_errors() {
    for (code, expect) in [
        ("locked", "locked"),
        ("disabled", "turned off"),
        ("unauthorized", "access key"),
        ("not_shared", "not_shared: nope"),
    ] {
        let d = ShortDir::new();
        let answer = format!(r#"{{"v":1,"error":{{"code":"{code}","message":"nope"}}}}"#);
        fake_app(&d, move |_| Some(answer.clone()));
        let r = call(&mut adapter(&d), "get_info", json!({}));
        assert_eq!(r["isError"], true, "{code}");
        assert!(text(&r).contains(expect), "{code}: {}", text(&r));
    }
}

#[test]
fn without_key_or_app_the_user_is_told_what_to_do() {
    assert!(text(&call(&mut offline(), "get_info", json!({}))).contains("GIROCRAB_API_TOKEN"));
    let mut a = adapter_at(std::path::Path::new("/nonexistent/s.sock"), Ok("k".into()));
    let r = call(&mut a, "get_info", json!({}));
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("not reachable"), "{}", text(&r));
}

#[test]
fn a_long_socket_path_is_explained() {
    let long = format!("/tmp/{}/s.sock", "x".repeat(120));
    let r = call(
        &mut adapter_at(long.as_ref(), Ok("k".into())),
        "get_info",
        json!({}),
    );
    assert!(text(&r).contains("too long"), "{}", text(&r));
}

#[test]
fn a_request_the_app_drops_is_asked_again() {
    let d = ShortDir::new();
    // Every second request: closed without an answer.
    let n = Arc::new(Mutex::new(0));
    let seen = fake_app(&d, move |_| {
        let mut n = n.lock().unwrap();
        *n += 1;
        (*n % 2 == 1).then(|| format!(r#"{{"v":1,"result":{{"ok":{n}}}}}"#))
    });
    let mut a = adapter(&d);
    assert_eq!(a.request("info", json!({})).unwrap(), json!({ "ok": 1 }));
    assert_eq!(a.request("info", json!({})).unwrap(), json!({ "ok": 3 }));
    assert_eq!(
        seen.lock().unwrap().len(),
        3,
        "the dropped request was sent again"
    );
}

#[test]
fn an_unreadable_answer_is_reported_and_not_asked_again() {
    let d = ShortDir::new();
    let seen = fake_app(&d, |_| Some("not json".into()));
    let r = call(&mut adapter(&d), "get_info", json!({}));
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("unreadable answer"), "{}", text(&r));
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn an_answer_that_is_not_utf8_is_reported_and_not_asked_again() {
    let d = ShortDir::new();
    let listener = UnixListener::bind(d.socket()).unwrap();
    let requests = Arc::new(Mutex::new(0));
    let count = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let mut out = stream.try_clone().unwrap();
            for _ in BufReader::new(stream).lines() {
                *count.lock().unwrap() += 1;
                out.write_all(b"{\"v\":1,\"result\":\"B\xe4cker\"}\n")
                    .unwrap();
            }
        }
    });
    let r = call(&mut adapter(&d), "get_info", json!({}));
    assert!(text(&r).contains("unreadable answer"), "{}", text(&r));
    assert_eq!(*requests.lock().unwrap(), 1);
}

/// Like the app at its connection limit: answers `busy` right on connect,
/// without reading the request, and closes.
fn busy_app(dir: &ShortDir, busy_connections: usize) -> Arc<Mutex<usize>> {
    let listener = UnixListener::bind(dir.socket()).unwrap();
    let served = Arc::new(Mutex::new(0));
    let count = served.clone();
    std::thread::spawn(move || {
        for (n, stream) in listener.incoming().enumerate() {
            let Ok(mut stream) = stream else { return };
            if n < busy_connections {
                let busy = r#"{"v":1,"error":{"code":"busy","message":"too many"}}"#;
                let _ = writeln!(stream, "{busy}");
                continue; // dropped: closed without reading
            }
            *count.lock().unwrap() += 1;
            let mut out = stream.try_clone().unwrap();
            for _ in BufReader::new(stream).lines() {
                writeln!(out, r#"{{"v":1,"result":{{}}}}"#).unwrap();
            }
        }
    });
    served
}

#[test]
fn busy_is_waited_out() {
    let d = ShortDir::new();
    let served = busy_app(&d, 2);
    assert_eq!(adapter(&d).request("info", json!({})).unwrap(), json!({}));
    assert_eq!(*served.lock().unwrap(), 1);
}

#[test]
fn busy_for_too_long_is_told_as_busy() {
    let d = ShortDir::new();
    busy_app(&d, 100);
    let r = call(&mut adapter(&d), "get_info", json!({}));
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("busy"), "{}", text(&r));
}

/// The built binary, as an MCP client starts it.
fn run_binary(args: &[&str], env_token: Option<&str>, input: &[u8]) -> (Vec<Value>, String) {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_girocrab-mcp"));
    cmd.args(args)
        .env("GIROCRAB_API_SOCKET", "/tmp/gcm-no-such.sock")
        .env_remove("GIROCRAB_API_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(t) = env_token {
        cmd.env("GIROCRAB_API_TOKEN", t);
    }
    let mut child = cmd.spawn().unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    let answers = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    (answers, String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn a_line_that_is_not_utf8_does_not_end_the_adapter() {
    // A search term in Latin-1: "B\xe4cker" is no UTF-8.
    let mut input = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_transactions","arguments":{"search":"B"#.to_vec();
    input.extend_from_slice(b"\xe4cker\"}}}\n");
    input.extend_from_slice(format!("{}\n", rpc(json!(2), "ping", json!({}))).as_bytes());
    let (answers, _) = run_binary(&[], Some("k"), &input);
    assert_eq!(answers.len(), 2);
    assert_eq!(answers[0]["error"]["code"], -32700);
    assert_eq!(answers[1]["id"], 2);
    assert_eq!(answers[1]["result"], json!({}));
}

#[test]
fn an_unreadable_key_file_is_explained_on_every_call() {
    let input = format!(
        "{}\n",
        rpc(json!(1), "tools/call", json!({ "name": "get_info" }))
    );
    let (answers, stderr) = run_binary(
        &["--token-file", "/tmp/gcm-no-such-key"],
        None,
        input.as_bytes(),
    );
    let result = &answers[0]["result"];
    assert_eq!(result["isError"], true);
    assert!(text(result).contains("cannot be read"), "{}", text(result));
    assert!(stderr.contains("cannot be read"), "{stderr}");
}
