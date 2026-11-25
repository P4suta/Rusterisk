#[path = "../../cli/tests/support/time.rs"]
mod time;
pub use time::Wait;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    future::Future,
    io,
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, ExitStatus},
};
use tokio::sync::watch;
use wait_timeout::ChildExt;

fn expired() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "fixture readiness deadline expired",
    )
}

pub fn block_on<T>(future: impl Future<Output = io::Result<T>>) -> io::Result<T> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(future)
}

pub struct DirectoryChanges {
    _watcher: RecommendedWatcher,
    events: watch::Receiver<Result<(), String>>,
}

impl DirectoryChanges {
    pub fn new(directory: &Path) -> io::Result<Self> {
        let (events, receiver) = watch::channel(Ok(()));
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if matches!(&event, Ok(event) if event.kind.is_access()) {
                    return;
                }
                events.send_if_modified(|state| {
                    if state.is_err() {
                        return false;
                    }
                    *state = event.map(|_| ()).map_err(|error| error.to_string());
                    true
                });
            })
            .map_err(io::Error::other)?;
        watcher
            .watch(directory, RecursiveMode::NonRecursive)
            .map_err(io::Error::other)?;
        Ok(Self {
            _watcher: watcher,
            events: receiver,
        })
    }

    pub async fn changed(&mut self) -> io::Result<()> {
        self.events.changed().await.map_err(io::Error::other)?;
        self.events
            .borrow_and_update()
            .clone()
            .map_err(io::Error::other)
    }
}

pub fn file_ready<T>(
    directory: &Path,
    mut read: impl FnMut() -> io::Result<Option<T>>,
) -> io::Result<T> {
    let mut changes = DirectoryChanges::new(directory)?;
    block_on(async {
        Wait::Ready
            .bounded(async {
                loop {
                    if let Some(value) = read()? {
                        return Ok(value);
                    }
                    changes.changed().await?;
                }
            })
            .await
            .map_err(|_| expired())?
    })
}

pub fn accept(listener: &TcpListener, wait: Wait) -> io::Result<TcpStream> {
    listener.set_nonblocking(true)?;
    block_on(async {
        let listener = tokio::net::TcpListener::from_std(listener.try_clone()?)?;
        let (stream, _) = wait
            .bounded(listener.accept())
            .await
            .map_err(|_| expired())??;
        let stream = stream.into_std()?;
        stream.set_nonblocking(false)?;
        Ok(stream)
    })
}

pub fn child_exit(child: &mut Child, wait: Wait) -> io::Result<ExitStatus> {
    child.wait_timeout(wait.duration())?.ok_or_else(expired)
}

#[cfg(unix)]
pub fn child_state<T, E: From<io::Error>>(
    mut check: impl FnMut() -> Result<Option<T>, E>,
) -> Result<T, E> {
    use std::{io::Read, os::unix::net::UnixStream, time::Instant};
    let (mut reader, writer) = UnixStream::pair()?;
    let registration =
        signal_hook::low_level::pipe::register(signal_hook::consts::SIGCHLD, writer)?;
    let deadline = Instant::now() + Wait::Ready.duration();
    let result = (|| loop {
        if let Some(value) = check()? {
            return Ok(value);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(expired().into());
        }
        reader.set_read_timeout(Some(remaining))?;
        reader.read_exact(&mut [0; 1])?;
    })();
    if !signal_hook::low_level::unregister(registration) {
        return Err(io::Error::other("child signal registration disappeared").into());
    }
    result
}

#[cfg(windows)]
pub fn signaled(handle: &impl std::os::windows::io::AsHandle, wait: Wait) -> io::Result<()> {
    match win_custody::wait(handle, Some(wait.duration()))? {
        win_custody::WaitOutcome::Signaled => Ok(()),
        win_custody::WaitOutcome::TimedOut => Err(expired()),
        win_custody::WaitOutcome::Abandoned => {
            Err(io::Error::other("fixture process wait was abandoned"))
        }
    }
}

#[cfg(windows)]
pub fn process_handle(pid: u32) -> io::Result<impl std::os::windows::io::AsHandle> {
    win_custody::Process::open(pid, win_custody::ProcessAccess::Synchronize)
}

pub async fn virtual_window<F: std::future::IntoFuture>(
    duration: std::time::Duration,
    future: F,
) -> Result<F::Output, tokio::time::error::Elapsed> {
    tokio::time::advance(std::time::Duration::ZERO).await;
    tokio::time::timeout(duration, future).await
}
