//! Drive the real `sykli-mcp` over stdin/stdout, with the real `sykli` it
//! shells out to. Std only: the shim's dependency list is a boundary.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The `sykli` binary from the same target directory as the shim, built on
/// demand so `cargo test -p sykli-mcp` works alone.
fn sykli_binary() -> PathBuf {
    let shim = Path::new(env!("CARGO_BIN_EXE_sykli-mcp"));
    let sykli = shim.with_file_name("sykli");
    if !sykli.is_file() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args(["build", "-p", "sykli"])
            .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
            .status()
            .expect("cargo runs");
        assert!(status.success(), "building sykli for the shim tests");
    }
    sykli
}

fn temp_dir(tag: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("sykli-mcp-{tag}-{nonce}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Send every line, close stdin, collect responses keyed by id.
fn session(cwd: &Path, requests: &[serde_json::Value]) -> (Vec<serde_json::Value>, Option<i32>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sykli-mcp"))
        .current_dir(cwd)
        .env("SYKLI_BIN", sykli_binary())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("shim runs");
    {
        let mut stdin = child.stdin.take().unwrap();
        for request in requests {
            writeln!(stdin, "{request}").unwrap();
        }
    }
    let output = child.wait_with_output().unwrap();
    let responses = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
        .collect();
    (responses, output.status.code())
}

fn by_id(responses: &[serde_json::Value], id: u64) -> &serde_json::Value {
    responses
        .iter()
        .find(|response| response["id"] == id)
        .unwrap_or_else(|| panic!("no response with id {id} in {responses:?}"))
}

fn text(result: &serde_json::Value) -> serde_json::Value {
    serde_json::from_str(result["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[test]
fn a_session_negotiates_lists_tools_calls_them_and_exits_with_stdin() {
    let dir = temp_dir("session");
    std::fs::write(
        dir.join("sykli.json"),
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"hello","run":"printf hi","inputs":["sykli.json"]},{"name":"after","run":"true","after":["hello"]}]}"#,
    )
    .unwrap();
    let (responses, code) = session(
        &dir,
        &[
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}),
            serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"sykli_validate","arguments":{"contract":"sykli.json"}}}),
            serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"sykli_plan","arguments":{"contract":"sykli.json","changed":["sykli.json"]}}}),
            serde_json::json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"no_such_tool"}}),
            serde_json::json!({"jsonrpc":"2.0","id":6,"method":"ping"}),
            serde_json::json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"sykli_validate","arguments":{"contract":"missing.json"}}}),
        ],
    );
    std::fs::remove_dir_all(&dir).unwrap();

    assert_eq!(code, Some(0), "exits 0 when stdin closes");
    // The notification produced no response: 8 lines in, 7 out.
    assert_eq!(responses.len(), 7, "{responses:?}");

    let init = by_id(&responses, 1);
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "sykli-mcp");
    assert!(init["result"]["capabilities"]["tools"].is_object());

    let tools: Vec<&str> = by_id(&responses, 2)["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        tools,
        ["sykli_validate", "sykli_plan", "sykli_run", "sykli_verify"]
    );

    let validate = by_id(&responses, 3);
    assert_eq!(validate["result"]["isError"], false);
    let verdict = text(validate);
    assert_eq!(verdict["schema"], "sykli-validate.v1");
    assert_eq!(verdict["valid"], true);
    assert_eq!(verdict["contract_hash"].as_str().unwrap().len(), 64);

    let plan = text(by_id(&responses, 4));
    assert_eq!(plan["schema"], "sykli-plan.v1");
    assert_eq!(plan["tasks"], serde_json::json!(["hello", "after"]));

    let unknown = by_id(&responses, 5);
    assert_eq!(unknown["error"]["code"], -32602);
    assert!(unknown.get("result").is_none());

    assert_eq!(by_id(&responses, 6)["result"], serde_json::json!({}));

    // A non-zero sykli exit is a tool error, still with the verdict as text.
    let invalid = by_id(&responses, 7);
    assert_eq!(invalid["result"]["isError"], true);
    assert_eq!(text(invalid)["valid"], false);
}

#[test]
fn malformed_input_is_answered_not_fatal() {
    let dir = temp_dir("malformed");
    let (responses, code) = session(
        &dir,
        &[
            serde_json::json!({"jsonrpc":"2.0","id":9,"method":"nothing/here"}),
            serde_json::json!({"jsonrpc":"2.0","id":10}),
        ],
    );
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(code, Some(0));
    assert_eq!(by_id(&responses, 9)["error"]["code"], -32601);
    assert_eq!(by_id(&responses, 10)["error"]["code"], -32600);
}
