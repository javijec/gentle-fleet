//! Owned provider process. A Windows job belongs to exactly one launch;
//! it is never shared with the daemon, runner or an attached service.

use std::io;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus};

#[cfg(windows)]
mod windows_job;

pub struct OwnedChild {
    child: Child,
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
    #[cfg(windows)]
    job: windows_job::Job,
}

/// Keep std's argument escaping, environment and stdio handling. On Windows
/// the provider starts suspended and is assigned to its private job before
/// the initial thread is allowed to execute. Failure is closed, not a fallback
/// to launching an uncontained process.
pub fn spawn_owned(command: &mut Command) -> io::Result<OwnedChild> {
    #[cfg(windows)]
    let (mut child, job) = windows_job::spawn(command)?;
    #[cfg(not(windows))]
    let mut child = command.spawn()?;
    Ok(OwnedChild {
        stdin: child.stdin.take(),
        stdout: child.stdout.take(),
        stderr: child.stderr.take(),
        child,
        #[cfg(windows)]
        job,
    })
}

impl OwnedChild {
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// On Windows this terminates the entire private job, even after the
    /// original provider process has exited.
    pub fn kill(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        {
            self.job.terminate()
        }
        #[cfg(not(windows))]
        {
            if self.child.try_wait()?.is_none() {
                self.child.kill()?;
            }
            Ok(())
        }
    }

    /// On Windows require both root exit and a zero active-process count.
    /// A successful TerminateJobObject alone is not termination evidence.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        #[cfg(windows)]
        {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            loop {
                if let Some(status) = self.child.try_wait()? {
                    if self.job.active()? == 0 {
                        return Ok(status);
                    }
                }
                if std::time::Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "provider job did not become empty",
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        #[cfg(not(windows))]
        {
            self.child.wait()
        }
    }

    /// After a normal root exit, reject a surviving Windows descendant.
    /// Reap the owned job first, then report that shutdown was not orderly.
    pub fn ensure_tree_empty(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        {
            // Root exit and job accounting may become observable at different
            // instants. Give the job a small bounded grace period to drain.
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_millis(200);
            while self.job.active()? != 0 {
                if std::time::Instant::now() >= deadline {
                    self.kill()?;
                    self.wait()?;
                    return Err(io::Error::other("provider exited with active descendants"));
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        Ok(())
    }

    #[cfg(all(windows, test))]
    pub(super) fn root_id(&self) -> u32 {
        self.child.id()
    }

    #[cfg(all(windows, test))]
    pub(super) fn active_processes(&self) -> io::Result<u32> {
        self.job.active()
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        drop(self.stdin.take());
        // The job handle itself is a final kill-on-close backstop on Windows.
        // Unix retains the existing direct-child cleanup policy.
        let _ = self.kill();
        let _ = self.wait();
    }
}
