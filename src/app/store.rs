//! Persistence of the account's aggregates behind one trait per aggregate, so a cloud account
//! store can later stand next to the local profile (organization and cloud storage design, phase
//! 2). Each trait has a shared contract suite that every implementation must pass; the local
//! profile is the default implementation and behaves exactly as the files always did.

pub(crate) mod settings;
