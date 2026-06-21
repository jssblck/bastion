//! Thin binary entrypoint. All logic lives in the `bastion` library crate so it
//! can be tested without spawning a process; `main` only wires the async runtime
//! to [`bastion::run`].

// Single-threaded (current-thread) runtime: reviewers are I/O-bound — they shell
// out to agent backends and await — so concurrency, not parallelism, is what the
// runner needs. A current-thread runtime keeps the process lean and avoids a
// worker pool the workload would not use.
#[tokio::main(flavor = "current_thread")]
async fn main() -> color_eyre::Result<std::process::ExitCode> {
    bastion::run().await
}
