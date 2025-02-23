//! Utilities crate shared across all SDK crates

#![warn(
    missing_docs,
    clippy::dbg_macro,
    clippy::unwrap_used,
    clippy::disallowed_types,
    unused_features
)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

use std::pin::Pin;
use std::sync::Arc;

use anyhow::Context;
use derive_more::{Deref, DerefMut, Display, From, FromStr, Into};
use futures::prelude::*;
use jsonrpsee_core::client::{
    BatchResponse, ClientT, Subscription, SubscriptionClientT, SubscriptionKind,
};
use jsonrpsee_core::params::BatchRequestBuilder;
use jsonrpsee_core::server::rpc_module::RpcModule;
use jsonrpsee_core::traits::ToRpcParams;
use jsonrpsee_core::Error;
use parity_scale_codec::{Decode, Encode};
pub use parse_ss58::Ss58ParsingError;
use sc_rpc_api::state::StateApiClient;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use subspace_core_primitives::PUBLIC_KEY_LENGTH;

mod rpc_client;

/// Rpc implementation over jsonrpsee_core debug rpc module
#[derive(Clone, Debug)]
pub struct Rpc {
    inner: Arc<RpcModule<()>>,
}

impl Rpc {
    /// Constructor for our rpc from substrate rpc handlers
    pub fn new(handlers: &sc_service::RpcHandlers) -> Self {
        let inner = handlers.handle();
        Self { inner }
    }

    /// Subscribe to new block headers
    pub async fn subscribe_new_heads<'a, 'b, T>(
        &'a self,
    ) -> Result<impl Stream<Item = T::Header> + Send + Sync + Unpin + 'static, Error>
    where
        T: frame_system::Config + sp_runtime::traits::GetRuntimeBlockType,
        T::RuntimeBlock: serde::de::DeserializeOwned + sp_runtime::DeserializeOwned + 'static,
        T::Header: serde::de::DeserializeOwned + sp_runtime::DeserializeOwned + 'static,
        'a: 'b,
    {
        let stream = sc_rpc::chain::ChainApiClient::<
            T::BlockNumber,
            T::Hash,
            T::Header,
            sp_runtime::generic::SignedBlock<T::RuntimeBlock>,
        >::subscribe_new_heads(self)
        .await?
        .filter_map(|result| futures::future::ready(result.ok()));

        Ok(stream)
    }

    /// Subscribe to new finalized block headers
    pub async fn subscribe_finalized_heads<'a, 'b, T>(
        &'a self,
    ) -> Result<impl Stream<Item = T::Header> + Send + Sync + Unpin + 'static, Error>
    where
        T: frame_system::Config + sp_runtime::traits::GetRuntimeBlockType,
        T::RuntimeBlock: serde::de::DeserializeOwned + sp_runtime::DeserializeOwned + 'static,
        T::Header: serde::de::DeserializeOwned + sp_runtime::DeserializeOwned + 'static,
        'a: 'b,
    {
        let stream = sc_rpc::chain::ChainApiClient::<
            T::BlockNumber,
            T::Hash,
            T::Header,
            sp_runtime::generic::SignedBlock<T::RuntimeBlock>,
        >::subscribe_finalized_heads(self)
        .await?
        .filter_map(|result| futures::future::ready(result.ok()));

        Ok(stream)
    }

    /// Get substrate events for some block
    pub async fn get_events<T>(
        &self,
        block: Option<T::Hash>,
    ) -> anyhow::Result<Vec<frame_system::EventRecord<T::RuntimeEvent, T::Hash>>>
    where
        T: frame_system::Config,
        T::Hash: serde::ser::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
        Vec<frame_system::EventRecord<T::RuntimeEvent, T::Hash>>: parity_scale_codec::Decode,
    {
        match self
            .get_storage::<T::Hash>(StorageKey::events(), block)
            .await
            .context("Failed to get events from storage")?
        {
            Some(sp_storage::StorageData(events)) =>
                parity_scale_codec::DecodeAll::decode_all(&mut events.as_ref())
                    .context("Failed to decode events"),
            None => Ok(vec![]),
        }
    }
}

#[async_trait::async_trait]
impl ClientT for Rpc {
    async fn notification<Params>(&self, method: &str, params: Params) -> Result<(), Error>
    where
        Params: ToRpcParams + Send,
    {
        self.inner.call(method, params).await
    }

