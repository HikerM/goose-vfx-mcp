use std::future::Future;
use std::pin::Pin;
use tokio::signal;

#[cfg(unix)]
pub fn shutdown_signal() -> Pin<Box<dyn Future<Output = ()> + Send>> {
    Box::pin(async move {
        let ctrl_c = async {
            signal::ctrl_c()
                .await
                .expect("failed to install Ctrl+C handler");
        };

        #[cfg(unix)]
        let terminate = async {
            signal::unix::signal(signal::unix::SignalKind::terminate())
                .expect("failed to install signal handler")
                .recv()
                .await;
        };

        tokio::select! {
            _ = ctrl_c => {},
            _ = terminate => {},
        }
    })
}

#[cfg(windows)]
pub fn shutdown_signal() -> Pin<Box<dyn Future<Output = ()> + Send>> {
    Box::pin(async move {
        let mut ctrl_break =
            signal::windows::ctrl_break().expect("failed to install Ctrl+Break handler");
        tokio::select! {
            result = signal::ctrl_c() => result.expect("failed to install Ctrl+C handler"),
            _ = ctrl_break.recv() => {}
        }
    })
}

#[cfg(not(any(unix, windows)))]
pub fn shutdown_signal() -> Pin<Box<dyn Future<Output = ()> + Send>> {
    Box::pin(async move {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    })
}
