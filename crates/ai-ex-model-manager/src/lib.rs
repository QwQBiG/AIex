#![forbid(unsafe_code)]

mod catalog;
mod files;
mod manager;
mod transfer;

pub use catalog::{
    ArtifactSource, DistributionCatalog, DownloadLocation, ModelPackage, PackageSource,
    RuntimeManifest, RuntimePackage, RuntimeWorker,
};
pub use manager::{
    ModelManager, OperationPhase, OperationSnapshot, PackageKind, PackageLease, PackageSnapshot,
    PackageState,
};