    async fn request<R, Params>(&self, method: &str, params: Params) -> Result<R, Error>
    where
        R: DeserializeOwned,
        Params: ToRpcParams + Send,
    {
        self.inner.call(method, params).await
    }

    async fn batch_request<'a, R>(
        &self,
        _batch: BatchRequestBuilder<'a>,
    ) -> Result<BatchResponse<'a, R>, Error>
    where
        R: DeserializeOwned + std::fmt::Debug + 'a,
    {
        unreachable!("It isn't called at all")
    }
}

#[async_trait::async_trait]
impl SubscriptionClientT for Rpc {
    async fn subscribe<'a, Notif, Params>(
        &self,
        subscribe_method: &'a str,
        params: Params,
        _unsubscribe_method: &'a str,
    ) -> Result<jsonrpsee_core::client::Subscription<Notif>, Error>
    where
        Params: ToRpcParams + Send,
        Notif: DeserializeOwned,
    {
        let mut subscription = Arc::clone(&self.inner).subscribe(subscribe_method, params).await?;
        let kind = subscription.subscription_id().clone().into_owned();
        let (to_back, _) = futures::channel::mpsc::channel(10);
        let (mut notifs_tx, notifs_rx) = futures::channel::mpsc::channel(10);
        tokio::spawn(async move {
            while let Some(result) = subscription.next().await {
                let Ok((item, _)) = result else { break };
                if notifs_tx.send(item).await.is_err() {
                    break;
                }
            }
        });

        Ok(Subscription::new(to_back, notifs_rx, SubscriptionKind::Subscription(kind)))
    }

    async fn subscribe_to_method<'a, Notif>(
        &self,
        _method: &'a str,
    ) -> Result<jsonrpsee_core::client::Subscription<Notif>, Error>
    where
        Notif: DeserializeOwned,
    {
        unreachable!("It isn't called")
    }
}

/// Useful predicate for serde, which allows to skip type during serialization
pub fn is_default<T: Default + PartialEq>(t: &T) -> bool {
    t == &T::default()
}

struct Defer<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Defer<F> {
    pub fn new(f: F) -> Self {
        Self(Some(f))
    }
}

impl<F: FnOnce()> Drop for Defer<F> {
    fn drop(&mut self) {
        (self.0.take().expect("Always set"))();
    }
}

/// Useful type which will ensure that things will be dropped
#[derive(Default, derivative::Derivative)]
#[derivative(Debug)]
pub struct DropCollection {
    #[derivative(Debug = "ignore")]
    vec: Vec<Box<dyn Send + Sync>>,
}

impl DropCollection {
    /// Constructor
    pub fn new() -> Self {
        Self::default()
    }

    /// Run closure during drop
    pub fn defer<F: FnOnce() + Sync + Send + 'static>(&mut self, f: F) {
        self.push(Defer::new(f))
    }

    /// Add something to drop collection
    pub fn push<T: Send + Sync + 'static>(&mut self, t: T) {
        self.vec.push(Box::new(t))
    }
}

impl<T: Send + Sync + 'static> FromIterator<T> for DropCollection {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut me = Self::new();
        for item in iter {
            me.push(item);
        }
        me
    }
}

impl<T: Send + Sync + 'static> Extend<T> for DropCollection {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for item in iter {
            self.push(item);
        }
    }
}

/// Type for dropping things asynchronously
#[derive(Default, derivative::Derivative)]
#[derivative(Debug)]
pub struct AsyncDropFutures {
    #[derivative(Debug = "ignore")]
    vec: Vec<Pin<Box<dyn Future<Output = ()> + Send + Sync>>>,
}

impl AsyncDropFutures {
    /// Constructor
    pub fn new() -> Self {
        Self::default()
    }

    /// Push some future
    pub fn push<F: Future<Output = ()> + Send + Sync + 'static>(&mut self, fut: F) {
        self.vec.push(Box::pin(fut))
    }

    /// Drop and wait on every future
    pub async fn async_drop(self) {
        for f in self.vec {
            f.await;
        }
    }
}

