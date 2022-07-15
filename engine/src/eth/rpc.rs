use sp_core::{H256, U256};
use utilities::make_periodic_tick;
use web3::{
    api::SubscriptionStream,
    signing::SecretKeyRef,
    types::{
        Block, BlockHeader, BlockNumber, Bytes, CallRequest, FeeHistory, Filter, Log,
        SignedTransaction, SyncState, TransactionParameters, TransactionReceipt, U64,
    },
    Web3,
};
use web3_secp256k1::SecretKey;

use futures::{future::select_ok, FutureExt};

use anyhow::{Context, Result};

use crate::{
    constants::{ETH_DUAL_REQUEST_TIMEOUT, ETH_LOG_REQUEST_TIMEOUT, SYNC_POLL_INTERVAL},
    settings,
};

use super::{redact_secret_eth_node_endpoint, TransportProtocol};

use async_trait::async_trait;

#[cfg(test)]
use mockall::automock;

pub type EthHttpRpcClient = EthRpcClient<web3::transports::Http>;
pub type EthWsRpcClient = EthRpcClient<web3::transports::WebSocket>;

#[derive(Clone)]
pub struct EthRpcClient<T: EthTransport> {
    web3: Web3<T>,
}

impl<T: EthTransport> EthRpcClient<T> {
    async fn inner_new<F: futures::Future<Output = Result<T>>>(
        node_endpoint: &str,
        f: F,
        logger: &slog::Logger,
    ) -> Result<Self> {
        slog::debug!(
            logger,
            "Connecting new {} web3 client{}",
            T::transport_protocol(),
            match redact_secret_eth_node_endpoint(node_endpoint) {
                Ok(redacted_node_endpoint) => format!(" to {}", redacted_node_endpoint),
                Err(e) => {
                    slog::error!(
                        logger,
                        "Could not redact secret in {} ETH node endpoint: {}",
                        T::transport_protocol(),
                        e
                    );
                    "".to_string()
                }
            }
        );

        Ok(Self {
            web3: Web3::new(f.await?),
        })
    }
}

pub trait EthTransport: web3::Transport {
    fn transport_protocol() -> TransportProtocol;
}

impl EthTransport for web3::transports::WebSocket {
    fn transport_protocol() -> TransportProtocol {
        TransportProtocol::Ws
    }
}

impl EthTransport for web3::transports::Http {
    fn transport_protocol() -> TransportProtocol {
        TransportProtocol::Http
    }
}

// We use a trait so we can inject a mock in the tests
#[cfg_attr(test, automock)]
#[async_trait]
pub trait EthRpcApi: Send + Sync {
    async fn estimate_gas(&self, req: CallRequest, block: Option<BlockNumber>) -> Result<U256>;

    async fn sign_transaction(
        &self,
        tx: TransactionParameters,
        key: &SecretKey,
    ) -> Result<SignedTransaction>;

    async fn send_raw_transaction(&self, rlp: Bytes) -> Result<H256>;

    async fn get_logs(&self, filter: Filter) -> Result<Vec<Log>>;

    async fn chain_id(&self) -> Result<U256>;

    async fn transaction_receipt(&self, tx_hash: H256) -> Result<TransactionReceipt>;

    /// Gets block, returning error when either:
    /// - Request fails
    /// - Request succeeds, but doesn't return a block
    async fn block(&self, block_number: U64) -> Result<Block<H256>>;

    async fn fee_history(
        &self,
        block_count: U256,
        newest_block: BlockNumber,
        reward_percentiles: Option<Vec<f64>>,
    ) -> Result<FeeHistory>;

    /// Get the latest block number.
    async fn block_number(&self) -> Result<U64>;
}

