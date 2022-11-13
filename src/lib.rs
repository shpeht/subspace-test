//! Subspace SDK for easy running of both Subspace node and farmer

#![deny(missing_docs)]
#![feature(type_changing_struct_update)]

/// Module related to the farmer
pub mod farmer;
/// Module related to the node
pub mod node;

pub use farmer::{Builder as FarmerBuilder, Farmer, Info as NodeInfo, Plot, PlotDescription};
pub use node::{chain_spec, Builder as NodeBuilder, Info as FarmerInfo, Node};
pub use parse_ss58::Ss58ParsingError;

use derive_more::{Deref, DerefMut};
use serde::{Deserialize, Serialize};
use subspace_core_primitives::PUBLIC_KEY_LENGTH;

/// Public key type
#[derive(
    Debug,
    Default,
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

    //! Modified version of SS58 parser extracted from Substrate in order to not pull the whole
    //! `sp-core` into farmer application

    use base58::FromBase58;
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
                // weird bit manipulation owing to the combination of LE encoding and missing two
                // bits from the left.
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
        let checksum = &hash.as_bytes()[0..CHECKSUM_LEN];
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
    fn ss58hash(data: &[u8]) -> blake2_rfc::blake2b::Blake2bResult {
        let mut context = blake2_rfc::blake2b::Blake2b::new(64);
        context.update(PREFIX);
        context.update(data);
        context.finalize()
    }

    impl std::str::FromStr for super::PublicKey {
        type Err = Ss58ParsingError;
        fn from_str(s: &str) -> Result<Self, Self::Err> {
            parse_ss58_reward_address(s).map(Self)
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use subspace_farmer::RpcClient;
    use tempdir::TempDir;

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_integration() {
        let dir = TempDir::new("test").unwrap();
        let node = Node::builder()
            .force_authoring(true)
            .role(sc_service::Role::Authority)
            .build(dir, node::chain_spec::dev_config().unwrap())
            .await
            .unwrap();

        let mut slot_info_sub = node.subscribe_slot_info().await.unwrap();

        let dir = TempDir::new("test").unwrap();
        let plot_descriptions = [PlotDescription::new(dir.path(), bytesize::ByteSize::mb(10))];
        let _farmer = Farmer::builder()
            .build(Default::default(), node.clone(), &plot_descriptions)
            .await
            .unwrap();

        // New slots arrive at each block. So basically we wait for 3 blocks to produce
        for _ in 0..3 {
            assert!(slot_info_sub.next().await.is_some());
        }
    }
}