/// Container for number of bytes.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Deref,
    DerefMut,
    Deserialize,
    Display,
    Eq,
    From,
    FromStr,
    Into,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
)]
#[serde(transparent)]
pub struct ByteSize(#[serde(with = "bytesize_serde")] pub bytesize::ByteSize);

impl ByteSize {
    /// Constructor for bytes
    pub const fn b(n: u64) -> Self {
        Self(bytesize::ByteSize::b(n))
    }

    /// Constructor for kilobytes
    pub const fn kb(n: u64) -> Self {
        Self(bytesize::ByteSize::kb(n))
    }

    /// Constructor for kibibytes
    pub const fn kib(n: u64) -> Self {
        Self(bytesize::ByteSize::kib(n))
    }

    /// Constructor for megabytes
    pub const fn mb(n: u64) -> Self {
        Self(bytesize::ByteSize::mb(n))
    }

    /// Constructor for mibibytes
    pub const fn mib(n: u64) -> Self {
        Self(bytesize::ByteSize::mib(n))
    }

    /// Constructor for gigabytes
    pub const fn gb(n: u64) -> Self {
        Self(bytesize::ByteSize::gb(n))
    }

    /// Constructor for gibibytes
    pub const fn gib(n: u64) -> Self {
        Self(bytesize::ByteSize::gib(n))
    }
}

/// Multiaddr is a wrapper around libp2p one
#[derive(
    Clone,
    Debug,
    Deref,
    DerefMut,
    Deserialize,
    Display,
    Eq,
    From,
    FromStr,
    Into,
    PartialEq,
    Serialize,
)]
#[serde(transparent)]
pub struct Multiaddr(pub libp2p_core::Multiaddr);

impl From<sc_network::Multiaddr> for Multiaddr {
    fn from(multiaddr: sc_network::Multiaddr) -> Self {
        multiaddr.to_string().parse().expect("Conversion between 2 libp2p versions is always right")
    }
}

impl From<Multiaddr> for sc_network::Multiaddr {
    fn from(multiaddr: Multiaddr) -> Self {
        multiaddr.to_string().parse().expect("Conversion between 2 libp2p versions is always right")
    }
}

/// Multiaddr with peer id
#[derive(
    Debug, Clone, Deserialize, Serialize, PartialEq, From, Into, FromStr, Deref, DerefMut, Display,
)]
#[serde(transparent)]
pub struct MultiaddrWithPeerId(pub sc_service::config::MultiaddrWithPeerId);

impl MultiaddrWithPeerId {
    /// Constructor for peer id
    pub fn new(multiaddr: impl Into<Multiaddr>, peer_id: sc_network::PeerId) -> Self {
        Self(sc_service::config::MultiaddrWithPeerId {
            multiaddr: multiaddr.into().into(),
            peer_id,
        })
    }
}

impl From<MultiaddrWithPeerId> for sc_network::Multiaddr {
    fn from(multiaddr: MultiaddrWithPeerId) -> Self {
        multiaddr.to_string().parse().expect("Conversion between 2 libp2p versions is always right")
    }
}

impl From<MultiaddrWithPeerId> for libp2p_core::Multiaddr {
    fn from(multiaddr: MultiaddrWithPeerId) -> Self {
        multiaddr.to_string().parse().expect("Conversion between 2 libp2p versions is always right")
    }
}

impl From<MultiaddrWithPeerId> for Multiaddr {
    fn from(multiaddr: MultiaddrWithPeerId) -> Self {
        multiaddr.to_string().parse().expect("Conversion between 2 libp2p versions is always right")
    }
}

/// Spawn task with provided name (if possible)
#[cfg(not(tokio_unstable))]
pub fn task_spawn<F>(name: impl AsRef<str>, future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let _ = name;
    tokio::task::spawn(future)
}

/// Spawn task with provided name (if possible)
#[cfg(tokio_unstable)]
pub fn task_spawn<F>(name: impl AsRef<str>, future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::task::Builder::new()
        .name(name.as_ref())
        .spawn(future)
        .expect("Spawning task never fails")
}

/// Spawn task with provided name (if possible)
#[cfg(not(tokio_unstable))]
pub fn task_spawn_blocking<F, R>(name: impl AsRef<str>, f: F) -> tokio::task::JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let _ = name;
    tokio::task::spawn_blocking(f)
}

/// Spawn task with provided name (if possible)
#[cfg(tokio_unstable)]
pub fn task_spawn_blocking<F, R>(name: impl AsRef<str>, f: F) -> tokio::task::JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    tokio::task::Builder::new()
        .name(name.as_ref())
        .spawn_blocking(f)
        .expect("Spawning task never fails")
}

/// Substrate storage key abstraction
pub struct StorageKey(pub Vec<u8>);

