pub mod util;

use std::time::Duration;

use crate::logging::COMPONENT_KEY;

use super::rpc::EthRpcApi;

use cf_chains::{eth::TrackedData, Ethereum};
use futures::{future, Stream, StreamExt};
use slog::o;

use sp_core::U256;
use web3::types::{BlockNumber, U64};

/// Returns a stream of latest eth block numbers by polling at regular intervals.
///
/// Uses polling.
pub fn poll_latest_block_numbers<'a, EthRpc: EthRpcApi + Send + Sync + 'a>(
    eth_rpc: &'a EthRpc,
    polling_interval: Duration,
    logger: &slog::Logger,
) -> impl Stream<Item = u64> + 'a {
    let logger = logger.new(o!(COMPONENT_KEY => "ETH_Poll_LatestBlockStream"));

    util::tick_stream(polling_interval)
        // Get the latest block number.
        .then(move |_| eth_rpc.block_number())
        // Warn on error.
        .filter_map(move |rpc_result| {
            future::ready(match rpc_result {
                Ok(block_number) => Some(block_number.as_u64()),
                Err(e) => {
                    slog::warn!(logger, "Error fetching ETH block number: {}", e);
                    None
                }
            })
        })
}

pub async fn get_tracked_data<EthRpcClient: EthRpcApi + Send + Sync>(
    rpc: &EthRpcClient,
    block_number: u64,
) -> anyhow::Result<TrackedData<Ethereum>> {
    let fee_history = rpc
        .fee_history(
            U256::one(),
            BlockNumber::Number(U64::from(block_number)),
            Some(vec![0.5]),
        )
        .await?;

    Ok(TrackedData::<Ethereum> {
        block_height: block_number,
        base_fee: fee_history
            .base_fee_per_gas
            .first()
            .expect("Requested, so should be present.")
            .as_u128(),
        priority_fee: fee_history
            .reward
            .expect("Requested, so should be present.")
            .first()
            .expect("Requested, so should be present.")
            .first()
            .expect("Requested, so should be present.")
            .as_u128(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::test_utils::new_test_logger;

    #[tokio::test]
    async fn test_get_tracked_data() {
        use crate::eth::rpc::MockEthRpcApi;

        const BLOCK_HEIGHT: u64 = 42;
        const BASE_FEE: u128 = 40;
        const PRIORITY_FEE: u128 = 5;

        let mut rpc = MockEthRpcApi::new();

        // ** Rpc Api Assumptions **
        rpc.expect_fee_history()
            .once()
            .returning(|_, block_number, _| {
                Ok(web3::types::FeeHistory {
                    oldest_block: block_number,
                    base_fee_per_gas: vec![U256::from(BASE_FEE)],
                    gas_used_ratio: vec![],
                    reward: Some(vec![vec![U256::from(PRIORITY_FEE)]]),
                })
            });
        // ** Rpc Api Assumptions **

        assert_eq!(
            get_tracked_data(&rpc, BLOCK_HEIGHT).await.unwrap(),
            TrackedData {
                block_height: BLOCK_HEIGHT,
                base_fee: BASE_FEE,
                priority_fee: PRIORITY_FEE,
            }
        );
    }

    #[tokio::test]
    async fn test_poll_latest_block_numbers() {
        use crate::eth::rpc::MockEthRpcApi;

        const BLOCK_COUNT: u64 = 10;
        let mut block_numbers = (0..BLOCK_COUNT).map(Into::into);

        let mut rpc = MockEthRpcApi::new();
        let logger = new_test_logger();

        // ** Rpc Api Assumptions **
        rpc.expect_block_number()
            .times(BLOCK_COUNT as usize)
            .returning(move || {
                block_numbers
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("No more block numbers"))
            });
        // ** Rpc Api Assumptions **

        assert_eq!(
            poll_latest_block_numbers(&rpc, Duration::from_millis(10), &logger)
                .collect::<Vec<_>>()
                .await,
            (0..BLOCK_COUNT).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn test_poll_latest_block_numbers_skips_errors() {
        use crate::eth::rpc::MockEthRpcApi;

        const BLOCK_COUNT: u64 = 10;
        let mut block_numbers = (0..BLOCK_COUNT).map(Into::into);

        let mut rpc = MockEthRpcApi::new();
        let logger = new_test_logger();

        // ** Rpc Api Assumptions **
        rpc.expect_block_number()
            .times(BLOCK_COUNT as usize)
            .returning(move || {
                block_numbers
                    .next()
                    .and_then(|n: web3::types::U64| if n.as_usize() < 5 { None } else { Some(n) })
                    .ok_or_else(|| anyhow::anyhow!("No more block numbers"))
            });
        // ** Rpc Api Assumptions **

        assert_eq!(
            poll_latest_block_numbers(&rpc, Duration::from_millis(10), &logger)
                .collect::<Vec<_>>()
                .await,
            (0..BLOCK_COUNT).filter(|n| *n >= 5).collect::<Vec<_>>()
        );
    }
}
