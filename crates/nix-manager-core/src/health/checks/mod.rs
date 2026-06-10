//! Reusable, domain-agnostic checks. Each struct in this module implements
//! `Check` and is parameterised over the bits a caller would reasonably want
//! to vary (paths, ports, thresholds, ...). Domain-specific compositions live
//! outside this module.

mod advisory;
mod filesystem;
mod host;
mod local;
mod network;
mod systemd;

pub use advisory::ConstantAdvisory;
pub use filesystem::{FileSecret, StoragePath, ZeroByteFileScan};
pub use host::HostIdentity;
pub use local::{CommandPresent, Sudo};
pub use network::{HttpProbe, TcpListener};
pub use systemd::SystemdUnit;