#[async_trait]
impl<T> EthRpcApi for EthRpcClient<T>
where
    T: Send + Sync + EthTransport,
    T::Out: Send,
{
    async fn estimate_gas(&self, req: CallRequest, block: Option<BlockNumber>) -> Result<U256> {
        self.web3
            .eth()
            .estimate_gas(req, block)
            .await
            .context(format!(
                "{} client: Failed to estimate gas",
                T::transport_protocol()
            ))
    }

    async fn sign_transaction(
        &self,
        tx: TransactionParameters,
        key: &SecretKey,
    ) -> Result<SignedTransaction> {
        self.web3
            .accounts()
            .sign_transaction(tx, SecretKeyRef::from(key))
            .await
            .context(format!(
                "{} client: Failed to sign transaction",
                T::transport_protocol()
            ))
    }

    async fn send_raw_transaction(&self, rlp: Bytes) -> Result<H256> {
        self.web3
            .eth()
            .send_raw_transaction(rlp)
            .await
            .context(format!(
                "{} client: Failed to send raw transaction",
                T::transport_protocol()
            ))
    }

    async fn get_logs(&self, filter: Filter) -> Result<Vec<Log>> {
        let request_fut = self.web3.eth().logs(filter);

        // NOTE: if this does time out we will most likely have a
        // "memory leak" associated with rust-web3's state for this
        // request not getting properly cleaned up
        tokio::time::timeout(ETH_LOG_REQUEST_TIMEOUT, request_fut)
            .await
            .context(format!(
                "{} client: get_logs request timeout",
                T::transport_protocol()
            ))?
            .context(format!(
                "{} client: Failed to fetch ETH logs",
                T::transport_protocol()
            ))
    }

    async fn chain_id(&self) -> Result<U256> {
        self.web3.eth().chain_id().await.context(format!(
            "{} client: Failed to fetch ETH ChainId",
            T::transport_protocol()
        ))
    }

    async fn transaction_receipt(&self, tx_hash: H256) -> Result<TransactionReceipt> {
        self.web3
            .eth()
            .transaction_receipt(tx_hash)
            .await
            .context(format!(
                "{} client: Failed to fetch ETH transaction",
                T::transport_protocol()
            ))
            .and_then(|opt_block| {
                opt_block.ok_or_else(|| {
                    anyhow::Error::msg(format!(
                        "{} client: Getting ETH transaction receipt with tx_hash {} returned None",
                        T::transport_protocol(),
                        tx_hash
                    ))
                })
            })
    }

    async fn block(&self, block_number: U64) -> Result<Block<H256>> {
        self.web3
            .eth()
            .block(block_number.into())
            .await
            .context(format!(
                "{} client: Failed to fetch block",
                T::transport_protocol()
            ))
            .and_then(|opt_block| {
                opt_block.ok_or_else(|| {
                    anyhow::Error::msg(format!(
                        "{} client: Getting ETH block for block number {} returned None",
                        T::transport_protocol(),
                        block_number,
                    ))
                })
            })
    }

    async fn fee_history(
        &self,
        block_count: U256,
        newest_block: BlockNumber,
        reward_percentiles: Option<Vec<f64>>,
    ) -> Result<FeeHistory> {
        self.web3
            .eth()
            .fee_history(block_count, newest_block, reward_percentiles.clone())
            .await
            .context(format!(
                "{} client: Call failed: fee_history({:?}, {:?}, {:?})",
                T::transport_protocol(),
                block_count,
                newest_block,
                reward_percentiles,
            ))
    }

    async fn block_number(&self) -> Result<U64> {
        self.web3
            .eth()
            .block_number()
            .await
            .context("Failed to fetch block number with HTTP client")
    }
}

impl EthWsRpcClient {
    pub async fn new(eth_settings: &settings::Eth, logger: &slog::Logger) -> Result<Self> {
        let client = Self::inner_new(
            &eth_settings.ws_node_endpoint,
            async {
                context!(web3::transports::WebSocket::new(&eth_settings.ws_node_endpoint).await)
            },
            logger,
        )
        .await?;

        let mut poll_interval = make_periodic_tick(SYNC_POLL_INTERVAL, false);

        while let SyncState::Syncing(info) = client
            .web3
            .eth()
            .syncing()
            .await
            .context("Failure while syncing EthRpcClient client")?
        {
            slog::info!(
                logger,
                "Waiting for ETH node to sync. Sync state is: {:?}. Checking again in {:?} ...",
                info,
                poll_interval.period(),
            );
            poll_interval.tick().await;
        }
        slog::info!(logger, "ETH node is synced.");

        Ok(client)
    }
}

#[async_trait]
pub trait EthWsRpcApi {
    async fn subscribe_new_heads(
        &self,
    ) -> Result<SubscriptionStream<web3::transports::WebSocket, BlockHeader>>;
}

#[async_trait]
impl EthWsRpcApi for EthWsRpcClient {
    async fn subscribe_new_heads(
        &self,
    ) -> Result<SubscriptionStream<web3::transports::WebSocket, BlockHeader>> {
        self.web3
            .eth_subscribe()
            .subscribe_new_heads()
            .await
            .context("Failed to subscribe to new heads with WS Client")
    }
}

impl EthHttpRpcClient {
    pub fn new(eth_settings: &settings::Eth, logger: &slog::Logger) -> Result<Self> {
        Self::inner_new(
            &eth_settings.http_node_endpoint,
            std::future::ready({
                context!(web3::transports::Http::new(
                    &eth_settings.http_node_endpoint
                ))
            }),
            logger,
        )
        .now_or_never()
        .unwrap()
    }
}

