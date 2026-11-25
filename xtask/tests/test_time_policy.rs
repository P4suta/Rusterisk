use std::path::Path;
use xtask::test_time_policy::inspect;

#[test]
fn real_sleeps_and_private_timeouts_are_rejected_even_inside_macros() -> Result<(), syn::Error> {
    let path = Path::new("crates/server/tests/io.rs");
    for source in [
        "async fn read() { tokio::time::sleep(delay).await; }",
        "fn watch() { std::thread::sleep(delay); }",
        "async fn read() { tokio::select! { _ = tokio::time::timeout(delay, ready) => {} } }",
        "use tokio::time::timeout as guard; async fn read() { guard(delay, ready).await; }",
        "use std::thread; fn wait() { thread::sleep(delay); }",
    ] {
        assert!(!inspect(path, source)?.is_empty(), "{source}");
    }
    Ok(())
}

#[test]
fn native_receives_and_request_clients_use_shared_deadlines() -> Result<(), syn::Error> {
    let path = Path::new("crates/server/tests/fixture.rs");
    for source in [
        "fn wait() { chan.recv_timeout(Duration::from_secs(7)); }",
        "fn wait() { stream.set_read_timeout(Some(std::time::Duration::from_secs(2))); }",
        "fn wait() { stream.set_write_timeout(Some(Duration::from_millis(50))); }",
        "fn wait() { TcpStream::connect_timeout(addr, Duration::from_secs(4)); }",
        "fn wait() { Client::builder().timeout(Duration::from_secs(9)); }",
    ] {
        assert!(!inspect(path, source)?.is_empty(), "{source}");
    }
    let shared = "fn wait() { client.timeout(Wait::Ready.duration()); }";
    assert!(inspect(path, shared)?.is_empty());
    Ok(())
}

#[test]
fn test_only_methods_and_resumed_clocks_require_shared_waits() -> Result<(), syn::Error> {
    let path = Path::new("crates/server/src/worker.rs");
    for source in [
        "#[cfg(test)] impl Worker { async fn read() { tokio::time::timeout(delay, ready).await; } }",
        "impl Worker { #[cfg(test)] async fn read() { std::thread::sleep(delay); } }",
        "use tokio::time::resume as wall; #[tokio::test(start_paused = true)] async fn read() { wall(); tokio::time::sleep(delay).await; }",
        "#[tokio::test(start_paused = true)] async fn read() { if real { tokio::time::resume(); } tokio::time::sleep(delay).await; }",
    ] {
        assert!(!inspect(path, source)?.is_empty(), "{source}");
    }
    let paused = "#[tokio::test(flavor = \"current_thread\", start_paused = true)] async fn clock() { tokio::time::sleep(delay).await; }";
    assert!(inspect(path, paused)?.is_empty());
    Ok(())
}

#[test]
fn test_modules_are_checked_and_paused_timers_preserve_their_clock() -> Result<(), syn::Error> {
    let path = Path::new("crates/server/src/worker.rs");
    let source = "async fn serve() { tokio::time::timeout(limit, read()).await; }
        #[cfg(test)] mod tests { async fn read() { tokio::time::timeout(limit, read()).await; } }";
    assert_eq!(inspect(path, source)?.len(), 1);
    let paused = "#[tokio::test(start_paused = true)] async fn clock() {
        tokio::time::sleep(period).await; tokio::time::advance(period).await; }";
    assert!(inspect(path, paused)?.is_empty());
    assert!(inspect(path, "fn work() { observer.timeout(); }")?.is_empty());
    Ok(())
}

#[test]
fn applied_clocks_and_aliases_do_not_hide_real_waits() -> Result<(), syn::Error> {
    let path = Path::new("crates/server/src/relay_tests/driver.rs");
    for source in [
        "async fn read() { if false { tokio::time::pause(); } tokio::time::sleep(delay).await; }",
        "async fn read() { let unused = tokio::time::pause; tokio::time::sleep(delay).await; }",
        "async fn read() { let unused = tokio::time::advance(delay); tokio::time::sleep(delay).await; }",
        "async fn read() { guard(delay, read()).await; } use tokio::time::timeout as guard;",
        "#[tokio::test(start_paused = true)] async fn read() { tokio::time::timeout(delay, read()).await; }",
    ] {
        assert!(!inspect(path, source)?.is_empty(), "{source}");
    }
    Ok(())
}
