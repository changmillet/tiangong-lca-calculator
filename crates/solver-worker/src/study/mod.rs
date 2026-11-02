//! Bounded source-model compiler and executor; queue, ACL and storage belong to W06.
mod compile;
pub mod contracts;
mod identity;
mod source;
pub use compile::{VerifiedMethods, execute, prepare};
pub use identity::{artifact_bytes, decode_prepared, semantic_hash, sha256};
#[cfg(test)]
mod tests;

pub mod runtime_child;
pub mod runtime_contracts;

pub mod runtime_process;

pub mod runtime;

pub mod runtime_preflight;

pub mod json;
