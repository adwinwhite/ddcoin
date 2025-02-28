#![feature(never_type)]

use anyhow::Result;
use ddcoin::{CoinAddress, PeerHubActorMessage, UnconfirmedBlock, hub_helper::HubHelper};
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
        let (peer_hub, _handle) = if cfg!(feature = "test_util") {
            self.peerhub_config.run_with_local_discovery().await?
        } else {
            self.peerhub_config.run().await?
        };
        let difficulty_adjustment_period = peerhub_config.difficulty_adjustment_period();
        loop {
            let leading_block = peer_hub.leading_block().await?;
            let mut fees = ractor::call!(peer_hub, PeerHubActorMessage::QueryMemPool)?;
            fees.sort_by_key(|k| k.1);
            let top_ids = fees
                .into_iter()
                .take(peerhub_config.block_txn_limit())
                .map(|(id, _)| id)
                .collect::<Vec<_>>();
            let txns = ractor::call!(peer_hub, PeerHubActorMessage::QueryTransactions, top_ids)?;
            let txns = txns.into_iter().flatten().collect::<Vec<_>>();
            let new_seqno = leading_block.seqno() + 1;
            let unconfirmed_block = if new_seqno % difficulty_adjustment_period == 0 {
                let prev_adjustment_time = ractor::call!(
                    peer_hub,
                    PeerHubActorMessage::QueryDifficultyAdjustTime,
                    new_seqno
                )?;
                UnconfirmedBlock::new(
                    &leading_block,
                    self.miner.clone(),
                    txns,
                    Some(prev_adjustment_time),
                    &peerhub_config,
                )?
            } else {
                UnconfirmedBlock::new(
                    &leading_block,
                    self.miner.clone(),
                    txns,
                    None,
                    &peerhub_config,
                )?
            };
            let block = unconfirmed_block.try_confirm(&mut self.signing_key)?;
            ractor::cast!(peer_hub, PeerHubActorMessage::NewBlock(None, block))?;
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
    use std::time::Duration;

    use anyhow::Result;
    use tokio::task::JoinSet;

    use ddcoin::hub_helper::HubHelper;
    use ddcoin::test_util::{config_with_random_alpn, random_address};

    use crate::MinerConfig;

    #[tokio::test]
    async fn test_single_node() -> Result<()> {
        let config = config_with_random_alpn();
        let mut tasks = JoinSet::new();

        let mut csprng = rand::rngs::OsRng;
        let mut signing_key = ed25519_dalek::SigningKey::generate(&mut csprng);
        let miner: ddcoin::CoinAddress = signing_key.verifying_key().into();
        let miner_config = MinerConfig::new(signing_key.clone(), miner, config.clone());
        tasks.spawn(miner_config.run());

        let (peer_hub, _peer_hub_handle) = config.clone().run().await?;

        // Wait for peer discovery.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // Wait for mining so that our miner has some cash.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                // Check there's block mined
                let leading_block = peer_hub.leading_block().await?;
                if leading_block.seqno() > 0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            anyhow::Ok(())
        })
        .await
        .map_err(|_| anyhow::anyhow!("timeout mining the first block"))??;

        // Submit transactions.
        let fourth_of_subsidy = config.initial_block_subsidy() / 4;
        let txn_id0 = peer_hub
            .new_transfer(
                &mut signing_key,
                &random_address(),
                fourth_of_subsidy,
                fourth_of_subsidy,
            )
            .await?;
        let txn_id1 = peer_hub
            .new_transfer(
                &mut signing_key,
                &random_address(),
                fourth_of_subsidy,
                fourth_of_subsidy,
            )
            .await?;
        // Check that transactions are in the chain.
        // Wait for mining.
        let initial_seqno = peer_hub.leading_block().await?.seqno();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                // Check there's block mined
                let leading_block = peer_hub.leading_block().await?;
                if leading_block.seqno() > initial_seqno {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            anyhow::Ok(())
        })
        .await
        .map_err(|_| {
            anyhow::anyhow!("timeout mining the first block after submitting transactions")
        })??;

        let depth0 = peer_hub.transaction_depth(txn_id0).await?;
        assert!(depth0.is_some());
        let depth1 = peer_hub.transaction_depth(txn_id1).await?;
        assert!(depth1.is_some());

        Ok(())
    }
}
