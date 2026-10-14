use std::path::{Path, PathBuf};

/// `$SITTY_SOCKET`, else `$TMPDIR/sitty-$UID/sittyd.sock`.
pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("SITTY_SOCKET") {
        return PathBuf::from(p);
    }
    let uid = unsafe { libc::getuid() };
    std::env::temp_dir().join(format!("sitty-{uid}")).join("sittyd.sock")
}

/// The human token lives next to the socket, mode 0600.
pub fn human_token_path(socket: &Path) -> PathBuf {
    socket.with_file_name("human.token")
}
