//! Engine registry — how a URL finds its engine (architecture rule #2:
//! protocol = trait). Engines register by name; `route` picks the first one
//! that claims a URL. HTTP before BT before FTP — registration order wins.

use crate::engine::ProtocolEngine;
use crate::error::ApiError;

/// All live engines, in registration order (earlier engines win ties).
#[derive(Default)]
pub struct EngineRegistry {
    engines: Vec<Box<dyn ProtocolEngine>>,
}

impl EngineRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an engine. Duplicate names are a wiring bug, not a runtime
    /// condition — callers get an error instead of a silent overwrite.
    pub fn register(&mut self, engine: Box<dyn ProtocolEngine>) -> Result<(), ApiError> {
        let name = engine.name();
        if self.engines.iter().any(|e| e.name() == name) {
            return Err(ApiError::DuplicateEngine(name.to_string()));
        }
        self.engines.push(engine);
        Ok(())
    }

    /// The first engine (by registration order) that claims `url`, if any.
    pub fn route(&self, url: &str) -> Option<&dyn ProtocolEngine> {
        self.engines
            .iter()
            .find(|e| e.supports(url))
            .map(|e| e.as_ref())
    }

    /// Number of registered engines.
    pub fn len(&self) -> usize {
        self.engines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.engines.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{ProbeFuture, ProbeInfo};

    struct YesEngine {
        name: &'static str,
        schemes: &'static [&'static str],
    }

    impl ProtocolEngine for YesEngine {
        fn name(&self) -> &'static str {
            self.name
        }

        fn supports(&self, url: &str) -> bool {
            self.schemes.iter().any(|s| url.starts_with(s))
        }

        fn probe(&self, url: &str) -> ProbeFuture<Result<ProbeInfo, ApiError>> {
            let url = url.to_string();
            Box::pin(async move {
                Ok(ProbeInfo {
                    url,
                    content_length: None,
                    accept_ranges: false,
                    etag: None,
                    etag_strong: false,
                    last_modified: None,
                    filename: None,
                })
            })
        }
    }

    fn http_engine() -> Box<dyn ProtocolEngine> {
        Box::new(YesEngine {
            name: "http",
            schemes: &["http://", "https://"],
        })
    }

    fn bt_engine() -> Box<dyn ProtocolEngine> {
        Box::new(YesEngine {
            name: "bt",
            schemes: &["magnet:"],
        })
    }

    #[test]
    fn route_prefers_earliest_registered_engine() {
        // Two engines claiming the same scheme: registration order wins.
        let mut reg = EngineRegistry::new();
        reg.register(Box::new(YesEngine {
            name: "generic",
            schemes: &["http://", "magnet:"],
        }))
        .unwrap();
        reg.register(http_engine()).unwrap();

        assert_eq!(reg.route("http://example.com/f").unwrap().name(), "generic");
        assert_eq!(reg.route("magnet:?x").unwrap().name(), "generic");
    }

    #[test]
    fn register_route_and_reject_duplicates() {
        let mut reg = EngineRegistry::new();
        assert!(reg.is_empty());

        reg.register(http_engine()).unwrap();
        reg.register(bt_engine()).unwrap();
        assert_eq!(reg.len(), 2);

        // Duplicate name is refused, count unchanged.
        let err = reg
            .register(Box::new(YesEngine {
                name: "http",
                schemes: &[],
            }))
            .unwrap_err();
        assert!(matches!(err, ApiError::DuplicateEngine(n) if n == "http"));
        assert_eq!(reg.len(), 2);

        // Routing: first match in iteration order wins; no match → None.
        assert_eq!(reg.route("https://example.com/f").unwrap().name(), "http");
        assert_eq!(reg.route("magnet:?xt=urn:btih:x").unwrap().name(), "bt");
        assert!(reg.route("ftp://old.example.com").is_none());
    }
}
