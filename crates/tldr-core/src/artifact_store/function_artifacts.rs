//! Demand-driven, dependency-explicit function artifact construction.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};

use super::redb::{decode, encode};
use super::{ArtifactEnvelope, ArtifactKey, ArtifactKind, ArtifactStore};
use crate::{TldrError, TldrResult};

/// Per-key single-flight coordinator for CFG, DFG, and PDG artifacts.
pub struct FunctionArtifactCoordinator {
    store: Arc<dyn ArtifactStore>,
    flights: Mutex<HashMap<ArtifactKey, Arc<Mutex<()>>>>,
}

impl FunctionArtifactCoordinator {
    /// Bind demand construction to the authoritative project store.
    pub fn new(store: Arc<dyn ArtifactStore>) -> Self {
        Self {
            store,
            flights: Mutex::new(HashMap::new()),
        }
    }

    /// Load or construct one typed function artifact.
    ///
    /// `key.revision` is the containing file revision. DFG callers pass the
    /// exact CFG key in `dependencies`; PDG callers pass both CFG and DFG.
    pub fn materialize<T>(
        &self,
        key: ArtifactKey,
        dependencies: Vec<ArtifactKey>,
        build: impl FnOnce() -> TldrResult<T>,
    ) -> TldrResult<T>
    where
        T: Archive
            + for<'a> RkyvSerialize<
                rkyv::api::high::HighSerializer<
                    rkyv::util::AlignedVec,
                    rkyv::ser::allocator::ArenaHandle<'a>,
                    rkyv::rancor::Error,
                >,
            >,
        T::Archived: for<'a> rkyv::bytecheck::CheckBytes<
                rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>,
            > + RkyvDeserialize<T, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>,
    {
        if !matches!(
            key.kind,
            ArtifactKind::Cfg | ArtifactKind::Dfg | ArtifactKind::Pdg
        ) {
            return Err(function_error(
                "function coordinator only accepts CFG, DFG, or PDG",
            ));
        }
        if let Some(existing) = self.store.artifact(&key)? {
            return decode(&existing.payload);
        }

        let flight = {
            let mut flights = self.flights.lock().expect("function flights poisoned");
            flights
                .entry(key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = flight.lock().expect("function flight poisoned");
        if let Some(existing) = self.store.artifact(&key)? {
            return decode(&existing.payload);
        }

        let value = build()?;
        let generation = self
            .store
            .active_generation()?
            .ok_or_else(|| function_error("artifact store is not ready"))?;
        self.store.commit_optional(&ArtifactEnvelope::new(
            key,
            generation,
            dependencies,
            encode(&value)?,
        ))?;
        Ok(value)
    }
}

fn function_error(message: impl Into<String>) -> TldrError {
    TldrError::DaemonError(message.into())
}
