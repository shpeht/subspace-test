//! Crate with interfaces for SDK

#![warn(
    missing_docs,
    clippy::dbg_macro,
    clippy::unwrap_used,
    clippy::disallowed_types,
    unused_features
)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

/// Trait which abstracts farmer for node
#[async_trait::async_trait]
pub trait Farmer {
    /// Proof of space table
    type Table: subspace_proof_of_space::Table;

    /// Fetch piece by its hash
    async fn get_piece_by_hash(
        piece_index_hash: subspace_core_primitives::PieceIndexHash,
        piece_store: &sdk_dsn::PieceStore,
        weak_readers_and_pieces: &std::sync::Weak<
            parking_lot::Mutex<
                Option<subspace_farmer::utils::readers_and_pieces::ReadersAndPieces>,
            >,
        >,
        piece_memory_cache: &subspace_farmer_components::piece_caching::PieceMemoryCache,
    ) -> Option<subspace_core_primitives::Piece>;
}

/// Trait which abstracts node for farmer
pub trait Node {
    /// Client for aux store for DSN
    type Client: sc_client_api::AuxStore + Send + Sync + 'static;
    /// Proof of space table type
    type Table: subspace_proof_of_space::Table;
    /// Rpc implementation
    type Rpc: subspace_farmer::node_client::NodeClient + Clone;

    /// Node name in telemetry
    fn name(&self) -> &str;
    /// Shared dsn configuration
    fn dsn(&self) -> &sdk_dsn::DsnShared<Self::Client>;
    /// Rpc
    fn rpc(&self) -> &Self::Rpc;
}