impl StorageKey {
    /// Constructor which accepts storage keys
    pub fn new<IT, K>(keys: IT) -> Self
    where
        IT: IntoIterator<Item = K>,
        K: AsRef<[u8]>,
    {
        Self(keys.into_iter().flat_map(|key| sp_core_hashing::twox_128(key.as_ref())).collect())
    }

    /// Storage key for events
    pub fn events() -> Self {
        Self::new(["System", "Events"])
    }
}

impl Rpc {
    pub(crate) async fn get_storage<H>(
        &self,
        StorageKey(key): StorageKey,
        block: Option<H>,
    ) -> anyhow::Result<Option<sp_storage::StorageData>>
    where
        H: Send + Sync + 'static + serde::ser::Serialize + serde::de::DeserializeOwned,
    {
        self.storage(sp_storage::StorageKey(key), block)
            .await
            .context("Failed to fetch storage entry")
    }
}

/// Public key type
#[derive(
    Debug,
    Default,
    Decode,
    Encode,
    Copy,
    Clone,
    PartialEq,
    Eq,
    Ord,
    PartialOrd,
    Hash,
    Deref,
    DerefMut,
    Serialize,
    Deserialize,
)]
#[serde(transparent)]
pub struct PublicKey(pub subspace_core_primitives::PublicKey);

impl PublicKey {
    /// Construct public key from raw bytes
    pub fn new(raw: [u8; PUBLIC_KEY_LENGTH]) -> Self {
        Self(subspace_core_primitives::PublicKey::from(raw))
    }
}

impl From<[u8; PUBLIC_KEY_LENGTH]> for PublicKey {
    fn from(key: [u8; PUBLIC_KEY_LENGTH]) -> Self {
        Self::new(key)
    }
}

impl From<sp_core::crypto::AccountId32> for PublicKey {
    fn from(account_id: sp_core::crypto::AccountId32) -> Self {
        From::<[u8; PUBLIC_KEY_LENGTH]>::from(*account_id.as_ref())
    }
}

impl From<PublicKey> for sp_core::crypto::AccountId32 {
    fn from(account_id: PublicKey) -> Self {
        Self::new(*account_id.0)
    }
}

impl std::fmt::Display for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

mod parse_ss58 {
    // Copyright (C) 2017-2022 Parity Technologies (UK) Ltd.
    // Copyright (C) 2022 Subspace Labs, Inc.
    // SPDX-License-Identifier: Apache-2.0

    // Licensed under the Apache License, Version 2.0 (the "License");
    // you may not use this file except in compliance with the License.
    // You may obtain a copy of the License at
    //
    // 	http://www.apache.org/licenses/LICENSE-2.0
    //
    // Unless required by applicable law or agreed to in writing, software
    // distributed under the License is distributed on an "AS IS" BASIS,
    // WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
    // See the License for the specific language governing permissions and
    // limitations under the License.

    //! Modified version of SS58 parser extracted from Substrate in order to not
    //! pull the whole `sp-core` into farmer application

    use base58::FromBase58;
    use blake2::digest::typenum::U64;
    use blake2::digest::FixedOutput;
    use blake2::{Blake2b, Digest};
    use ss58_registry::Ss58AddressFormat;
    use subspace_core_primitives::{PublicKey, PUBLIC_KEY_LENGTH};
    use thiserror::Error;

    const PREFIX: &[u8] = b"SS58PRE";
    const CHECKSUM_LEN: usize = 2;

    /// An error type for SS58 decoding.
    #[derive(Debug, Error)]
    pub enum Ss58ParsingError {
        /// Base 58 requirement is violated
        #[error("Base 58 requirement is violated")]
        BadBase58,
        /// Length is bad
        #[error("Length is bad")]
        BadLength,
        /// Invalid SS58 prefix byte
        #[error("Invalid SS58 prefix byte")]
        InvalidPrefix,
        /// Disallowed SS58 Address Format for this datatype
        #[error("Disallowed SS58 Address Format for this datatype")]
        FormatNotAllowed,
        /// Invalid checksum
        #[error("Invalid checksum")]
        InvalidChecksum,
    }

