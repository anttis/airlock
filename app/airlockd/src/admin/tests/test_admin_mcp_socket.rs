//! `airlock` MCP server on the local socket: the tools, the client identity
//! of each connection, and the end of a command when its client goes away.
//!
//! The commands run outside of a container and as the test user. The root
//! tests in `test_execute_as_caller` cover the change of the user.

use std::rc::Rc;

use serde_json::{Value, json};

use crate::admin::{AdminState, Capabilities, RuleEditing};
use crate::test_cfg::{
    FakeDecision, FakeGrantHost, LineClient, block_on_local, eventually_async, post_admin_mcp,
    serve_admin_mcp, serve_admin_socket,
};

/// All agent features on.
const ALL: Capabilities = Capabilities {
    grants: true,
    rule_editing: RuleEditing::All,
};

/// Get the `execute` description from a `tools/list` reply.
fn execute_description(reply: &Value) -> String {
    reply["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "execute")
        .map(|t| t["description"].as_str().unwrap().to_string())
        .expect("no execute tool")
}

/// Test that a socket client gets the same tools as an HTTP client, and
/// that only the HTTP endpoint marks `execute` as deprecated. An agent on
/// the old endpoint must learn that its commands do not run as its user.
///   1. Connect to the socket, initialize, and list the tools
///   2. Check the tool names and that `execute` has no deprecation note
///   3. List the tools over HTTP and check the deprecation note
#[test]
fn socket_client_gets_all_tools_and_http_execute_is_deprecated() {
    block_on_local(async {
        let host = Rc::new(FakeGrantHost::default());
        let socket = serve_admin_socket(&host, ALL, "", false).await;
        let mut client = LineClient::connect(&socket.path).await;

        let reply = client.request("tools/list", json!({})).await;
        let mut names: Vec<&str> = reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "execute",
                "get_manual_entry",
                "list_manual_entries",
                "show_rules",
                "upsert_rule"
            ]
        );
        assert!(!execute_description(&reply).contains("DEPRECATED"));

        let addr = serve_admin_mcp(AdminState::new(), &host, true, ALL).await;
        let list = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
        let http = post_admin_mcp(addr, &list, &[]).await;
        let reply: Value = serde_json::from_str(&http.body).unwrap();
        assert!(
            execute_description(&reply).starts_with("DEPRECATED"),
            "{reply}"
        );
    });
}

/// Test that only the kernel identity of the socket client selects the
/// user of a command. Arguments that name another user have no effect.
/// The command gets the home and the name of the client from the account
/// data of the container.
///   1. Serve the socket with account data for the test user
///   2. Call `execute` with forged `uid` and `user` arguments
///   3. Check the user in the host request and the user values that the
///      command sees, and that the result has no deprecation warning
#[test]
fn forged_user_arguments_cannot_change_command_user() {
    block_on_local(async {
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        let host = Rc::new(FakeGrantHost {
            decision: FakeDecision::Grant,
            ..FakeGrantHost::default()
        });
        let passwd = format!("tester:x:{uid}:{gid}::/home/tester:/bin/sh\n");
        let socket = serve_admin_socket(&host, ALL, &passwd, false).await;
        let mut client = LineClient::connect(&socket.path).await;

        let result = client
            .call_tool(
                "execute",
                json!({
                    "cmd": "id -u; echo \"$HOME\"; echo \"$USER\"",
                    "hosts": ["example.com"],
                    "uid": 0,
                    "user": "root",
                }),
            )
            .await;
        assert_ne!(result["isError"], true, "{result}");
        let output = &result["structuredContent"];
        assert_eq!(output["approved"], true, "{result}");
        assert_eq!(output["stdout"], format!("{uid}\n/home/tester\ntester\n"));
        assert!(output.get("warning").is_none(), "{result}");
        assert_eq!(host.calls.borrow()[0].user, (uid, "tester".into(), true));
    });
}

/// Test that the end of a client connection stops only the commands of
/// that client. Another client with a command at the same time is not
/// affected.
///   1. Start a long command on client A and a short command on client B
///   2. Wait until the host has both grant requests
///   3. Close client A
///   4. Check that the command of B completes, and that both grants end
///      (the grant of A ends because its command stops)
#[test]
fn closing_one_connection_stops_only_its_command() {
    block_on_local(async {
        let host = Rc::new(FakeGrantHost {
            decision: FakeDecision::Grant,
            ..FakeGrantHost::default()
        });
        let socket = serve_admin_socket(&host, ALL, "", false).await;
        let mut a = LineClient::connect(&socket.path).await;
        let mut b = LineClient::connect(&socket.path).await;

        a.start_call(
            "execute",
            json!({ "cmd": "sleep 30", "hosts": ["a.example"] }),
        )
        .await;
        let id = b
            .start_call(
                "execute",
                json!({ "cmd": "sleep 1; echo B", "hosts": ["b.example"] }),
            )
            .await;
        eventually_async("both grant requests", async || {
            (host.calls.borrow().len() == 2).then_some(())
        })
        .await;
        drop(a);

        let reply = b.read_reply(id).await;
        assert_eq!(
            reply["result"]["structuredContent"]["stdout"], "B\n",
            "{reply}"
        );
        // The MCP service waits up to 5 s for open replies before it stops
        // the requests of a closed connection.
        eventually_async("both grants released", async || {
            (host.released.get() == 2).then_some(())
        })
        .await;
    });
}
