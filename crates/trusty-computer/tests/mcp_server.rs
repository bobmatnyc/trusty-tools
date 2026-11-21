//! The `trusty-computer-mcp` tool surface and a smoke run of the real bin
//! against a broker on the fake daemon (issue #11460, Phase 5).

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use common::rig::{Conn, Rig, WINDOW};
use serde_json::{Value, json};
use trusty_computer::mcp::{driver_tool, tools};
use trusty_computer::policy::{self, Verdict};

/// Plan section 5, in table order.
const PLAN_TOOLS: [&str; 20] = [
    "computer_connect",
    "computer_status",
    "computer_release",
    "computer_disconnect",
    "computer_grants",
    "computer_apps",
    "computer_windows",
    "computer_read",
    "computer_verify",
    "computer_click",
    "computer_double_click",
    "computer_right_click",
    "computer_scroll",
    "computer_type",
    "computer_set_value",
    "computer_key",
    "computer_hotkey",
    "computer_launch",
    "computer_wait",
    "computer_refresh",
];

#[test]
fn tools_list_is_the_plan_table() {
    let names: Vec<&str> = tools().iter().filter_map(|t| t["name"].as_str()).collect();
    assert_eq!(names, PLAN_TOOLS);
    for t in tools() {
        let desc = t["description"].as_str().expect("description");
        assert!(!desc.is_empty() && !desc.contains('\n'), "{t}");
        assert_eq!(t["inputSchema"]["type"], "object", "{t}");
        assert!(t["inputSchema"]["properties"]["request_id"].is_object());
        let text = t.to_string().to_lowercase();
        for banned in ["screenshot", "pixel", "invoke_menu", "clipboard"] {
            assert!(!text.contains(banned), "{banned} in {t}");
        }
        if let Some(tool) = driver_tool(t["name"].as_str().unwrap_or("")) {
            let rule = policy::rule(tool).expect("a policy row");
            assert_ne!(rule.verdict, Verdict::Deny, "{tool} is DENY");
        }
    }
}

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Mcp {
    fn spawn(rig: &Rig) -> Mcp {
        let mut child = Command::new(env!("CARGO_BIN_EXE_trusty-computer-mcp"))
            .arg("--state")
            .arg(rig.state())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn trusty-computer-mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Mcp {
            child,
            stdin,
            stdout,
            next: 0,
        }
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let msg = json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params});
        writeln!(self.stdin, "{msg}").expect("write");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read");
        let reply: Value = serde_json::from_str(&line).expect("json-rpc reply");
        assert_eq!(reply["id"], json!(self.next), "{reply}");
        reply
    }

    fn tool(&mut self, name: &str, args: Value) -> Value {
        self.rpc("tools/call", json!({"name": name, "arguments": args}))["result"].clone()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn mcp_smoke_against_a_fake_broker() {
    let mut rig = Rig::new();
    let grant = rig.mint(WINDOW);
    let code = rig.code(&grant.id);
    rig.conn = Conn::open(&rig.server.agent); // the rig's holder lets go
    let mut mcp = Mcp::spawn(&rig);

    let init = mcp.rpc(
        "initialize",
        json!({"protocolVersion": "2025-06-18",
        "capabilities": {}, "clientInfo": {"name": "smoke", "version": "0"}}),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "trusty-computer-mcp");
    writeln!(
        mcp.stdin,
        r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
    )
    .expect("notify");
    let listed = mcp.rpc("tools/list", json!({}));
    assert_eq!(listed["result"]["tools"].as_array().map(Vec::len), Some(20));

    let unbound = mcp.tool("computer_apps", json!({}));
    assert_eq!(unbound["structuredContent"]["code"], "not_connected");
    let connected = mcp.tool("computer_connect", json!({"code": code}));
    assert_eq!(connected["isError"], false, "{connected}");
    let deadline = Instant::now() + Duration::from_secs(5);
    while mcp.tool("computer_status", json!({}))["structuredContent"]["lease"]["lease"] != "held" {
        assert!(
            Instant::now() < deadline,
            "lease never reached the MCP session"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // Allowed: list_apps, with a uuid4 request_id filled in.
    let apps = mcp.tool("computer_apps", json!({}));
    assert_eq!(apps["isError"], false, "{apps}");
    let forward = rig
        .audit()
        .into_iter()
        .rfind(|l| l["op"] == "call" && l["tool"] == "list_apps")
        .expect("audited call");
    let rid = forward["request_id"].as_str().expect("request_id");
    assert!(rid.len() == 36 && rid.as_bytes()[14] == b'4', "{rid}");

    // Denied: launch_app is not in this grant; the broker's code comes back as a tool error.
    let denied = mcp.tool(
        "computer_launch",
        json!({"bundle_id": common::rig::BUNDLE, "request_id": "smoke-1"}),
    );
    assert_eq!(denied["isError"], true, "{denied}");
    assert_eq!(denied["structuredContent"]["code"], "tool_not_granted");
    assert!(
        denied["content"][0]["text"]
            .as_str()
            .is_some_and(|t| t.starts_with("tool_not_granted"))
    );
    let calls = rig.agent_calls();
    assert!(!calls.iter().any(|c| c == "launch_app"), "{calls:?}");
}

/// #11460 fix round: an argument outside a tool's schema never reaches the broker.
#[test]
fn undeclared_arguments_are_refused() {
    let dir = common::short_tmp();
    let mut server = trusty_computer::mcp::McpServer::new(dir.path().to_path_buf());
    let cases = [
        (
            "computer_click",
            json!({"pid": 1, "window_id": 1, "element_token": "t", "snapshot_id": "s", "element_index": 2}),
        ),
        (
            "computer_key",
            json!({"pid": 1, "key": "q", "modifiers": ["cmd"]}),
        ),
        (
            "computer_read",
            json!({"pid": 1, "window_id": 1, "include_screenshot": true}),
        ),
        (
            "computer_connect",
            json!({"code": "tc-x", "grant_id": "g-1"}),
        ),
    ];
    for (name, args) in cases {
        let args = args.as_object().expect("object").clone();
        let reply = server.call(name, &args);
        assert_eq!(reply["isError"], true, "{name}: {reply}");
        assert_eq!(
            reply["structuredContent"]["code"], "bad_args",
            "{name}: {reply}"
        );
    }
}
