use super::{config::VaultConfig, transactions::TransactionProvider};
use crate::common::api::handle_rejection;
use crate::local_store::ILocalStore;
use std::sync::{Arc, Mutex};

use parking_lot::RwLock;
use tokio::sync::oneshot;
use warp::Filter;

/// Api v1
pub mod v1;

/// Unused
pub struct APIServer {}

impl APIServer {
    /// Starts an http server in the current thread and blocks. Gracefully shutdowns
    /// when `shotdown_receiver` receives a signal (i.e. `send()` is called).
    pub fn serve<T, L>(
        config: &VaultConfig,
        local_store: Arc<Mutex<L>>,
        provider: Arc<RwLock<T>>,
        shutdown_receiver: oneshot::Receiver<()>,
    ) where
        T: TransactionProvider + Send + Sync + 'static,
        L: ILocalStore + Send + 'static,
    {
        let config = v1::Config {
            loki_wallet_address: config.loki.wallet_address.clone(),
            eth_master_root_key: config.eth.master_root_key.clone(),
            btc_master_root_key: config.btc.master_root_key.clone(),
            net_type: config.net_type,
        };
        let routes = v1::endpoints(local_store, provider, config).recover(handle_rejection);

        let mut rt = tokio::runtime::Runtime::new().unwrap();

        let future = async {
            let (_addr, server) =
                warp::serve(routes).bind_with_graceful_shutdown(([127, 0, 0, 1], 3030), async {
                    shutdown_receiver.await.ok();
                });

            server.await;
        };

        rt.block_on(future);
    }
}
