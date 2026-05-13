//! Verb dispatcher. Verb crates (vdsm-host, vdsm-virt, ...) register
//! handlers by name; the server invokes them per inbound request.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use tracing::{info, warn};

use crate::protocol::JsonRpcError;

/// A pinned, boxed, sendable future returning a JSON-RPC result or error.
pub type BoxFut = Pin<Box<dyn Future<Output = Result<Value, JsonRpcError>> + Send>>;

/// A handler is a Send+Sync closure mapping params -> future of result.
pub type DispatchFn = Arc<dyn Fn(Value) -> BoxFut + Send + Sync>;

#[derive(Default, Clone)]
pub struct Dispatcher {
    handlers: Arc<HashMap<String, DispatchFn>>,
}

impl Dispatcher {
    pub fn builder() -> DispatcherBuilder {
        DispatcherBuilder { handlers: HashMap::new() }
    }

    pub fn handler_count(&self) -> usize {
        self.handlers.len()
    }

    pub async fn invoke(&self, method: &str, params: Value) -> Result<Value, JsonRpcError> {
        // Trace inbound verb + the top-level param keys. Logging the full
        // params blows up logs for VM XML / large dicts; key names are enough
        // for protocol-conformance debugging.
        let param_keys: Vec<&str> = params
            .as_object()
            .map(|m| m.keys().map(String::as_str).collect())
            .unwrap_or_default();
        match self.handlers.get(method) {
            Some(h) => {
                info!(verb = method, params = ?param_keys, "dispatch");
                let r = (h)(params).await;
                // Wire trace: dump the response body so we can compare what
                // engine actually parses to what we think we're sending.
                if let Ok(v) = &r {
                    info!(verb = method, body = %serde_json::to_string(v).unwrap_or_default(), "response");
                }
                r
            }
            None => {
                warn!(verb = method, params = ?param_keys, "UNIMPLEMENTED verb");
                Err(JsonRpcError::method_not_found(method))
            }
        }
    }
}

pub struct DispatcherBuilder {
    handlers: HashMap<String, DispatchFn>,
}

impl DispatcherBuilder {
    pub fn register<F, Fut>(mut self, method: impl Into<String>, f: F) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, JsonRpcError>> + Send + 'static,
    {
        let f = Arc::new(f);
        let handler: DispatchFn = Arc::new(move |params| {
            let f = Arc::clone(&f);
            Box::pin(async move { (f)(params).await })
        });
        self.handlers.insert(method.into(), handler);
        self
    }

    pub fn build(self) -> Dispatcher {
        Dispatcher {
            handlers: Arc::new(self.handlers),
        }
    }
}