#[derive(Clone)]
pub struct EthDualRpcClient {
    ws_client: EthWsRpcClient,
    http_client: EthHttpRpcClient,
}

impl EthDualRpcClient {
    pub fn new(ws_client: EthWsRpcClient, http_client: EthHttpRpcClient) -> Self {
        Self {
            ws_client,
            http_client,
        }
    }
}

macro_rules! dual_call_rpc {
    ($eth_dual:expr, $method:ident, $($arg:expr),*) => {
        {
            let ws_request = $eth_dual.ws_client.$method($($arg.clone()),*);
            let http_request = $eth_dual.http_client.$method($($arg),*);

            // TODO: Work out how to wait for both errors (select_ok returns only the last error)
            tokio::time::timeout(ETH_DUAL_REQUEST_TIMEOUT, select_ok([ws_request, http_request]))
                .await
                .context("ETH Dual RPC request timed out")?
                .context("ETH Dual RPC request failed")
                .map(|x| x.0)
        }
    };
}

#[async_trait]
impl EthRpcApi for EthDualRpcClient {
    async fn estimate_gas(&self, req: CallRequest, block: Option<BlockNumber>) -> Result<U256> {
        dual_call_rpc!(self, estimate_gas, req, block)
    }

    async fn sign_transaction(
        &self,
        tx: TransactionParameters,
        key: &SecretKey,
    ) -> Result<SignedTransaction> {
        // NB: This clippy allow applies file-wide, but we only need it for this borrow
        #![allow(clippy::needless_borrow)]
        dual_call_rpc!(self, sign_transaction, tx, &key)
    }

    async fn send_raw_transaction(&self, rlp: Bytes) -> Result<H256> {
        dual_call_rpc!(self, send_raw_transaction, rlp)
    }

    async fn get_logs(&self, filter: Filter) -> Result<Vec<Log>> {
        dual_call_rpc!(self, get_logs, filter)
    }

    async fn chain_id(&self) -> Result<U256> {
        dual_call_rpc!(self, chain_id,)
    }

    async fn transaction_receipt(&self, tx_hash: H256) -> Result<TransactionReceipt> {
        dual_call_rpc!(self, transaction_receipt, tx_hash)
    }

    async fn block(&self, block_number: U64) -> Result<Block<H256>> {
        dual_call_rpc!(self, block, block_number)
    }

    async fn fee_history(
        &self,
        block_count: U256,
        newest_block: BlockNumber,
        reward_percentiles: Option<Vec<f64>>,
    ) -> Result<FeeHistory> {
        dual_call_rpc!(
            self,
            fee_history,
            block_count,
            newest_block,
            reward_percentiles
        )
    }

    async fn block_number(&self) -> Result<U64> {
        dual_call_rpc!(self, block_number,)
    }
}

#[cfg(test)]
pub mod mocks {
    use super::*;

    use mockall::mock;
    use sp_core::H256;
    use web3::types::{Block, Bytes, Filter, Log};

    mock!(
        // becomes MockEthHttpRpcClient
        pub EthHttpRpcClient {}

        #[async_trait]
        impl EthRpcApi for EthHttpRpcClient {
            async fn estimate_gas(&self, req: CallRequest, block: Option<BlockNumber>) -> Result<U256>;

            async fn sign_transaction(
                &self,
                tx: TransactionParameters,
                key: &SecretKey,
            ) -> Result<SignedTransaction>;

            async fn send_raw_transaction(&self, rlp: Bytes) -> Result<H256>;

            async fn get_logs(&self, filter: Filter) -> Result<Vec<Log>>;

            async fn chain_id(&self) -> Result<U256>;

            async fn transaction_receipt(&self, tx_hash: H256) -> Result<TransactionReceipt>;

            async fn block(&self, block_number: U64) -> Result<Block<H256>>;

            async fn fee_history(
                &self,
                block_count: U256,
                newest_block: BlockNumber,
                reward_percentiles: Option<Vec<f64>>,
            ) -> Result<FeeHistory>;

            async fn block_number(&self) -> Result<U64>;
        }
    );
}

#[cfg(test)]
mod tests {

    use crate::eth::EIP1559_TX_ID;

    use super::*;

    use cf_chains::eth::{verify_transaction, UnsignedTransaction};
    use ethereum::{LegacyTransaction, LegacyTransactionMessage, TransactionV2};
    use futures::future::BoxFuture;
    use rand::{prelude::StdRng, Rng, SeedableRng};
    use web3::{signing::Key, Transport};

    // The web3 test transport is not Send, so we create this just to satisfy the EthRpc.
    // We don't actually need to make any RPC calls (if the necessary parameters are supplied when `sign_transaction`
    // is called) so we just use this dummy
    #[derive(Debug, Clone, Default)]
    struct DummyTransport {}

