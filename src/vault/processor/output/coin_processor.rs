use crate::{
    common::Coin,
    transactions::OutputSentTx,
    transactions::OutputTx,
    utils::bip44,
    vault::{
        blockchain_connection::{btc::BitcoinClient, ethereum::EthereumClient},
        config::VAULT_CONFIG,
        transactions::TransactionProvider,
    },
};
use async_trait::async_trait;

use super::senders::{btc::BtcOutputSender, ethereum::EthOutputSender, OutputSender};

/// Handy trait for injecting custom processing code during testing
#[async_trait]
pub trait CoinProcessor {
    /// Send outputs using corresponding "sender" for each coin
    async fn process<T: TransactionProvider + Sync>(
        &self,
        provider: &T,
        coin: Coin,
        outputs: &[OutputTx],
    ) -> Vec<OutputSentTx>;
}

/// Struct responsible for sending outputs all supported coin types
pub struct OutputCoinProcessor<L: OutputSender, E: EthereumClient, B: BitcoinClient> {
    loki: L,
    eth: E,
    btc: B,
}

impl<L: OutputSender, E: EthereumClient, B: BitcoinClient> OutputCoinProcessor<L, E, B> {
    /// Create a new output coin processor
    pub fn new(loki: L, eth: E, btc: B) -> Self {
        OutputCoinProcessor { eth, btc, loki }
    }
}

#[async_trait]
impl<L, E, B> CoinProcessor for OutputCoinProcessor<L, E, B>
where
    L: OutputSender + Sync + Send,
    E: EthereumClient + Clone + Sync + Send,
    B: BitcoinClient + Clone + Sync + Send,
{
    async fn process<T: TransactionProvider + Sync>(
        &self,
        provider: &T,
        coin: Coin,
        outputs: &[OutputTx],
    ) -> Vec<OutputSentTx> {
        match coin {
            Coin::ETH => {
                let root_key = match bip44::RawKey::decode(&VAULT_CONFIG.eth.master_root_key) {
                    Ok(key) => key,
                    Err(_) => {
                        error!("Failed to generate root key from eth master root key");
                        return vec![];
                    }
                };
                let sender = EthOutputSender::new(self.eth.clone(), root_key);
                sender.send(provider, outputs).await
            }
            Coin::BTC => {
                let root_key = match bip44::RawKey::decode(&VAULT_CONFIG.btc.master_root_key) {
                    Ok(key) => key,
                    Err(_) => {
                        error!("Failed to generate root key from btc master root key");
                        return vec![];
                    }
                };
                let sender = BtcOutputSender::new(self.btc.clone(), root_key);
                sender.send(provider, outputs).await
            }
            Coin::LOKI => self.loki.send(provider, outputs).await,
        }
    }
}
