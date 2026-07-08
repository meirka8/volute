use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Read one LSP-framed message: headers up to the blank line, then exactly
/// `Content-Length` body bytes. Returns `None` on EOF or a malformed frame.
fn read_message<R: BufRead>(reader: &mut R) -> Option<Value> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None; // EOF
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed
            .strip_prefix("Content-Length:")
            .or_else(|| trimmed.strip_prefix("content-length:"))
        {
            content_length = value.trim().parse::<usize>().ok();
        }
        // Other headers (e.g. Content-Type) are allowed by the spec; ignore them.
    }

    let mut body = vec![0u8; content_length?];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn send(stdin: &mut ChildStdin, msg: &Value) {
    let body = msg.to_string();
    write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
    stdin.flush().unwrap();
}

/// Block until a message matching `pred` arrives, draining (and discarding)
/// any other notifications in between. Panics on timeout or a closed channel
/// rather than hanging, so a broken protocol fails the test instead of
/// stalling CI.
fn wait_for(rx: &mpsc::Receiver<Value>, timeout: Duration, mut pred: impl FnMut(&Value) -> bool) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            panic!("timed out waiting for expected LSP message");
        }
        match rx.recv_timeout(remaining) {
            Ok(msg) if pred(&msg) => return msg,
            Ok(_) => continue, // not the message we're waiting for; keep draining
            Err(_) => panic!("LSP message channel closed before expected message arrived"),
        }
    }
}

#[test]
fn test_lsp_turn_lifecycle() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cvc-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn cvc-lsp");

    let mut stdin = child.stdin.take().expect("Failed to open stdin");
    let stdout = child.stdout.take().expect("Failed to open stdout");

    // Continuously drain and parse framed messages on a background thread so
    // the child's stdout pipe never backs up. cvc-lsp emits several
    // window/logMessage notifications around this exchange (hook install,
    // "initialized", turn/start, turn/end) in addition to the initialize
    // response -- if nothing reads them, the pipe buffer can fill and block
    // the server's writes, stalling the very DB write this test verifies.
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Some(msg) = read_message(&mut reader) {
            if tx.send(msg).is_err() {
                break;
            }
        }
    });

    // 1. Initialize
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "capabilities": {},
                "rootUri": null,
                "processId": null
            }
        }),
    );

    let init_response = wait_for(&rx, Duration::from_secs(5), |m| m.get("id") == Some(&json!(1)));
    assert!(
        init_response["result"]["capabilities"].is_object(),
        "unexpected initialize response: {:?}",
        init_response
    );

    // 2. Initialized
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }),
    );

    // 3. Turn Start
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "$/cvc/turn/start",
            "params": {
                "id": "turn-1",
                "prompt": "Hello CVC",
                "author": "human"
            }
        }),
    );

    // 4. Turn End
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "$/cvc/turn/end",
            "params": {
                "id": "turn-1",
                "response": "Hello Human",
                "chain_of_thought": "Thinking...",
                "model": "gpt-4"
            }
        }),
    );

    // Wait for the server's own completion signal for the async DB write
    // instead of a fixed sleep, which was both flaky (no guarantee 500ms is
    // enough) and slower than necessary on a fast machine.
    let completion = wait_for(&rx, Duration::from_secs(5), |m| {
        m.get("method") == Some(&json!("window/logMessage"))
            && m["params"]["message"].as_str().is_some_and(|s| {
                s.contains("Interaction saved to DB") || s.contains("Failed to save interaction")
            })
    });
    assert!(
        completion["params"]["message"]
            .as_str()
            .unwrap()
            .contains("Interaction saved to DB"),
        "turn/end did not save the interaction: {:?}",
        completion["params"]["message"]
    );

    // Cleanup
    child.kill().unwrap();
}