    impl EthTransport for DummyTransport {
        fn transport_protocol() -> TransportProtocol {
            // arbitrary value, doesn't matter for these tests
            TransportProtocol::Http
        }
    }

    impl Transport for DummyTransport {
        type Out = BoxFuture<'static, std::result::Result<serde_json::Value, web3::Error>>;

        fn prepare(
            &self,
            _method: &str,
            _params: Vec<jsonrpc_core::Value>,
        ) -> (web3::RequestId, jsonrpc_core::Call) {
            panic!("You did not supply the appropriate parameters. Unnecessary RPC calls were attempted.")
        }

        fn send(&self, _id: web3::RequestId, _request: jsonrpc_core::Call) -> Self::Out {
            panic!("You did not supply the appropriate parameters. Unnecessary RPC calls were attempted.")
        }
    }

    type TestEthRpcClient = EthRpcClient<DummyTransport>;
    impl TestEthRpcClient {
        fn new() -> Self {
            let web3 = web3::Web3::new(DummyTransport::default());
            Self { web3 }
        }
    }

    fn test_unsigned_transaction() -> UnsignedTransaction {
        UnsignedTransaction {
            chain_id: 42,
            max_fee_per_gas: U256::from(1_000_000_000u32).into(),
            gas_limit: U256::from(21_000u32).into(),
            contract: [0xcf; 20].into(),
            value: 0.into(),
            data: b"do_something()".to_vec(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_eip1559_signature_verification() {
        let unsigned_tx = test_unsigned_transaction();

        let mut tx_params = TransactionParameters {
            to: Some(unsigned_tx.contract),
            data: unsigned_tx.data.clone().into(),
            chain_id: Some(unsigned_tx.chain_id),
            value: unsigned_tx.value,
            max_fee_per_gas: unsigned_tx.max_fee_per_gas,
            max_priority_fee_per_gas: unsigned_tx.max_priority_fee_per_gas,
            transaction_type: Some(web3::types::U64::from(EIP1559_TX_ID)),
            gas: unsigned_tx.gas_limit.unwrap(),
            ..Default::default()
        };
        // set this manually so we don't need an RPC request within web3's `sign_transaction`
        tx_params.nonce = Some(U256::from(2));
        for seed in 0..10 {
            let arr: [u8; 32] = StdRng::seed_from_u64(seed).gen();
            let key = SecretKey::from_slice(&arr[..]).unwrap();
            let address = web3::signing::SecretKeyRef::new(&key).address();

            let test_eth_rpc_client = TestEthRpcClient::new();
            let signed_tx = test_eth_rpc_client
                .sign_transaction(tx_params.clone(), &key)
                .await
                .unwrap();

            assert_eq!(
                verify_transaction(&unsigned_tx, &signed_tx.raw_transaction.0, &address),
                Ok(signed_tx.transaction_hash),
            );
        }
    }

    #[test]
    fn test_legacy_signature_verification() {
        let unsigned_tx = test_unsigned_transaction();

        let msg = LegacyTransactionMessage {
            chain_id: Some(unsigned_tx.chain_id),
            nonce: 0u32.into(),
            gas_limit: unsigned_tx.gas_limit.unwrap(),
            gas_price: U256::from(1_000_000_000u32),
            action: ethereum::TransactionAction::Call(unsigned_tx.contract),
            value: unsigned_tx.value,
            input: unsigned_tx.data.clone(),
        };

        for seed in 0..10 {
            let arr: [u8; 32] = StdRng::seed_from_u64(seed).gen();
            let key = SecretKey::from_slice(&arr[..]).unwrap();
            let key_ref = web3::signing::SecretKeyRef::new(&key);

            let sig = key_ref
                .sign(msg.hash().as_bytes(), unsigned_tx.chain_id.into())
                .unwrap();

            let signed_tx = TransactionV2::Legacy(LegacyTransaction {
                nonce: msg.nonce,
                gas_price: msg.gas_price,
                gas_limit: msg.gas_limit,
                action: msg.action,
                value: msg.value,
                input: msg.input.clone(),
                signature: ethereum::TransactionSignature::new(
                    sig.v,
                    sig.r.0.into(),
                    sig.s.0.into(),
                )
                .unwrap(),
            });

            assert_eq!(
                verify_transaction(
                    &unsigned_tx,
                    &rlp::encode(&signed_tx).to_vec(),
                    &key_ref.address()
                ),
                Ok(signed_tx.hash()),
                "Unable to verify tx signed by key {:?}",
                hex::encode(key.serialize_secret())
            );
        }
    }
}
