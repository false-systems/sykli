//! `sykli-mcp`: a Model Context Protocol shim beside `sykli`.
//!
//! JSON-RPC 2.0, one message per line, on the stdin/stdout of the one client
//! that spawned it. Each tool runs the `sykli` subcommand a human would run —
//! the binary is found through `SYKLI_BIN` or PATH, never linked — and hands
//! back its stdout as a text block, with `isError` when it exited non-zero.
//! No socket, no state between calls, no lifetime beyond the client's: the
//! loop ends when stdin does. Per ADR-0005 the sykli binary itself carries no
//! server; this is the shim that lives beside it (docs/adr/0008-mcp-adapter.md).

use std::io::{self, BufRead, Write};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

/// The protocol revision this shim speaks; clients negotiate down.
const PROTOCOL_VERSION: &str = "2025-06-18";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!(
            "sykli-mcp — sykli's CLI as MCP tools over stdio\n\n\
             Usage: sykli-mcp\n\n\
             Speaks JSON-RPC 2.0, one message per line, until stdin closes.\n\
             Runs `sykli` from SYKLI_BIN or PATH in the current directory.\n\
             Tools: sykli_validate, sykli_plan, sykli_run, sykli_verify."
        );
        return;
    }
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("sykli-mcp {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if !args.is_empty() {
        eprintln!("sykli-mcp takes no arguments; it is spawned by an MCP client");
        std::process::exit(2);
    }
    if let Err(error) = serve(io::stdin().lock(), io::stdout().lock()) {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

/// Serve until the reader is exhausted. Only I/O on the channel itself is an
/// error; a bad message is answered, never fatal.
fn serve(reader: impl BufRead, mut writer: impl Write) -> Result<(), String> {
    for line in reader.lines() {
        let line = line.map_err(|error| error.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let Some(response) = handle_line(&line) else {
            continue;
        };
        serde_json::to_writer(&mut writer, &response).map_err(|error| error.to_string())?;
        writer.write_all(b"\n").map_err(|error| error.to_string())?;
        writer.flush().map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// One message in, at most one response out; notifications get none.
fn handle_line(line: &str) -> Option<Value> {
    let message: Value = match serde_json::from_str(line) {
        Ok(message) => message,
        Err(error) => {
            return Some(error_response(
                Value::Null,
                PARSE_ERROR,
                &format!("parse error: {error}"),
            ));
        }
    };
    let id = message.get("id").cloned();
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Some(error_response(
            id.unwrap_or(Value::Null),
            INVALID_REQUEST,
            "request has no method",
        ));
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = dispatch(method, &params);
    let id = id?;
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => error_response(id, code, &message),
    })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn dispatch(method: &str, params: &Value) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "sykli-mcp", "version": env!("CARGO_PKG_VERSION") },
            "instructions": "Ask sykli_plan what applies before editing; run sykli_run after; \
                             hand the receipt to the reviewer and let sykli_verify judge it. \
                             A cached task outcome is reuse of an earlier receipt, not an observation.",
        })),
        "notifications/initialized" | "notifications/cancelled" => Ok(Value::Null),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => call(params),
        other => Err((METHOD_NOT_FOUND, format!("unknown method {other:?}"))),
    }
}

fn tools() -> Vec<Value> {
    let contract = json!({
        "type": "string",
        "description": "Path to sykli.rs or a sykli-contract.v1 JSON file, relative to the working directory",
        "default": "sykli.json",
    });
    vec![
        json!({
            "name": "sykli_validate",
            "description": "Validate a contract without executing it: `sykli validate <contract> --json`, a sykli-validate.v1 verdict with the contract hash. isError when invalid.",
            "inputSchema": { "type": "object", "properties": { "contract": contract } },
        }),
        json!({
            "name": "sykli_plan",
            "description": "Which tasks the changed files affect, in execution order: `sykli plan <contract> --changed … --json`, a sykli-plan.v1. No changed files means the whole graph.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "contract": contract,
                    "changed": { "type": "array", "items": { "type": "string" }, "description": "Changed file paths, relative to the working directory" },
                },
            },
        }),
        json!({
            "name": "sykli_run",
            "description": "Evaluate the graph: `sykli run <contract> --json`. Runs what is not cached, writes the receipt under .sykli/receipts, returns it (sykli-receipt.v1). isError unless the outcome is passed or cached. Cached tasks are reuse, not observation.",
            "inputSchema": { "type": "object", "properties": { "contract": contract } },
        }),
        json!({
            "name": "sykli_verify",
            "description": "Check a receipt against the current tree and contract: `sykli verify <receipt> --contract <contract>`. Text is the per-check report; the second block names the exit code: 0 verified, 1 outcome failed, 2 cannot verify, 3 stale tree (re-run), 4 contract drift (re-lock).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "receipt": { "type": "string", "description": "Path to a sykli-receipt.v1 JSON file" },
                    "contract": contract,
                },
                "required": ["receipt"],
            },
        }),
    ]
}

fn call(params: &Value) -> Result<Value, (i64, String)> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or((INVALID_PARAMS, "tools/call needs a tool name".to_string()))?;
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
    let contract = arguments
        .get("contract")
        .and_then(Value::as_str)
        .unwrap_or("sykli.json")
        .to_string();
    let mut argv: Vec<String> = match name {
        "sykli_validate" => vec!["validate".into(), contract, "--json".into()],
        "sykli_plan" => {
            let mut argv = vec!["plan".into(), contract];
            for path in arguments
                .get("changed")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                argv.push("--changed".into());
                argv.push(path.into());
            }
            argv.push("--json".into());
            argv
        }
        "sykli_run" => vec!["run".into(), contract, "--json".into()],
        "sykli_verify" => {
            let receipt = arguments.get("receipt").and_then(Value::as_str).ok_or((
                INVALID_PARAMS,
                "sykli_verify needs a receipt path".to_string(),
            ))?;
            vec![
                "verify".into(),
                receipt.into(),
                "--contract".into(),
                contract,
            ]
        }
        other => return Err((INVALID_PARAMS, format!("unknown tool {other:?}"))),
    };
    let program = std::env::var("SYKLI_BIN").unwrap_or_else(|_| "sykli".into());
    let output = Command::new(&program)
        .args(argv.drain(..))
        .stdin(Stdio::null())
        .output()
        .map_err(|error| (INVALID_PARAMS, format!("cannot run {program}: {error}")))?;
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let mut content = vec![json!({ "type": "text", "text": stdout })];
    if name == "sykli_verify" {
        content.push(json!({ "type": "text", "text": format!("exit code {code}: {}", verify_meaning(code)) }));
    }
    if code != 0 && !stderr.is_empty() {
        content.push(json!({ "type": "text", "text": format!("stderr:\n{stderr}") }));
    }
    Ok(json!({ "content": content, "isError": code != 0 }))
}

/// The staged meanings `sykli verify --help` documents.
fn verify_meaning(code: i32) -> &'static str {
    match code {
        0 => "verified — receipt matches this tree and contract",
        1 => "outcome or evidence failed — the work is bad or incomplete",
        2 => "cannot verify — not a receipt, unreadable input, git or contract error",
        3 => "tree or input mismatch — receipt is stale; re-run sykli",
        4 => "contract mismatch — contract drifted from the receipt; re-lock",
        _ => "unknown exit code",
    }
}
