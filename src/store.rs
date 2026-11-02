//! Persistence of the account's aggregates behind one trait per aggregate, so a cloud account
//! store can later stand next to the local profile (organization and cloud storage design, phase
//! 2). Each trait has a shared contract suite that every implementation must pass; the local
//! profile is the default implementation and behaves exactly as the files always did. The module
//! is at the crate's root because the message pipeline uses it as much as the host does.

pub mod notes;
pub(crate) mod settings;
