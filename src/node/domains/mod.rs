//! Module for executor and its domains

use std::path::Path;
use std::sync::{Arc, Weak};

use anyhow::Context;
use core_payments_domain_runtime::RelayerId;
use derivative::Derivative;
use derive_builder::Builder;
use domain_service::DomainConfiguration;
use futures::prelude::*;
use sc_client_api::BlockchainEvents;
use sc_service::ChainSpecExtension;
use serde::{Deserialize, Serialize};
use sp_domains::DomainId;
use subspace_runtime::Block;
use tracing_futures::Instrument;

use self::core::CoreDomainNode;
use self::eth::EthDomainNode;
use crate::node::{Base, BaseBuilder, BlockNotification};

pub(crate) mod chain_spec;
pub mod core;
pub mod eth;

/// System domain executor instance.
pub(crate) struct ExecutorDispatch;

impl sc_executor::NativeExecutionDispatch for ExecutorDispatch {
    // #[cfg(feature = "runtime-benchmarks")]
    // type ExtendHostFunctions = frame_benchmarking::benchmarking::HostFunctions;
    // #[cfg(not(feature = "runtime-benchmarks"))]
    type ExtendHostFunctions = ();

    fn dispatch(method: &str, data: &[u8]) -> Option<Vec<u8>> {
        system_domain_runtime::api::dispatch(method, data)
    }

    fn native_version() -> sc_executor::NativeVersion {
        system_domain_runtime::native_version()
    }
}

/// Node builder
#[derive(Debug, Clone, Derivative, Builder, Deserialize, Serialize, PartialEq)]
#[derivative(Default)]
#[builder(pattern = "immutable", build_fn(private, name = "_build"), name = "ConfigBuilder")]
#[non_exhaustive]
pub struct Config {
    /// Id of the relayer
    #[builder(setter(strip_option), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub relayer_id: Option<RelayerId>,
    #[doc(hidden)]
    #[builder(
        setter(into, strip_option),
        field(type = "BaseBuilder", build = "self.base.build()")
    )]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub base: Base,
    /// The core config
    #[builder(setter(strip_option), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub core: Option<core::Config>,
    /// The eth domain config
    #[builder(setter(strip_option), default)]
    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub eth: Option<eth::Config>,
}

crate::derive_base!(crate::node::Base => ConfigBuilder);

pub(crate) type FullClient =
    domain_service::FullClient<system_domain_runtime::RuntimeApi, ExecutorDispatch>;
pub(crate) type NewFull = domain_service::NewFullSystem<
    Arc<FullClient>,
    sc_executor::NativeElseWasmExecutor<ExecutorDispatch>,
    subspace_runtime_primitives::opaque::Block,
    crate::node::FullClient,
    system_domain_runtime::RuntimeApi,
    ExecutorDispatch,
>;
/// Chain spec of the system domain
pub type ChainSpec = chain_spec::ChainSpec;

/// System domain node
#[derive(Clone, Derivative)]
#[derivative(Debug)]
pub struct SystemDomainNode {
    #[derivative(Debug = "ignore")]
    _client: Weak<FullClient>,
    core: Option<CoreDomainNode>,
    eth: Option<EthDomainNode>,
    rpc_handlers: crate::utils::Rpc,
}

