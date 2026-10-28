use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    let addr = std::env::var("BROWSER_RELAY_BIND").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let socket: SocketAddr = addr.parse().expect("valid BROWSER_RELAY_BIND");
    let listener = tokio::net::TcpListener::bind(socket)
        .await
        .expect("bind listener");
    println!("BrowserRelay listening on http://{addr}");
    axum::serve(
        listener,
        relay_server::router(browser_runtime::BrowserRuntimeConfig::default()),
    )
    .await
    .expect("server stopped");
}
