use std::path::{Path, PathBuf};

/// `$SITTY_SOCKET` (ignored when empty; made absolute against the current dir), else `$TMPDIR/sitty-$UID/sittyd.sock`.
pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("SITTY_SOCKET").filter(|p| !p.is_empty()) {
        return resolve(PathBuf::from(p));
    }
    let uid = unsafe { libc::getuid() };
    std::env::temp_dir().join(format!("sitty-{uid}")).join("sittyd.sock")
}

/// The human token lives next to the socket, mode 0600.
pub fn human_token_path(socket: &Path) -> PathBuf {
    socket.with_file_name("human.token")
}

/// Make `p` absolute against the current dir (no symlink resolution, `..` kept).
fn resolve(p: PathBuf) -> PathBuf {
    std::path::absolute(&p).unwrap_or(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Env vars are process-global; serialize the tests that touch SITTY_SOCKET.
    static ENV: Mutex<()> = Mutex::new(());

    #[test]
    fn empty_socket_env_is_ignored() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("SITTY_SOCKET", "") };
        let p = socket_path();
        unsafe { std::env::remove_var("SITTY_SOCKET") };
        assert!(p.ends_with("sittyd.sock") && p.is_absolute(), "{p:?}");
    }

    #[test]
    fn relative_socket_env_becomes_absolute() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("SITTY_SOCKET", "rel/s.sock") };
        let p = socket_path();
        unsafe { std::env::remove_var("SITTY_SOCKET") };
        assert!(p.is_absolute());
        assert_eq!(p, std::env::current_dir().unwrap().join("rel/s.sock"));
    }
}
