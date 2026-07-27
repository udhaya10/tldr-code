//! Demand-driven, dependency-explicit function artifact construction.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::de::DeserializeOwned;
use serde::Serialize;

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
        T: Serialize + DeserializeOwned,
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
            return decode_cbor(&existing.payload);
        }

        let flight = {
            let mut flights = self
                .flights
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            flights
                .entry(key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let result = {
            let _guard = flight
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (|| {
                if let Some(existing) = self.store.artifact(&key)? {
                    return decode_cbor(&existing.payload);
                }

                let value = build()?;
                let generation = self
                    .store
                    .active_generation()?
                    .ok_or_else(|| function_error("artifact store is not ready"))?;
                self.store.commit_optional(&ArtifactEnvelope::new(
                    key.clone(),
                    generation,
                    dependencies,
                    encode_cbor(&value)?,
                ))?;
                Ok(value)
            })()
        };
        let mut flights = self
            .flights
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if flights
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, &flight))
            && Arc::strong_count(&flight) == 2
        {
            flights.remove(&key);
        }
        result
    }
}

fn encode_cbor<T: Serialize>(value: &T) -> TldrResult<Vec<u8>> {
    let mut bytes = Vec::new();
    ciborium::into_writer(value, &mut bytes)
        .map_err(|error| function_error(format!("CBOR encode failed: {error}")))?;
    Ok(bytes)
}

fn decode_cbor<T: DeserializeOwned>(bytes: &[u8]) -> TldrResult<T> {
    ciborium::from_reader(bytes)
        .map_err(|error| function_error(format!("CBOR decode failed: {error}")))
}

fn function_error(message: impl Into<String>) -> TldrError {
    TldrError::DaemonError(message.into())
}
