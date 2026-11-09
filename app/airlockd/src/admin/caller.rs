//! Identity of the client of an admin MCP connection.
//!
//! A client of the local MCP socket has a user that the kernel verified.
//! Its commands run as that user. A client of the old HTTP endpoint has no
//! verified user. Its commands run as the sandbox user.
//!
//! Also reads the name and the home directory of a user from the account
//! data of the container. These values are only for display and for the
//! environment of a command. They never select the user.

use std::io::Read;
use std::path::Path;

/// Maximum size of the account file to read.
const ACCOUNT_FILE_LIMIT: u64 = 1024 * 1024;

/// Client of one admin MCP connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    /// Client of the local socket, with the IDs from the kernel.
    Verified(PeerId),
    /// Client of the old HTTP endpoint. Its commands run as the sandbox
    /// user.
    BootUnverified,
}

/// IDs of a socket client, as the kernel gave them at connect time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerId {
    /// Effective user ID.
    pub uid: u32,
    /// Effective group ID.
    pub gid: u32,
    /// Process ID, if the kernel gave it. Only for logs.
    pub pid: Option<i32>,
}

/// Account data of a user in the container.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Account {
    /// User name, or `None` if the account file has no entry.
    pub name: Option<String>,
    /// Home directory, or `None` if the account file has no entry.
    pub home: Option<String>,
}

/// Read the account data of a user from `/etc/passwd` in the container.
/// The read runs on a blocking thread, because the container controls the
/// file.
/// Args:
///  - `root`: Container root as this process sees it
///  - `uid`: User ID to find
///
/// Returns:
///   Account data. Empty if the file or the entry is missing or not valid,
///   or if `/etc` or `/etc/passwd` is a link.
pub async fn lookup_account(root: String, uid: u32) -> Account {
    tokio::task::spawn_blocking(move || read_account(Path::new(&root), uid))
        .await
        .unwrap_or_default()
}

/// Read and parse the account file. See [`lookup_account`]. A link at
/// `/etc` or at `/etc/passwd` gives no account data, so the read cannot
/// leave the container root or follow a file that the guest selects.
fn read_account(root: &Path, uid: u32) -> Account {
    match open_without_links(root, &["etc", "passwd"]).and_then(read_limited) {
        Ok(text) => parse_passwd(&text, uid),
        Err(e) => {
            tracing::debug!("caller: no account data for uid {uid}: {e}");
            Account::default()
        }
    }
}

/// Open a file below `root`. Each name in `names` must not be a link.
/// Args:
///  - `root`: Directory to start at. It is the trusted container root
///  - `names`: Directory names, then the file name
///
/// Returns:
///   The open file, or an error if a name is missing or a link.
fn open_without_links(root: &Path, names: &[&str]) -> std::io::Result<std::fs::File> {
    use std::os::fd::OwnedFd;

    let (file, dirs) = names
        .split_last()
        .ok_or_else(|| std::io::Error::other("no file name"))?;
    // Each step opens the next name relative to the directory of the step
    // before. Thus a change of a name after a check cannot add a link.
    let mut dir: OwnedFd = std::fs::File::open(root)?.into();
    for name in dirs {
        dir = open_at(&dir, name, libc::O_DIRECTORY)?;
    }
    // O_NONBLOCK: a FIFO in place of the file must not block the open.
    Ok(open_at(&dir, file, libc::O_NONBLOCK)?.into())
}

/// Open `name` in the directory `dir` for reading, with no link at the name.
fn open_at(
    dir: &std::os::fd::OwnedFd,
    name: &str,
    flags: libc::c_int,
) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let name = std::ffi::CString::new(name)?;
    let flags = flags | libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: `name` is a valid C string and `dir` is an open descriptor.
    let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a new descriptor that nothing else owns.
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}

/// Read a regular file up to [`ACCOUNT_FILE_LIMIT`] bytes. A FIFO or a
/// device is refused, so the read cannot block.
fn read_limited(file: std::fs::File) -> std::io::Result<String> {
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(ACCOUNT_FILE_LIMIT).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Find the entry of `uid` in `passwd` text
/// (`name:password:uid:gid:gecos:home:shell`). Lines that are not valid
/// are ignored. The first entry of the uid wins.
fn parse_passwd(text: &str, uid: u32) -> Account {
    for line in text.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 7 || fields[0].is_empty() {
            continue;
        }
        if fields[2].parse::<u32>().ok() != Some(uid) {
            continue;
        }
        let home = Some(fields[5]).filter(|h| h.starts_with('/'));
        return Account {
            name: Some(fields[0].to_string()),
            home: home.map(ToString::to_string),
        };
    }
    Account::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that the parser finds the first valid entry of a uid and
    /// ignores lines that are not valid. A home that is not absolute is
    /// not used.
    ///   1. Parse text with comments, short lines and a bad uid
    ///   2. Check the result for a known uid, an unknown uid, and a uid with
    ///      a relative home
    #[test]
    fn passwd_parser_finds_first_valid_entry() {
        let text = "\
# comment
root:x:0:0:root:/root:/bin/sh
short:x:5
bad:x:abc:0::/bad:/bin/sh
agent:x:1000:1000:Agent:/home/agent:/bin/sh
agent2:x:1000:1000::/home/agent2:/bin/sh
rel:x:1001:1001::home/rel:/bin/sh
";
        assert_eq!(
            parse_passwd(text, 1000),
            Account {
                name: Some("agent".into()),
                home: Some("/home/agent".into()),
            }
        );
        assert_eq!(parse_passwd(text, 42), Account::default());
        assert_eq!(
            parse_passwd(text, 1001),
            Account {
                name: Some("rel".into()),
                home: None,
            }
        );
    }

    /// Test that the account lookup uses the file in the container root
    /// and gives no account data for a link at `/etc/passwd`, a link at
    /// `/etc` or a FIFO. The guest controls these names. A FIFO must not
    /// block the read.
    ///   1. Write a passwd file in a temp root and check the lookup
    ///   2. Replace the file with a link to a valid file and check the lookup
    ///   3. Replace `etc` with a link to a directory with a valid file and
    ///      check the lookup
    ///   4. Put a FIFO in place of the file and check the lookup
    #[test]
    fn account_lookup_refuses_links_and_fifo() {
        let root = crate::test_cfg::temp_dir();
        let etc = root.path().join("etc");
        std::fs::create_dir_all(&etc).unwrap();
        let passwd = etc.join("passwd");
        let text = "agent:x:1000:1000::/home/agent:/bin/sh\n";
        std::fs::write(&passwd, text).unwrap();
        assert_eq!(
            read_account(root.path(), 1000).home.as_deref(),
            Some("/home/agent")
        );

        let real = root.path().join("real");
        std::fs::write(&real, text).unwrap();
        std::fs::remove_file(&passwd).unwrap();
        std::os::unix::fs::symlink(&real, &passwd).unwrap();
        assert_eq!(read_account(root.path(), 1000), Account::default());

        let other = root.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("passwd"), text).unwrap();
        std::fs::remove_dir_all(&etc).unwrap();
        std::os::unix::fs::symlink(&other, &etc).unwrap();
        assert_eq!(read_account(root.path(), 1000), Account::default());

        std::fs::remove_file(&etc).unwrap();
        std::fs::create_dir_all(&etc).unwrap();
        let path = std::ffi::CString::new(passwd.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o644) }, 0);
        assert_eq!(read_account(root.path(), 1000), Account::default());
    }
}
