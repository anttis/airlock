//! Commands of socket clients run as the user of the client. These tests
//! change the user of the commands, so they need root. Run them with
//! `mise run test:root`.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::rc::Rc;

use serde_json::{Value, json};

use crate::admin::{Capabilities, RuleEditing};
use crate::test_cfg::{
    FakeDecision, FakeGrantHost, LineClient, block_on_local, serve_admin_socket,
};

/// All agent features on.
const ALL: Capabilities = Capabilities {
    grants: true,
    rule_editing: RuleEditing::All,
};

/// Unprivileged user of the tests.
const NOBODY: u32 = 65534;

/// Account data of the test container.
const PASSWD: &str = "root:x:0:0:root:/root:/bin/sh\n\
                      nobody:x:65534:65534:nobody:/home/nobody:/bin/sh\n";

/// Connect to the socket as `id` (user and group). Only a dedicated thread
/// changes its IDs: the raw syscalls change the calling thread only, and
/// the kernel records the IDs of the client at connect time.
///
/// Returns:
///   Connected stream for the test runtime.
fn connect_as(path: &Path, id: u32) -> tokio::net::UnixStream {
    let path = path.to_path_buf();
    let stream = std::thread::spawn(move || {
        // The group must change first. Without root, the thread cannot
        // change it.
        assert_eq!(unsafe { libc::syscall(libc::SYS_setresgid, id, id, id) }, 0);
        assert_eq!(unsafe { libc::syscall(libc::SYS_setresuid, id, id, id) }, 0);
        std::os::unix::net::UnixStream::connect(path).unwrap()
    })
    .join()
    .unwrap();
    stream.set_nonblocking(true).unwrap();
    tokio::net::UnixStream::from_std(stream).unwrap()
}

/// Run a command with a grant and return the output object.
async fn execute(client: &mut LineClient, args: Value) -> Value {
    let mut args = args;
    args["hosts"] = json!(["example.com"]);
    let result = client.call_tool("execute", args).await;
    assert_ne!(result["isError"], true, "{result}");
    assert_eq!(result["structuredContent"]["approved"], true, "{result}");
    result["structuredContent"].clone()
}

/// Test that the command of an unprivileged socket client runs as that
/// user, while a root client on the same server runs as root. A non-root
/// agent must not get root commands when it asks only for network access.
///   1. Make a root-only file and a directory below a root-only parent
///   2. Connect one client as `nobody` and one client as root
///   3. Check the IDs, the groups and the home of the `nobody` command
///   4. Check that `nobody` cannot read the root-only file, and that a cwd
///      below the root-only parent falls back to `/`
///   5. Check that the root command runs as root, and the users in the
///      host requests
#[test]
#[ignore = "needs root: mise run test:root"]
fn root_socket_clients_run_commands_as_their_users() {
    assert_eq!(unsafe { libc::geteuid() }, 0, "the test needs root");
    block_on_local(async {
        let host = Rc::new(FakeGrantHost {
            decision: FakeDecision::Grant,
            ..FakeGrantHost::default()
        });
        let socket = serve_admin_socket(&host, ALL, PASSWD, true).await;
        let secret = socket.dir.path().join("secret");
        std::fs::write(&secret, "root only").unwrap();
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
        let locked = socket.dir.path().join("locked");
        let inner = locked.join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();

        let mut nobody = LineClient::start(connect_as(&socket.path, NOBODY)).await;
        let mut root = LineClient::connect(&socket.path).await;

        let ids = execute(
            &mut nobody,
            json!({ "cmd": "id -u; id -g; id -G; echo \"$HOME\"" }),
        )
        .await;
        assert_eq!(
            ids["stdout"], "65534\n65534\n65534\n/home/nobody\n",
            "{ids}"
        );

        let read = execute(
            &mut nobody,
            json!({ "cmd": format!("cat {}", secret.display()) }),
        )
        .await;
        assert_ne!(read["exit_code"], 0, "{read}");
        assert!(
            read["stderr"]
                .as_str()
                .unwrap()
                .contains("Permission denied"),
            "{read}"
        );

        let cwd = execute(
            &mut nobody,
            json!({ "cmd": "pwd", "cwd": inner.to_str().unwrap() }),
        )
        .await;
        assert_eq!(cwd["stdout"], "/\n", "{cwd}");

        let as_root = execute(
            &mut root,
            json!({ "cmd": "id -u; cat secret", "cwd": socket.dir.path().to_str().unwrap() }),
        )
        .await;
        assert_eq!(as_root["stdout"], "0\nroot only", "{as_root}");

        let users: Vec<(u32, String, bool)> =
            host.calls.borrow().iter().map(|c| c.user.clone()).collect();
        let nobody_user = (NOBODY, "nobody".to_string(), true);
        assert_eq!(
            users,
            [
                nobody_user.clone(),
                nobody_user.clone(),
                nobody_user,
                (0, "root".to_string(), true),
            ]
        );
    });
}
