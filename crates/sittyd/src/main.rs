use sitty_proto::paths;

#[tokio::main]
async fn main() {
    let socket = paths::socket_path();
    if let Err(e) = sittyd::server::run(socket.clone()).await {
        eprintln!("sittyd: {}: {e}", socket.display());
        std::process::exit(if e.kind() == std::io::ErrorKind::AddrInUse { 2 } else { 1 });
    }
}
