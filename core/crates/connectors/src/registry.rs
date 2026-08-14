//! Which connectors this build knows about.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::connector::{SinkConnector, SourceConnector};
use crate::error::{ConnectorError, Result};

/// The set of connectors available to the dispatcher and the surfaces.
///
/// `BTreeMap` rather than `HashMap` so `sink_ids()` is stable — a UI listing connectors in
/// a different order on every launch is a bug report waiting to happen.
#[derive(Debug, Default)]
pub struct ConnectorRegistry {
    sinks: BTreeMap<String, Arc<dyn SinkConnector>>,
    sources: BTreeMap<String, Arc<dyn SourceConnector>>,
}

impl ConnectorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_sink(&mut self, sink: Arc<dyn SinkConnector>) {
        self.sinks.insert(sink.id().to_string(), sink);
    }

    pub fn register_source(&mut self, source: Arc<dyn SourceConnector>) {
        self.sources.insert(source.id().to_string(), source);
    }

    pub fn sink(&self, id: &str) -> Result<Arc<dyn SinkConnector>> {
        self.sinks
            .get(id)
            .cloned()
            .ok_or_else(|| ConnectorError::UnknownConnector(id.to_string()))
    }

    pub fn source(&self, id: &str) -> Result<Arc<dyn SourceConnector>> {
        self.sources
            .get(id)
            .cloned()
            .ok_or_else(|| ConnectorError::UnknownConnector(id.to_string()))
    }

    pub fn sink_ids(&self) -> Vec<String> {
        self.sinks.keys().cloned().collect()
    }

    pub fn source_ids(&self) -> Vec<String> {
        self.sources.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sinks::MockConnector;
    use std::sync::Arc;

    #[test]
    fn resolves_a_registered_sink_by_id() {
        let mut registry = ConnectorRegistry::new();
        registry.register_sink(Arc::new(MockConnector::new("mock")));

        assert!(registry.sink("mock").is_ok());
        assert_eq!(registry.sink_ids(), vec!["mock".to_string()]);
    }

    #[test]
    fn an_unknown_id_is_an_error_not_a_panic() {
        let registry = ConnectorRegistry::new();
        let err = registry.sink("nope").unwrap_err();
        assert!(matches!(err, ConnectorError::UnknownConnector(id) if id == "nope"));
    }

    #[test]
    fn registering_the_same_id_twice_replaces_it() {
        let mut registry = ConnectorRegistry::new();
        registry.register_sink(Arc::new(MockConnector::new("mock")));
        registry.register_sink(Arc::new(MockConnector::new("mock")));

        assert_eq!(registry.sink_ids().len(), 1);
    }
}
