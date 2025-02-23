//! Subspace SDK for easy running of both Subspace node and farmer

#![warn(
    missing_docs,
    clippy::dbg_macro,
    clippy::unwrap_used,
    clippy::disallowed_types,
    unused_features
)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

/// Module related to the farmer
pub use sdk_farmer::{Builder as FarmerBuilder, Info as FarmerInfo, PlotDescription};
pub use sdk_node::{chain_spec, Builder as NodeBuilder, Info as NodeInfo};
pub use sdk_utils::{ByteSize, Multiaddr, MultiaddrWithPeerId, PublicKey, Ss58ParsingError};
use subspace_proof_of_space::chia::ChiaTable;

static_assertions::assert_impl_all!(Node: Send, Sync);
static_assertions::assert_impl_all!(Farmer: Send, Sync);
static_assertions::assert_impl_all!(Plot: Send, Sync);

/// Subspace farmer type
pub type Farmer = sdk_farmer::Farmer<ChiaTable>;
/// Subspace farmer's plot
pub type Plot = sdk_farmer::Plot<ChiaTable>;
/// Subspace primary node
pub type Node = sdk_node::Node<Farmer>;

/// Farmer related things located here
pub mod farmer {
    pub use sdk_farmer::{CacheDescription, PlotDescription};

    pub use super::{Farmer, Plot};
}

/// Node related things located here
pub mod node {
    pub use sdk_dsn::*;
    pub use sdk_node::chain_spec::ChainSpec;
    pub use sdk_node::{chain_spec, domains, Event, RewardsEvent, SubspaceEvent};
    pub use sdk_substrate::*;

    pub use super::Node;
}
