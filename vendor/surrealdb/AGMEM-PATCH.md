# agmem patch to surrealdb 3.2.4

Vendored from crates.io surrealdb 3.2.4 (Cargo.lock removed). The only change, issue #196:

```diff
diff -ru src/engine/local/mod.rs vendor/surrealdb/src/engine/local/mod.rs
--- src/engine/local/mod.rs	2006-07-24 03:21:28
+++ vendor/surrealdb/src/engine/local/mod.rs	2026-10-04 10:19:05
@@ -150,6 +150,26 @@
 
 #[cfg(not(target_family = "wasm"))]
 pub(crate) mod native;
+
+/// agmem patch (issue #196): engine settings for every embedded datastore
+/// this process opens from now on — `surrealkv_max_memtable_size`,
+/// `surrealkv_block_cache_capacity`, `hnsw_cache_size` and the rest of the
+/// keys `surrealdb_core` parses from a `ConfigMap`. Upstream builds the
+/// datastore with an empty map and no way in, so surrealkv sizes its caches
+/// from total RAM: a 1 GiB memtable and a 7 GiB block cache on 16 GiB.
+///
+/// The first call wins; it returns `false` when settings were already set.
+#[cfg(not(target_family = "wasm"))]
+pub fn set_datastore_config<I, K, V>(pairs: I) -> bool
+where
+	I: IntoIterator<Item = (K, V)>,
+	K: Into<String>,
+	V: Into<String>,
+{
+	native::DATASTORE_CONFIG
+		.set(pairs.into_iter().map(|(k, v)| (k.into(), v.into())).collect())
+		.is_ok()
+}
 #[cfg(target_family = "wasm")]
 pub(crate) mod wasm;
 
diff -ru src/engine/local/native.rs vendor/surrealdb/src/engine/local/native.rs
--- src/engine/local/native.rs	2006-07-24 03:21:28
+++ vendor/surrealdb/src/engine/local/native.rs	2026-10-04 10:19:05
@@ -1,5 +1,5 @@
 use std::collections::HashSet;
-use std::sync::Arc;
+use std::sync::{Arc, OnceLock};
 use std::task::Poll;
 
 use async_channel::{Receiver, Sender};
@@ -7,6 +7,7 @@
 use futures::stream::poll_fn;
 use surrealdb_core::channel::Receiver as CoreReceiver;
 use surrealdb_core::iam::Level;
+use surrealdb_core::cnf::ConfigMap;
 use surrealdb_core::kvs::Datastore;
 use surrealdb_core::options::EngineOptions;
 use surrealdb_types::Notification;
@@ -109,6 +110,17 @@
 	}
 }
 
+/// What `set_datastore_config` stored (agmem patch, issue #196).
+pub(crate) static DATASTORE_CONFIG: OnceLock<Vec<(String, String)>> = OnceLock::new();
+
+fn datastore_config() -> ConfigMap {
+	DATASTORE_CONFIG
+		.get()
+		.into_iter()
+		.flatten()
+		.fold(ConfigMap::empty(), |map, (k, v)| map.with_key_value(k.as_str(), v.as_str()))
+}
+
 pub(crate) async fn run_router(
 	address: Endpoint,
 	conn_tx: Sender<Result<()>>,
@@ -129,6 +141,7 @@
 	};
 
 	let builder = Datastore::builder()
+		.with_config(datastore_config())
 		.with_query_timeout(address.config.query_timeout)
 		.with_transaction_timeout(address.config.transaction_timeout)
 		.with_auth(configured_root.is_some());
```
