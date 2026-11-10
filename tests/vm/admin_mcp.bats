#!/usr/bin/env bats

load helpers

# The `airlock` MCP server on the local socket, through the stdio adapter.
# The approval of a command needs the user on the host, so these tests do
# not run commands. The Rust tests cover the command user.

setup_file() {
    vm_setup_file

    cat > airlock.toml <<'EOF'
[vm]
image = "alpine:latest"

[network.grants]
enabled = true
EOF
}

# MCP messages of a client: initialize, the initialized notification and
# tools/list, one per line.
MESSAGES='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}'

# Test that the stdio adapter gives each user the MCP server, as an agent
# uses it. The adapter must keep the replies that come after the end of
# its input.
#   1. Send the messages through the adapter as root
#   2. Add an unprivileged user and send the messages as that user
#   3. Check the server name and the execute tool in both replies
@test "mcp adapter serves tools to root and to unprivileged user" {
    run_vm sh -c '
        msgs="$1"
        out=$(printf "%s\n" "$msgs" | /run/airlock/bin/airlock-mcp)
        case "$out" in *"\"id\":2"*"\"name\":\"execute\""*) echo ROOT-OK ;; esac
        case "$out" in *"\"name\":\"airlock\""*) echo ROOT-SERVER-OK ;; esac
        adduser -D agent
        out=$(printf "%s\n" "$msgs" | su agent -s /bin/sh -c /run/airlock/bin/airlock-mcp)
        case "$out" in *"\"id\":2"*"\"name\":\"execute\""*) echo AGENT-OK ;; esac
    ' sh "$MESSAGES"
    assert_success
    assert_output_contains 'ROOT-OK'
    assert_output_contains 'ROOT-SERVER-OK'
    assert_output_contains 'AGENT-OK'
}

# Test that guest users cannot replace the socket or the adapter. The
# socket authenticates the user, so another file in its place would get
# the requests of the agents.
#   1. Show the owner and mode of the directory, the socket and the adapter
#   2. Try to replace them as an unprivileged user
#   3. Check that the tries fail and that the adapter still works
@test "mcp socket and adapter are root owned and unprivileged user cannot replace them" {
    run_vm sh -c '
        stat -c "%n %U %a %F" /run/airlock /run/airlock/bin \
            /run/airlock/mcp.sock /run/airlock/bin/airlock-mcp
        adduser -D agent
        su agent -s /bin/sh -c "
            rm -f /run/airlock/mcp.sock || echo RM-SOCKET-REFUSED
            echo x > /run/airlock/bin/airlock-mcp || echo WRITE-ADAPTER-REFUSED
            mv /run/airlock/bin/airlock-mcp /run/airlock/bin/x || echo MOVE-ADAPTER-REFUSED
        "
        echo x > /run/airlock/bin/airlock-mcp || echo ROOT-WRITE-REFUSED
        out=$(printf "%s\n" "$1" | /run/airlock/bin/airlock-mcp)
        case "$out" in *"\"name\":\"execute\""*) echo STILL-OK ;; esac
    ' sh "$MESSAGES"
    assert_success
    assert_output_contains '/run/airlock root 755 directory'
    assert_output_contains '/run/airlock/bin root 755 directory'
    assert_output_contains '/run/airlock/mcp.sock root 666 socket'
    assert_output_contains 'RM-SOCKET-REFUSED'
    assert_output_contains 'WRITE-ADAPTER-REFUSED'
    assert_output_contains 'MOVE-ADAPTER-REFUSED'
    assert_output_contains 'ROOT-WRITE-REFUSED'
    assert_output_contains 'STILL-OK'
}

# Test that the old HTTP endpoint still lists `execute` for one release,
# with a deprecation note in its description.
#   1. Send tools/list to http://admin.airlock/mcp
#   2. Check the execute tool and the note
@test "http endpoint still lists deprecated execute tool" {
    run_vm sh -c '
        wget -qO- --header "Content-Type: application/json" \
            --header "Accept: application/json, text/event-stream" \
            --post-data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}" \
            http://admin.airlock/mcp
    '
    assert_success
    assert_output_contains '"name":"execute"'
    assert_output_contains 'DEPRECATED on this HTTP endpoint'
}
