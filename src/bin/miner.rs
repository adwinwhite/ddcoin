#![feature(never_type)]

use anyhow::Result;
use ddcoin::{CoinAddress, PeerHubActorMessage, UnconfirmedBlock};
use ed25519_dalek::SigningKey;
use ractor::concurrency::Duration;

struct MinerConfig {
    signing_key: SigningKey,
    miner: CoinAddress,
    peerhub_config: ddcoin::Config,
}

impl MinerConfig {
    fn new(signing_key: SigningKey, miner: CoinAddress, peerhub_config: ddcoin::Config) -> Self {
        Self {
            signing_key,
            miner,
            peerhub_config,
        }
    }

    async fn run(mut self) -> Result<!> {
        let peerhub_config = self.peerhub_config.clone();
        let (peer_hub, _handle) = self.peerhub_config.run().await?;
        loop {
            let leading_block = {
                let id = ractor::call!(peer_hub, PeerHubActorMessage::QueryLeadingBlock)?;
                // peerhub is bound to have leading block otherwise something is going wrong.
                ractor::call!(peer_hub, PeerHubActorMessage::QueryBlock, id)?.unwrap()
            };
            let prev_adjustment_time =
                ractor::call!(peer_hub, PeerHubActorMessage::QueryDifficultyAdjustTime)?;
            let mut amounts = ractor::call!(peer_hub, PeerHubActorMessage::QueryMemPool)?;
            amounts.sort_by_key(|k| k.1);
            let top_ids = amounts
                .into_iter()
                .take(peerhub_config.block_txn_limit() as usize)
                .map(|(id, _)| id)
                .collect::<Vec<_>>();
            let txns = ractor::call!(peer_hub, PeerHubActorMessage::QueryTransactions, top_ids)?;
            let txns = txns.into_iter().flatten().collect::<Vec<_>>();
            // FIXME: allow block with no transaction or not. Will produce a lot of empty blocks.
            if !txns.is_empty() {
                let unconfirmed_block = UnconfirmedBlock::new(
                    &leading_block,
                    self.miner.clone(),
                    txns,
                    Some(prev_adjustment_time),
                    &peerhub_config,
                )?;
                let block = unconfirmed_block.try_confirm(&mut self.signing_key)?;
                ractor::cast!(peer_hub, PeerHubActorMessage::NewBlock(None, block))?;
            }
            // FIXME: need to make this event driven.
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[tokio::main]
async fn main() -> Result<!> {
    tracing_subscriber::fmt::init();
    // TODO: get key from env var.
    let mut csprng = rand::rngs::OsRng;
    let signing_key = SigningKey::generate(&mut csprng);
    let miner: CoinAddress = signing_key.verifying_key().into();

    let config = ddcoin::Config::default();
    let miner_config = MinerConfig::new(signing_key, miner, config);
    miner_config.run().await
}

#[cfg(feature = "test_util")]
#[cfg(test)]
mod tests {
    use anyhow::Result;
    use ddcoin::test_util::{create_transaction, random_alpn};
    use tokio::task::JoinSet;

    use crate::MinerConfig;

    #[tokio::test]
    async fn test_single_node() -> Result<()> {
        let config = config_with_random_alpn();
        let mut tasks = JoinSet::new();
        {
            let mut csprng = rand::rngs::OsRng;
            let signing_key = ed25519_dalek::SigningKey::generate(&mut csprng);
            let miner: ddcoin::CoinAddress = signing_key.verifying_key().into();
            let config = ddcoin::Config::with_local_discovery(&alpn);
            let miner_config = MinerConfig::new(signing_key, miner, config);
            tasks.spawn(miner_config.run());
        }
        let config = ddcoin::Config::with_local_discovery(&alpn);
        let (peer_hub, _peer_hub_handle) = config.run().await?;

        // Wait for peer discovery.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // Check sent transactions are in blocks.
        let txns: Vec<_> = (0..(2 * ddcoin::Block::BLOCK_TXN_LIMIT))
            .map(|_| create_transaction())
            .collect();
        let txn_ids = txns.iter().map(|txn| txn.id()).collect::<Vec<_>>();
        txns.into_iter().for_each(|txn| {
            peer_hub
                .cast(ddcoin::PeerHubActorMessage::NewTransaction(txn))
                .unwrap();
        });
        // Wait for block mining and propagation.
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        let depths = ractor::call!(
            peer_hub,
            ddcoin::PeerHubActorMessage::QueryTransactionDepths,
            txn_ids
        )?;
        depths.into_iter().for_each(|depth| {
            assert!(depth.is_some());
        });

        Ok(())
    }
}
