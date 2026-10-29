#![forbid(unsafe_code)]

mod manifest;
mod path;
mod validation;
mod verification;

#[cfg(test)]
mod tests;

pub use manifest::{
    Artifact, ArtifactKind, License, ModelBackend, ModelCapability, ModelCatalog, ModelManifest,
    RuntimeRequirement,
};
pub use path::validate_relative_path;
pub use verification::{
    ArtifactIssue, ArtifactIssueKind, DirectoryVerification, PreparationStatus, verify_directory,
};

pub const CATALOG_SCHEMA_VERSION: u16 = 1;