    /// Some if the string is a properly encoded SS58Check address.
    pub(crate) fn parse_ss58_reward_address(s: &str) -> Result<PublicKey, Ss58ParsingError> {
        let data = s.from_base58().map_err(|_| Ss58ParsingError::BadBase58)?;
        if data.len() < 2 {
            return Err(Ss58ParsingError::BadLength);
        }
        let (prefix_len, ident) = match data[0] {
            0..=63 => (1, data[0] as u16),
            64..=127 => {
                // weird bit manipulation owing to the combination of LE encoding and missing
                // two bits from the left.
                // d[0] d[1] are: 01aaaaaa bbcccccc
                // they make the LE-encoded 16-bit value: aaaaaabb 00cccccc
                // so the lower byte is formed of aaaaaabb and the higher byte is 00cccccc
                let lower = (data[0] << 2) | (data[1] >> 6);
                let upper = data[1] & 0b00111111;
                (2, (lower as u16) | ((upper as u16) << 8))
            }
            _ => return Err(Ss58ParsingError::InvalidPrefix),
        };
        if data.len() != prefix_len + PUBLIC_KEY_LENGTH + CHECKSUM_LEN {
            return Err(Ss58ParsingError::BadLength);
        }
        let format: Ss58AddressFormat = ident.into();
        if format.is_reserved() {
            return Err(Ss58ParsingError::FormatNotAllowed);
        }

        let hash = ss58hash(&data[0..PUBLIC_KEY_LENGTH + prefix_len]);
        let checksum = &hash[0..CHECKSUM_LEN];
        if data[PUBLIC_KEY_LENGTH + prefix_len..PUBLIC_KEY_LENGTH + prefix_len + CHECKSUM_LEN]
            != *checksum
        {
            // Invalid checksum.
            return Err(Ss58ParsingError::InvalidChecksum);
        }

        let bytes: [u8; PUBLIC_KEY_LENGTH] = data[prefix_len..][..PUBLIC_KEY_LENGTH]
            .try_into()
            .map_err(|_| Ss58ParsingError::BadLength)?;

        Ok(PublicKey::from(bytes))
    }

    fn ss58hash(data: &[u8]) -> [u8; 64] {
        let mut state = Blake2b::<U64>::new();
        state.update(PREFIX);
        state.update(data);
        state.finalize_fixed().into()
    }

    impl std::str::FromStr for super::PublicKey {
        type Err = Ss58ParsingError;

        fn from_str(s: &str) -> Result<Self, Self::Err> {
            parse_ss58_reward_address(s).map(Self)
        }
    }
}

pub mod chain_spec {
    //! Subspace chain spec related utilities

    use frame_support::traits::Get;
    use sc_service::Properties;
    use sp_core::crypto::AccountId32;
    use sp_core::{sr25519, Pair, Public};
    use sp_runtime::traits::IdentifyAccount;
    use sp_runtime::MultiSigner;
    use subspace_runtime::SS58Prefix;
    use subspace_runtime_primitives::DECIMAL_PLACES;

    /// Shared chain spec properties related to the coin.
    pub fn chain_spec_properties() -> Properties {
        let mut properties = Properties::new();

        properties.insert("ss58Format".into(), <SS58Prefix as Get<u16>>::get().into());
        properties.insert("tokenDecimals".into(), DECIMAL_PLACES.into());
        properties.insert("tokenSymbol".into(), "tSSC".into());

        properties
    }

    /// Get public key from keypair seed.
    pub fn get_public_key_from_seed<TPublic: Public>(
        seed: &'static str,
    ) -> <TPublic::Pair as Pair>::Public {
        TPublic::Pair::from_string(&format!("//{seed}"), None)
            .expect("Static values are valid; qed")
            .public()
    }

    /// Generate an account ID from seed.
    pub fn get_account_id_from_seed(seed: &'static str) -> AccountId32 {
        MultiSigner::from(get_public_key_from_seed::<sr25519::Public>(seed)).into_account()
    }
}

/// Useful macro to generate some common methods and trait implementations for
/// builders
#[macro_export]
macro_rules! generate_builder {
    ( $name:ident ) => {
        impl concat_idents!($name, Builder) {
            /// Constructor
            pub fn new() -> Self {
                Self::default()
            }

            #[doc = concat!("Build ", stringify!($name))]
            pub fn build(self) -> $name {
                self._build().expect("Infallible")
            }
        }

        impl From<concat_idents!($name, Builder)> for $name {
            fn from(value: concat_idents!($name, Builder)) -> Self {
                value.build()
            }
        }
    };
    ( $name:ident, $($rest:ident),+ ) => {
        $crate::generate_builder!($name);
        $crate::generate_builder!($($rest),+);
    };
}
