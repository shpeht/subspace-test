use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use derivative::Derivative;
use derive_builder::Builder;
use derive_more::{Deref, DerefMut, Display, From};
use either::*;
use futures::prelude::*;
use libp2p_core::Multiaddr;
use sc_network_common::config::MultiaddrWithPeerId;
use serde::{Deserialize, Serialize};
use subspace_farmer::utils::readers_and_pieces::ReadersAndPieces;
use subspace_farmer_components::piece_caching::PieceMemoryCache;
use subspace_networking::{
    PieceByHashRequest, PieceByHashRequestHandler, PieceByHashResponse,
    SegmentHeaderBySegmentIndexesRequestHandler,
};
use subspace_service::segment_headers::SegmentHeaderCache;

use super::provider_storage_utils::MaybeProviderStorage;
use super::{FarmerProviderStorage, NodePieceCache, NodeProviderStorage, ProviderStorage};
use crate::node::PieceCacheSize;
use crate::utils::DropCollection;
use crate::{farmer, node};

/// Wrapper with default value for listen address
#[derive(
    Debug, Clone, Derivative, Deserialize, Serialize, PartialEq, Eq, From, Deref, DerefMut,
)]
#[derivative(Default)]
#[serde(transparent)]
pub struct ListenAddresses(
    #[derivative(Default(
        // TODO: get rid of it, once it won't be required by monorepo
        value = "vec![\"/ip4/127.0.0.1/tcp/0\".parse().expect(\"Always valid\")]"
    ))]
    pub(crate) Vec<Multiaddr>,
);

/// Wrapper with default value for number of incoming connections
#[derive(
    Debug, Clone, Derivative, Deserialize, Serialize, PartialEq, Eq, From, Deref, DerefMut, Display,
)]
#[derivative(Default)]
#[serde(transparent)]
pub struct InConnections(#[derivative(Default(value = "100"))] pub(crate) u32);

/// Wrapper with default value for number of outgoing connections
#[derive(
    Debug, Clone, Derivative, Deserialize, Serialize, PartialEq, Eq, From, Deref, DerefMut, Display,
)]
#[derivative(Default)]
#[serde(transparent)]
pub struct OutConnections(#[derivative(Default(value = "100"))] pub(crate) u32);

/// Wrapper with default value for number of target connections
#[derive(
    Debug, Clone, Derivative, Deserialize, Serialize, PartialEq, Eq, From, Deref, DerefMut, Display,
)]
#[derivative(Default)]
#[serde(transparent)]
pub struct TargetConnections(#[derivative(Default(value = "50"))] pub(crate) u32);

/// Wrapper with default value for number of pending incoming connections
#[derive(
    Debug, Clone, Derivative, Deserialize, Serialize, PartialEq, Eq, From, Deref, DerefMut, Display,
)]
#[derivative(Default)]
#[serde(transparent)]
pub struct PendingInConnections(#[derivative(Default(value = "100"))] pub(crate) u32);

/// Wrapper with default value for number of pending outgoing connections
#[derive(
    Debug, Clone, Derivative, Deserialize, Serialize, PartialEq, Eq, From, Deref, DerefMut, Display,
)]
#[derivative(Default)]
#[serde(transparent)]
pub struct PendingOutConnections(#[derivative(Default(value = "100"))] pub(crate) u32);

/// Node DSN builder
#[derive(Debug, Clone, Derivative, Builder, Deserialize, Serialize, PartialEq)]
#[derivative(Default)]
#[builder(pattern = "immutable", build_fn(private, name = "_build"), name = "DsnBuilder")]
#[non_exhaustive]
pub struct Dsn {
    /// Listen on some address for other nodes
    #[builder(default, setter(into, strip_option))]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub provider_storage_path: Option<std::path::PathBuf>,
    /// Listen on some address for other nodes
    #[builder(default, setter(into))]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub listen_addresses: ListenAddresses,
    /// Boot nodes
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boot_nodes: Vec<MultiaddrWithPeerId>,
    /// Reserved nodes
    #[builder(default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub reserved_nodes: Vec<Multiaddr>,
    /// Determines whether we allow keeping non-global (private, shared,
    /// loopback..) addresses in Kademlia DHT.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub allow_non_global_addresses_in_dht: bool,
    /// Defines max established incoming swarm connection limit.
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub in_connections: InConnections,
    /// Defines max established outgoing swarm connection limit.
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub out_connections: OutConnections,
    /// Pending incoming swarm connection limit.
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub pending_in_connections: PendingInConnections,
    /// Pending outgoing swarm connection limit.
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub pending_out_connections: PendingOutConnections,
    /// Defines target total (in and out) connection number for DSN that
    /// should be maintained.
    #[builder(setter(into), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub target_connections: TargetConnections,
}

crate::generate_builder!(Dsn);

impl DsnBuilder {
    /// Dev chain configuration
    pub fn dev() -> Self {
        Self::new().allow_non_global_addresses_in_dht(true)
    }

    /// Gemini 3d configuration
    pub fn gemini_3d() -> Self {
        Self::new().listen_addresses(vec![
            "/ip6/::/tcp/30433".parse().expect("hardcoded value is true"),
            "/ip4/0.0.0.0/tcp/30433".parse().expect("hardcoded value is true"),
        ])
    }

    /// Gemini 3d configuration
    pub fn devnet() -> Self {
        Self::new().listen_addresses(vec![
            "/ip6/::/tcp/30433".parse().expect("hardcoded value is true"),
            "/ip4/0.0.0.0/tcp/30433".parse().expect("hardcoded value is true"),
        ])
    }
}

const MAX_PROVIDER_RECORDS_LIMIT: NonZeroUsize = NonZeroUsize::new(100000).expect("100000 > 0"); // ~ 10 MB

pub(crate) struct DsnOptions<C: sc_client_api::AuxStore, ASNS> {
    pub(crate) client: Arc<C>,
    pub(crate) node_name: String,
    pub(crate) archived_segment_notification_stream: ASNS,
    pub(crate) piece_cache_size: PieceCacheSize,
    pub(crate) base_path: PathBuf,
    pub(crate) keypair: subspace_networking::libp2p::identity::Keypair,
}

#[derive(Derivative)]
#[derivative(Debug)]
pub(crate) struct DsnShared<C: sc_client_api::AuxStore + Send + Sync + 'static> {
    pub(crate) node: subspace_networking::Node,
    pub(crate) farmer_readers_and_pieces: Arc<parking_lot::Mutex<Option<ReadersAndPieces>>>,
    #[derivative(Debug = "ignore")]
    pub(crate) farmer_piece_store: Arc<
        tokio::sync::Mutex<
            Option<
                subspace_farmer::utils::parity_db_store::ParityDbStore<
                    subspace_networking::libp2p::kad::record::Key,
                    subspace_core_primitives::Piece,
                >,
            >,
        >,
    >,
    pub(crate) farmer_provider_storage: MaybeProviderStorage<FarmerProviderStorage>,
    #[derivative(Debug = "ignore")]
    pub(crate) piece_cache: NodePieceCache<C>,
    #[derivative(Debug = "ignore")]
    pub(crate) piece_memory_cache: PieceMemoryCache,

    _drop: DropCollection,
}

