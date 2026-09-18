//! macOS child wait and pipe tests.
use tokio::io::AsyncReadExt as _;

use super::*;

fn short_lived_child() -> MacChild {
    // SAFETY: the child calls only _exit before returning to Rust code.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "{}", io::Error::last_os_error());
    if pid == 0 {
        // SAFETY: _exit terminates the fork child without Rust cleanup.
        unsafe { libc::_exit(0) };
    }
    MacChild {
        state: Arc::new(Mutex::new(MacProcessState { pid: Some(pid) })),
        waiter: None,
        stdout: None,
        stderr: None,
    }
}

#[tokio::test]
async fn cancelled_wait_never_signals_after_the_waiter_reaps() {
    for _ in 0..32 {
        let mut child = short_lived_child();
        let mut wait = Box::pin(child.wait());
        let completed = tokio::select! {
            biased;
            _ = tokio::time::sleep(Duration::from_millis(1)) => false,
            status = &mut wait => {
                status.unwrap();
                true
            }
        };
        drop(wait);
        if !completed {
            child.terminate().unwrap();
            child.wait().await.unwrap();
        }
        assert!(lock_process_state(&child.state).unwrap().pid.is_none());
    }
}

#[tokio::test]
async fn dropping_pending_pipe_read_closes_the_descriptor() {
    let (read, write) = cloexec_pipe().unwrap();
    let raw = read.as_raw_fd();
    let task = tokio::spawn(async move {
        let mut pipe = AsyncPipe::new(read).unwrap();
        let mut byte = [0_u8; 1];
        let _ = pipe.read(&mut byte).await;
    });
    tokio::task::yield_now().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());

    // SAFETY: F_GETFD only probes whether the numeric descriptor is live.
    assert_eq!(unsafe { libc::fcntl(raw, libc::F_GETFD) }, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
    drop(write);
}

#[test]
fn spawn_launch_path_is_hold_fd_not_the_cloexec_source() {
    let path = spawn_launch_path();
    let hold = format!("/dev/fd/{HOLD_FD}");
    let source = format!("/dev/fd/{MIN_SPAWN_SOURCE_FD}");
    assert_eq!(path.as_bytes(), hold.as_bytes());
    assert_eq!(HOLD_FD, 3);
    const { assert!(MIN_SPAWN_SOURCE_FD > HOLD_FD) };
    assert_ne!(
        path.as_bytes(),
        source.as_bytes(),
        "launch path must not name the CLOEXEC O_EXEC source"
    );
}
