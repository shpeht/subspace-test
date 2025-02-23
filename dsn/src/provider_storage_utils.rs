// Because of Mutex guard
#![allow(clippy::await_holding_lock)]

use std::borrow::Cow;
use std::sync::Arc;

use derivative::Derivative;
use either::*;
use parking_lot::{RwLock, RwLockReadGuard};
use subspace_networking::libp2p::kad::ProviderRecord;

#[derive(Derivative)]
#[derivative(Debug)]
pub struct MaybeProviderStorage<S> {
    #[derivative(Debug = "ignore")]
    inner: Arc<RwLock<Option<S>>>,
}

impl<S> Clone for MaybeProviderStorage<S> {
    fn clone(&self) -> Self {
        Self { inner: Arc::clone(&self.inner) }
    }
}

impl<S> MaybeProviderStorage<S> {
    pub fn none() -> Self {
        Self { inner: Arc::new(RwLock::new(None)) }
    }

    pub fn swap(&self, value: Option<S>) {
        *self.inner.write() = value;
    }
}

#[allow(clippy::await_holding_lock)]
#[ouroboros::self_referencing]
pub struct RwLockGuardedIterator<'a, S: subspace_networking::ProviderStorage> {
    guard: RwLockReadGuard<'a, Option<S>>,
    #[borrows(guard)]
    #[not_covariant]
    iter: S::ProvidedIter<'this>,
}

impl<'a, S: subspace_networking::ProviderStorage> Iterator for RwLockGuardedIterator<'a, S> {
    type Item = Cow<'a, ProviderRecord>;

    fn next(&mut self) -> Option<Self::Item> {
        self.with_mut(|fields| fields.iter.next().map(|value| Cow::Owned(value.into_owned())))
    }
}

impl<S: subspace_networking::ProviderStorage + 'static> subspace_networking::ProviderStorage
    for MaybeProviderStorage<S>
{
    type ProvidedIter<'a> = Either<std::iter::Empty<Cow<'a, ProviderRecord>>, RwLockGuardedIterator<'a, S>>
    where S: 'a;

    fn provided(&self) -> Self::ProvidedIter<'_> {
        let lock = self.inner.read();
        if lock.is_none() {
            Either::Left(std::iter::empty())
        } else {
            Either::Right(
                RwLockGuardedIteratorBuilder {
                    guard: lock,
                    iter_builder: |guard| {
                        guard.as_ref().expect("We just checked that it is Some").provided()
                    },
                }
                .build(),
            )
        }
    }

    fn remove_provider(
        &mut self,
        k: &subspace_networking::libp2p::kad::record::Key,
        p: &subspace_networking::libp2p::PeerId,
    ) {
        if let Some(x) = &mut *self.inner.write() {
            x.remove_provider(k, p);
        }
    }

    fn providers(
        &self,
        key: &subspace_networking::libp2p::kad::record::Key,
    ) -> Vec<ProviderRecord> {
        self.inner.read().as_ref().map(|x| x.providers(key)).unwrap_or_default()
    }

    fn add_provider(
        &mut self,
        record: ProviderRecord,
    ) -> subspace_networking::libp2p::kad::store::Result<()> {
        self.inner.write().as_mut().map(|x| x.add_provider(record)).unwrap_or(Ok(()))
    }
}

pub struct AndProviderStorage<A, B> {
    a: A,
    b: B,
}

impl<A, B> AndProviderStorage<A, B> {
    pub fn new(a: A, b: B) -> Self {
        Self { a, b }
    }
}

impl<A: subspace_networking::ProviderStorage, B: subspace_networking::ProviderStorage>
    subspace_networking::ProviderStorage for AndProviderStorage<A, B>
{
    type ProvidedIter<'a> = std::iter::Chain<A::ProvidedIter<'a>, B::ProvidedIter<'a>>
    where A: 'a, B: 'a;

    fn add_provider(
        &mut self,
        record: ProviderRecord,
    ) -> subspace_networking::libp2p::kad::store::Result<()> {
        self.a.add_provider(record.clone())?;
        self.b.add_provider(record)?;
        Ok(())
    }

    fn provided(&self) -> Self::ProvidedIter<'_> {
        self.a.provided().chain(self.b.provided())
    }

    fn providers(
        &self,
        key: &subspace_networking::libp2p::kad::record::Key,
    ) -> Vec<ProviderRecord> {
        self.a
            .providers(key)
            .into_iter()
            .chain(self.b.providers(key))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect()
    }

    fn remove_provider(
        &mut self,
        k: &subspace_networking::libp2p::kad::record::Key,
        p: &subspace_networking::libp2p::PeerId,
    ) {
        self.a.remove_provider(k, p);
        self.b.remove_provider(k, p);
    }
}
