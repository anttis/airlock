//! Stdio adapter of the `airlock` MCP server.
//!
//! An agent starts this mode of the binary as a stdio MCP server. The
//! adapter connects the agent to the local MCP socket of the server, with
//! no change to the messages.
//!
//! The adapter has no privileges. It runs as the user of the agent, and the
//! server gets that user from the kernel. If the socket is not available,
//! the adapter stops with an error. It never uses another transport.

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

/// Program name that selects the adapter mode.
const NAME: &str = "airlock-mcp";

/// Size of one read from the socket.
const CHUNK: usize = 64 * 1024;

/// Tell if the program name selects the adapter mode.
/// Args:
///  - `argv0`: First command-line argument, as the process got it
pub fn is_adapter(argv0: Option<&OsStr>) -> bool {
    argv0
        .and_then(|a| Path::new(a).file_name())
        .is_some_and(|name| name == NAME)
}

/// Run the adapter on stdin and stdout until the server closes the
/// connection.
///
/// Returns:
///   Exit code of the process.
pub fn run() -> i32 {
    run_with(
        Path::new(airlock_common::MCP_SOCKET),
        std::io::stdin(),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}

/// Connect to the socket and relay. See [`run`].
/// Args:
///  - `socket`: Path of the MCP socket
///  - `input`: Messages from the agent
///  - `output`: Messages to the agent. Only bytes from the server go here
///  - `diag`: Error texts for the user
fn run_with(
    socket: &Path,
    input: impl Read + Send + 'static,
    output: &mut impl Write,
    diag: &mut impl Write,
) -> i32 {
    let stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(e) => {
            let _ = writeln!(
                diag,
                "{NAME}: cannot connect to the airlock MCP socket {}: {e}",
                socket.display()
            );
            return 1;
        }
    };
    match relay(stream, input, output) {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(
                diag,
                "{NAME}: connection to the airlock MCP server failed: {e}"
            );
            1
        }
    }
}

/// Relay the bytes in both directions: `input` to the socket, and the
/// socket to `output`, with no change. Both sides carry newline-delimited
/// JSON-RPC messages, the same framing as the MCP stdio transport.
///
/// At the end of `input`, only the write side of the socket closes. The
/// server then sends the replies of the open requests and closes the
/// connection. The relay ends when the server closes the connection or the
/// agent closes `output`.
///
/// Returns:
///   Error if the socket read fails. A closed `output` is not an error.
fn relay(
    stream: UnixStream,
    mut input: impl Read + Send + 'static,
    output: &mut impl Write,
) -> std::io::Result<()> {
    let mut to_server = stream.try_clone()?;
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut input, &mut to_server);
        let _ = to_server.shutdown(std::net::Shutdown::Write);
    });
    let mut from_server = stream;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = from_server.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        // Flush each read, so that a reply does not wait in the buffer.
        if output
            .write_all(&buf[..n])
            .and_then(|()| output.flush())
            .is_err()
        {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Cursor};

    use super::*;

    /// Test that only the program name `airlock-mcp` selects the adapter,
    /// also with a directory before it.
    ///   1. Check names with and without a directory
    ///   2. Check other names and no name
    #[test]
    fn adapter_mode_follows_program_name() {
        assert!(is_adapter(Some(OsStr::new("airlock-mcp"))));
        assert!(is_adapter(Some(OsStr::new("/run/airlock/bin/airlock-mcp"))));
        assert!(!is_adapter(Some(OsStr::new("/usr/bin/airlockd"))));
        assert!(!is_adapter(Some(OsStr::new("airlock-mcp-old"))));
        assert!(!is_adapter(None));
    }

    /// Test that the adapter sends all input to the server and keeps the
    /// replies that come after the end of the input. A client such as
    /// `printf … | airlock-mcp` closes its input before the replies.
    ///   1. Start a fake server that reads two lines to the end of input
    ///   2. Run the relay with the two lines as input
    ///   3. Check that the server got the lines and the output has only the
    ///      replies that the server sent after the end of input
    #[test]
    fn relay_keeps_replies_after_input_end() {
        let (client, server) = UnixStream::pair().unwrap();
        let fake = std::thread::spawn(move || {
            let mut lines = Vec::new();
            for line in BufReader::new(server.try_clone().unwrap()).lines() {
                lines.push(line.unwrap());
            }
            let mut server = server;
            server.write_all(b"{\"id\":1}\n{\"id\":2}\n").unwrap();
            lines
        });

        let input =
            Cursor::new(b"{\"id\":1,\"method\":\"a\"}\n{\"id\":2,\"method\":\"b\"}\n".to_vec());
        let mut output = Vec::new();
        relay(client, input, &mut output).unwrap();

        assert_eq!(
            fake.join().unwrap(),
            ["{\"id\":1,\"method\":\"a\"}", "{\"id\":2,\"method\":\"b\"}"]
        );
        assert_eq!(output, b"{\"id\":1}\n{\"id\":2}\n");
    }

    /// Test that the adapter stops with exit code 1 and a text on the error
    /// output if the socket is missing. It must not write to stdout, which
    /// carries only MCP messages.
    ///   1. Run the adapter with a socket path that does not exist
    ///   2. Check the exit code, the empty output and the error text
    #[test]
    fn missing_socket_fails_with_error_text() {
        let dir = crate::test_cfg::temp_dir();
        let mut output = Vec::new();
        let mut diag = Vec::new();
        let code = run_with(
            &dir.path().join("mcp.sock"),
            Cursor::new(Vec::new()),
            &mut output,
            &mut diag,
        );
        assert_eq!(code, 1);
        assert!(output.is_empty());
        let diag = String::from_utf8(diag).unwrap();
        assert!(
            diag.contains("cannot connect to the airlock MCP socket"),
            "{diag}"
        );
    }
}
