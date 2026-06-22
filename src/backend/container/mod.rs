//! Container provisioning for reviewers that declare a `runner`.
//!
//! A reviewer with a `runner` block runs its backend inside a container instead of
//! natively on the host. This module is the seam that makes that real:
//!
//! - [`ExecutionPlan::resolve`] parses a reviewer's `runner` + `capabilities` into
//!   either a [`Native`](ExecutionPlan::Native) or a
//!   [`Container`](ExecutionPlan::Container) plan, failing closed on a capability
//!   tier this build still does not provision (`mcp`, `skills`, and a native
//!   `network: true`). A value of this type is the proof a reviewer is runnable
//!   today, so [`dispatch`](super::dispatch) cannot reach a backend without one.
//! - [`ContainerPlan::ensure_image`] builds the image from a Dockerfile (or uses a
//!   prebuilt `image`) through the same [`CommandRunner`](super::command::CommandRunner)
//!   seam the backends use.
//! - [`ContainerRunner`] wraps the backend's [`CommandSpec`](super::command::CommandSpec)
//!   into a `docker run`
//!   invocation. The backend above is untouched: it builds the same logical spec,
//!   and this decorator decides it runs in a container.
//!
//! Network is only partially honored: a containerized `network: true` gets general
//! egress (the container has a network), but egress *restriction* for the default
//! `network: false` (a provider-only allowlist) is a later milestone, so both
//! attach the engine's default network today. See the honored-fields table in
//! `docs/developer-guide/backends.md`.

mod credentials;
mod plan;
mod runner;
mod teardown;

#[cfg(test)]
pub(crate) mod testutil;

pub use credentials::credential_passthrough;
pub use plan::{ContainerPlan, ExecutionPlan, ImageReference};
pub use runner::ContainerRunner;

use std::ffi::{OsStr, OsString};

use super::command::resolve_program;

/// Environment variable that overrides the container engine program. Defaults to
/// `docker`; `podman` is a drop-in replacement.
pub const ENGINE_ENV: &str = "BASTION_CONTAINER_ENGINE";

/// The default container engine program, resolved on `PATH` when [`ENGINE_ENV`] is
/// unset.
pub const DEFAULT_ENGINE: &str = "docker";

/// The path the checkout is bind-mounted at, and the working directory the agent
/// runs in, inside the container.
pub(crate) const WORKDIR: &str = "/workspace";

/// The container engine CLI Bastion shells out to (`docker` by default).
#[derive(Debug, Clone)]
pub struct ContainerEngine {
    program: OsString,
}

impl ContainerEngine {
    /// Resolve the engine program from [`ENGINE_ENV`], falling back to
    /// [`DEFAULT_ENGINE`] on `PATH`.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            program: resolve_program(DEFAULT_ENGINE, ENGINE_ENV),
        }
    }

    /// Build an engine with an explicit program path, bypassing the environment
    /// lookup. Used by tests that point at a fake engine.
    #[must_use]
    pub fn with_program(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// Borrow the resolved engine program.
    #[must_use]
    pub fn program(&self) -> &OsStr {
        &self.program
    }
}