impl SystemDomainNode {
    pub(crate) async fn new(
        cfg: Config,
        directory: impl AsRef<Path>,
        chain_spec: ChainSpec,
        primary_new_full: &mut crate::node::NewFull,
    ) -> anyhow::Result<Self> {
        let Config { base, relayer_id: maybe_relayer_id, core, eth } = cfg;
        let extensions = chain_spec.extensions().clone();
        let service_config =
            base.configuration(directory.as_ref().join("system"), chain_spec).await;

        let system_domain_config = DomainConfiguration { service_config, maybe_relayer_id };
        let block_importing_notification_stream = primary_new_full
            .block_importing_notification_stream
            .subscribe()
            .then(|block_importing_notification| async move {
                (
                    block_importing_notification.block_number,
                    block_importing_notification.acknowledgement_sender,
                )
            });

        let new_slot_notification_stream = primary_new_full
            .new_slot_notification_stream
            .subscribe()
            .then(|slot_notification| async move {
                (
                    slot_notification.new_slot_info.slot,
                    slot_notification.new_slot_info.global_challenge,
                    None,
                )
            });

        let (gossip_msg_sink, gossip_msg_stream) =
            sc_utils::mpsc::tracing_unbounded("Cross domain gossip messages", 100);

        // TODO: proper value
        let block_import_throttling_buffer_size = 10;

        let executor_streams = domain_client_executor::ExecutorStreams {
            primary_block_import_throttling_buffer_size: block_import_throttling_buffer_size,
            block_importing_notification_stream,
            imported_block_notification_stream: primary_new_full
                .client
                .every_import_notification_stream(),
            new_slot_notification_stream,
            _phantom: Default::default(),
        };

        let system_domain_node = domain_service::new_full_system(
            system_domain_config,
            primary_new_full.client.clone(),
            primary_new_full.sync_service.clone(),
            &primary_new_full.select_chain,
            executor_streams,
            gossip_msg_sink.clone(),
        )
        .await?;

        let mut domain_tx_pool_sinks = std::collections::BTreeMap::new();

        let core = if let Some(core) = core {
            let span = tracing::info_span!("CoreDomain");
            let core_domain_id = u32::from(DomainId::CORE_PAYMENTS);
            CoreDomainNode::new(
                core,
                directory.as_ref().join(format!("core-{core_domain_id}")),
                extensions
                    .get_any(std::any::TypeId::of::<Option<core::ChainSpec>>())
                    .downcast_ref()
                    .cloned()
                    .flatten()
                    .ok_or_else(|| anyhow::anyhow!("Core domain is not supported"))?,
                primary_new_full,
                &system_domain_node,
                gossip_msg_sink.clone(),
                &mut domain_tx_pool_sinks,
            )
            .instrument(span)
            .await
            .map(Some)?
        } else {
            None
        };

        let eth = if let Some(eth) = eth {
            let span = tracing::info_span!("EthDomain");
            let eth_domain_id = u32::from(DomainId::CORE_ETH_RELAY);
            EthDomainNode::new(
                eth,
                directory.as_ref().join(format!("eth-{eth_domain_id}")),
                extensions
                    .get_any(std::any::TypeId::of::<Option<eth::ChainSpec>>())
                    .downcast_ref()
                    .cloned()
                    .flatten()
                    .ok_or_else(|| anyhow::anyhow!("Eth domain is not supported"))?,
                primary_new_full,
                &system_domain_node,
                gossip_msg_sink.clone(),
                &mut domain_tx_pool_sinks,
            )
            .instrument(span)
            .await
            .map(Some)?
        } else {
            None
        };

        domain_tx_pool_sinks.insert(DomainId::SYSTEM, system_domain_node.tx_pool_sink);
        primary_new_full.task_manager.add_child(system_domain_node.task_manager);

        let cross_domain_message_gossip_worker =
            cross_domain_message_gossip::GossipWorker::<Block>::new(
                primary_new_full.network_service.clone(),
                primary_new_full.sync_service.clone(),
                domain_tx_pool_sinks,
            );

        let NewFull { client, network_starter, rpc_handlers, .. } = system_domain_node;

        tokio::spawn(
            cross_domain_message_gossip_worker
                .run(gossip_msg_stream)
                .instrument(tracing::Span::current()),
        );
        network_starter.start_network();

        Ok(Self {
            _client: Arc::downgrade(&client),
            core,
            eth,
            rpc_handlers: crate::utils::Rpc::new(&rpc_handlers),
        })
    }

    pub(crate) fn _client(&self) -> anyhow::Result<Arc<FullClient>> {
        self._client.upgrade().ok_or_else(|| anyhow::anyhow!("The node was already closed"))
    }

    /// Get the core node handler
    pub fn core(&self) -> Option<CoreDomainNode> {
        self.core.clone()
    }

    /// Subscribe to new blocks imported
    pub async fn subscribe_new_blocks(
        &self,
    ) -> anyhow::Result<impl Stream<Item = BlockNotification> + Send + Sync + Unpin + 'static> {
        self.rpc_handlers.subscribe_new_blocks().await.context("Failed to subscribe to new blocks")
    }
}

crate::generate_builder!(Config);
