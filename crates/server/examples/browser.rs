#[path = "browser/assets.rs"]
mod assets;
#[path = "browser/options.rs"]
mod options;

use std::{
    io,
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    time::Duration,
};
use tokio::{net::TcpListener, sync::watch};

async fn serve() -> Result<(), io::Error> {
    let port = options::port(std::env::args_os().skip(1))?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/browser");
    let router = assets::router(&root)?;
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).await?;
    println!("Browser call example: http://{}", listener.local_addr()?);
    println!("Press Ctrl+C to stop the localhost example.");
    let (stop, mut stopped) = watch::channel(false);
    let mut serving = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                while !*stopped.borrow_and_update() {
                    if stopped.changed().await.is_err() {
                        break;
                    }
                }
            })
            .await
    });
    let signal = tokio::select! {
        result = &mut serving => return result.map_err(io::Error::other)?,
        signal = tokio::signal::ctrl_c() => signal,
    };
    stop.send_replace(true);
    let cleanup = match tokio::time::timeout(Duration::from_secs(6), &mut serving).await {
        Ok(result) => result.map_err(io::Error::other)?,
        Err(primary) => {
            serving.abort();
            match serving.await {
                Err(error) if error.is_cancelled() => Err(io::Error::other(primary)),
                Err(error) => Err(io::Error::other(format!(
                    "{primary}; example task failed: {error}"
                ))),
                Ok(Err(error)) => Err(io::Error::other(format!(
                    "{primary}; example cleanup failed: {error}"
                ))),
                Ok(Ok(())) => Err(io::Error::other(primary)),
            }
        }
    };
    match (signal, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(io::Error::other(format!(
            "{primary}; example cleanup failed: {cleanup}"
        ))),
    }
}

#[tokio::main(worker_threads = 2)]
async fn main() -> Result<(), io::Error> {
    serve().await
}
