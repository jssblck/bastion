use std::ffi::OsString;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A monotonic counter making each container name unique within the process, so
/// concurrent reviewers (and a reviewer's reprompt) never collide on a name.
static CONTAINER_SEQ: AtomicU64 = AtomicU64::new(0);

/// A per-process random token mixed into every container name.
///
/// pid plus a sequence number is unique within one `bastion` process, but container
/// names are global to the daemon: two clients sharing a remote daemon, or a pid
/// reused over time, could otherwise mint the same name, and a losing run's armed
/// [`ContainerGuard`] would then `docker rm -f` a *different* client's live container.
/// A random token per process makes that collision vanishingly unlikely. The value is
/// a collision-avoidance nonce, not a security boundary, so a non-cryptographic source
/// (the entropy `RandomState` seeds its hasher keys from) is enough.
fn process_nonce() -> u64 {
    use std::hash::BuildHasher as _;
    static NONCE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *NONCE.get_or_init(|| std::collections::hash_map::RandomState::new().hash_one("bastion"))
}

/// A unique-per-invocation container name. The pid and per-process nonce keep two
/// `bastion` processes (even with a reused pid against a shared daemon) from
/// colliding, and the sequence number keeps invocations within one process distinct.
pub(crate) fn unique_container_name() -> String {
    let seq = CONTAINER_SEQ.fetch_add(1, Ordering::Relaxed);
    format!(
        "bastion-reviewer-{}-{:016x}-{seq}",
        std::process::id(),
        process_nonce()
    )
}

/// How long the teardown waits for `rm -f` before giving up.
///
/// The teardown fires when a reviewer is already timed out, so it must not block on an
/// unresponsive engine: a hung `docker rm` must not stall anything indefinitely. The
/// wait runs on its own thread (see the guard's `Drop`) and waits up to this bound for
/// the container to be removed (so a healthy engine finishes the teardown promptly),
/// then kills the `rm` process and moves on best-effort.
const TEARDOWN_BUDGET: Duration = Duration::from_secs(10);

/// Force-removes a named container when dropped, unless defused.
///
/// Held across the inner `docker run` await so that cancelling the run future (the
/// runner's timeout path) tears the container down. On a clean return the caller
/// defuses it, since `docker run --rm` has already removed the container.
pub(crate) struct ContainerGuard {
    program: OsString,
    name: String,
    armed: bool,
}

impl ContainerGuard {
    /// Arm a guard that will force-remove `name` (via `program rm -f`) on drop.
    pub(crate) fn new(program: OsString, name: String) -> Self {
        Self {
            program,
            name,
            armed: true,
        }
    }

    /// Disarm the guard after a clean run so its drop does no work.
    pub(crate) fn defuse(mut self) {
        self.armed = false;
    }
}

impl Drop for ContainerGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Launch `rm -f` synchronously here, then move only the bounded *wait* off
        // this thread. Two constraints pull in opposite directions: this `Drop` runs
        // on Bastion's current-thread Tokio runtime, so the wait (poll up to
        // `TEARDOWN_BUDGET`) must not block it, or it would stall every other reviewer
        // and the timed-out one; but a fully detached cleanup could be lost entirely,
        // because a detached thread is not guaranteed to have run before the process
        // exits (a timed-out reviewer can be the last work, and the CLI would return
        // and terminate first). Spawning the child *before* returning resolves both:
        // the removal is already in flight in the engine when `Drop` returns, even if
        // Bastion exits immediately, and a spawned child is an independent process that
        // outlives us. Only the optional wait/kill (for a *hung* `rm`) rides a detached
        // thread, off the runtime.
        let Ok(child) = std::process::Command::new(&self.program)
            .arg("rm")
            .arg("-f")
            .arg(&self.name)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return;
        };
        // `Builder::spawn` rather than `thread::spawn` so a failure to create the thread
        // (resource exhaustion) returns an error instead of panicking in `Drop`. The
        // `rm` child has already launched, so dropping the wait here is still
        // best-effort safe: the removal proceeds, we simply forgo bounding a hung one.
        let _ = std::thread::Builder::new()
            .name("bastion-container-teardown".to_string())
            .spawn(move || wait_bounded(child));
    }
}

/// Wait for an already-spawned `rm -f` child, bounded by [`TEARDOWN_BUDGET`].
///
/// `rm -f` on a `--rm` container that already exited is a harmless no-op; on a
/// container still running after a timeout it stops the hung agent. The child is
/// already running (the guard's `Drop` spawned it); this only bounds the wait so an
/// unresponsive engine cannot leave the `rm` running forever: past the budget the `rm`
/// is killed and we give up. Errors are ignored, since a missing engine or container
/// leaves nothing to clean up.
fn wait_bounded(mut child: std::process::Child) {
    let deadline = Instant::now() + TEARDOWN_BUDGET;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_names_are_unique() {
        let a = unique_container_name();
        let b = unique_container_name();
        assert_ne!(a, b);
        assert!(a.starts_with("bastion-reviewer-"));
    }
}
