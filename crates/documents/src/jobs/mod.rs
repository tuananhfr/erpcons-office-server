//! Background work of the document service, run on an interval by the server binary.
mod orphans;
mod purge;

pub use orphans::{OrphanConfig, OrphanReport};
pub use purge::{PurgeConfig, PurgeReport};