impl Dsn {
    pub(crate) fn build_dsn<C, ASNS>(
        self,
        options: DsnOptions<C, ASNS>,
    ) -> anyhow::Result<(DsnShared<C>, subspace_networking::NodeRunner<ProviderStorage<C>>)>
    where
        C: sc_client_api::AuxStore + Send + Sync + 'static,
        ASNS: Stream<Item = sc_consensus_subspace::ArchivedSegmentNotification>
            + Unpin
            + Send
            + 'static,
    {
        let DsnOptions {
            mut archived_segment_notification_stream,
            node_name,
            client,
            base_path,
            keypair,
            piece_cache_size,
        } = options;
        let farmer_readers_and_pieces = Arc::new(parking_lot::Mutex::new(None));
        let farmer_piece_store = Arc::new(tokio::sync::Mutex::new(None));
        let farmer_provider_storage = MaybeProviderStorage::none();
        let piece_memory_cache = PieceMemoryCache::default();

        let piece_cache = NodePieceCache::new(
            client.clone(),
            piece_cache_size.as_u64() / subspace_core_primitives::Piece::SIZE as u64,
            subspace_networking::peer_id(&keypair),
        );

        // Start before archiver, so we don't have potential race condition and
        // miss pieces
        tokio::task::Builder::new()
            .name(format!("subspace-sdk-node-{node_name}-piece-caching").as_ref())
            .spawn({
                let mut piece_cache = piece_cache.clone();

                async move {
                    while let Some(archived_segment_notification) =
                        archived_segment_notification_stream.next().await
                    {
                        let segment_index = archived_segment_notification
                            .archived_segment
                            .segment_header
                            .segment_index();
                        if let Err(error) = piece_cache.add_pieces(
                            segment_index.first_piece_index(),
                            &archived_segment_notification.archived_segment.pieces,
                        ) {
                            tracing::error!(
                                %segment_index,
                                %error,
                                "Failed to store pieces for segment in cache"
                            );
                        }
                    }
                }
            })?;

        let Self {
            listen_addresses,
            reserved_nodes,
            allow_non_global_addresses_in_dht,
            provider_storage_path,
            in_connections: InConnections(max_established_incoming_connections),
            out_connections: OutConnections(max_established_outgoing_connections),
            target_connections: TargetConnections(target_connections),
            pending_in_connections: PendingInConnections(max_pending_incoming_connections),
            pending_out_connections: PendingOutConnections(max_pending_outgoing_connections),
            boot_nodes,
        } = self;

        let peer_id = subspace_networking::peer_id(&keypair);
        let bootstrap_nodes = boot_nodes
            .into_iter()
            .map(|a| {
                a.to_string().parse().expect("Convertion between 2 libp2p version. Never panics")
            })
            .collect::<Vec<_>>();

        let listen_on = listen_addresses
            .0
            .into_iter()
            .map(|a| {
                a.to_string().parse().expect("Convertion between 2 libp2p version. Never panics")
            })
            .collect();

        let networking_parameters_registry = subspace_networking::NetworkingParametersManager::new(
            &base_path.join("known_addresses_db"),
            bootstrap_nodes,
        )
        .context("Failed to open known addresses database for DSN")?
        .boxed();

        let external_provider_storage = match provider_storage_path {
            Some(path) => Either::Left(subspace_networking::ParityDbProviderStorage::new(
                &path,
                MAX_PROVIDER_RECORDS_LIMIT,
                peer_id,
            )?),
            None => Either::Right(subspace_networking::MemoryProviderStorage::new(peer_id)),
        };

        let node_provider_storage =
            NodeProviderStorage::new(peer_id, piece_cache.clone(), external_provider_storage);
        let provider_storage =
            ProviderStorage::new(farmer_provider_storage.clone(), node_provider_storage);

        let config = subspace_networking::Config {
            keypair,
            listen_on,
            allow_non_global_addresses_in_dht,
            networking_parameters_registry,
            request_response_protocols: vec![
                PieceByHashRequestHandler::create({
                    let weak_readers_and_pieces = Arc::downgrade(&farmer_readers_and_pieces);
                    let farmer_piece_store = Arc::clone(&farmer_piece_store);
                    let piece_cache = piece_cache.clone();
                    let piece_memory_cache = piece_memory_cache.clone();

                    move |&PieceByHashRequest { piece_index_hash }| {
                        let weak_readers_and_pieces = weak_readers_and_pieces.clone();
                        let farmer_piece_store = Arc::clone(&farmer_piece_store);
                        let piece_cache = piece_cache.clone();
                        let node_piece_by_hash =
                            node::get_piece_by_hash(piece_index_hash, &piece_cache);
                        let piece_memory_cache = piece_memory_cache.clone();

                        async move {
                            match node_piece_by_hash {
                                Some(PieceByHashResponse { piece: None }) | None => (),
                                result => return result,
                            }

                            if let Some(piece_store) = farmer_piece_store.lock().await.as_ref() {
                                farmer::get_piece_by_hash(
                                    piece_index_hash,
                                    piece_store,
                                    &weak_readers_and_pieces,
                                    &piece_memory_cache,
                                )
                                .await
                            } else {
                                None
                            }
                        }
                    }
                }),
                SegmentHeaderBySegmentIndexesRequestHandler::create({
                    let segment_header_cache = SegmentHeaderCache::new(client);
                    move |req| {
                        futures::future::ready(node::get_segment_header_by_segment_indexes(
                            req,
                            &segment_header_cache,
                        ))
                    }
                }),
            ],
            provider_storage,
            reserved_peers: reserved_nodes
                .into_iter()
                .map(|addr| {
                    addr.to_string()
                        .parse()
                        .expect("Conversion between 2 libp2p versions is always right")
                })
                .collect(),
            max_established_incoming_connections,
            max_established_outgoing_connections,
            target_connections,
            max_pending_incoming_connections,
            max_pending_outgoing_connections,
            ..subspace_networking::Config::default()
        };

        let (node, runner) = subspace_networking::create(config)?;

        let mut drop_collection = DropCollection::new();
        let on_new_listener = node.on_new_listener(Arc::new({
            let node = node.clone();

            move |address| {
                tracing::info!(
                    "DSN listening on {}",
                    address.clone().with(subspace_networking::libp2p::multiaddr::Protocol::P2p(
                        node.id().into()
                    ))
                );
            }
        }));
        drop_collection.push(on_new_listener);

        Ok((
            DsnShared {
                node,
                farmer_piece_store,
                farmer_provider_storage,
                farmer_readers_and_pieces,
                piece_cache,
                piece_memory_cache,
                _drop: drop_collection,
            },
            runner,
        ))
    }
}
