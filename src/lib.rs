#![feature(never_type)]
#![feature(array_try_from_fn)]
#![feature(hash_extract_if)]

mod block;
mod config;
pub mod hub_helper;
mod new_peer_watcher;
mod peer;
mod peerhub;
mod serdes;
mod transaction;
mod util;

#[cfg(feature = "test_util")]
pub mod test_util;

pub use block::{Block, UnconfirmedBlock};
pub use config::Config;
pub use peerhub::PeerHubActorMessage;
pub use transaction::{CoinAddress, Transaction};

use anyhow::Result;
use ractor::ActorRef;
use tokio::task::JoinHandle;

pub async fn run() -> Result<(ActorRef<PeerHubActorMessage>, JoinHandle<()>)> {
    Config::default().run().await
}

// TODO: add more tests against validation.
#[cfg(feature = "test_util")]
#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::hub_helper::HubHelper;
    use crate::test_util::{
        config_with_random_alpn, create_empty_block, create_empty_chain, create_invalid_block,
        create_invalid_transaction, create_transaction,
    };
    use anyhow::Result;
    use tokio::task::JoinSet;

    use crate::{Block, Config, PeerHubActorMessage, Transaction, serdes::transport};

    #[test]
    fn invalid_transaction() {
        let bad_txn = create_invalid_transaction();
        let bytes = transport::encode(&bad_txn).unwrap();
        let received_txn: Result<Transaction> = transport::decode(&bytes);
        assert!(received_txn.is_err());
    }

    #[test]
    fn invalid_block() {
        let config = Config::INCOMPLETE_TESTING_CONFIG;
        let blocks = create_empty_chain(&config, 1);
        let bad_block = create_invalid_block(&blocks);
        let bytes = transport::encode(&bad_block).unwrap();
        let received_block: Result<Block> = transport::decode(&bytes);
        assert!(received_block.is_err());
    }

    #[tokio::test]
    async fn connect_to_peers() {
        async fn query_peer_number(config: Config) -> Result<usize> {
            let (peer_hub, _task_handle) = config.run_with_local_discovery().await?;
            // Wait for connection.
            tokio::time::sleep(Duration::from_secs(5)).await;

            let peers = peer_hub.peers().await?;
            Ok(peers.len())
        }
        let mut tasks = JoinSet::new();
        let config = config_with_random_alpn();
        tasks.spawn(query_peer_number(config.clone()));
        tasks.spawn(query_peer_number(config.clone()));
        tasks.spawn(query_peer_number(config.clone()));
        tasks.spawn(query_peer_number(config.clone()));
        while let Some(res) = tasks.join_next().await {
            let res = res.unwrap().unwrap();
            assert_eq!(res, 3);
        }
    }

    #[tokio::test]
    async fn propagate_transactions() {
        // Receive transactions before timeout.
        let mut tasks = JoinSet::new();
        let config = config_with_random_alpn();
        let txn1 = create_transaction();
        let txn2 = create_transaction();
        {
            let txn1 = txn1.clone();
            let txn2 = txn2.clone();
            let config = config.clone();
            tasks.spawn(async move {
                let (peer_hub, _peer_hub_handle) = config.run_with_local_discovery().await?;
                // Wait for connection.
                tokio::time::sleep(Duration::from_secs(3)).await;

                // Send two transactions.
                peer_hub.cast(PeerHubActorMessage::NewTransaction(txn1))?;
                peer_hub.cast(PeerHubActorMessage::NewTransaction(txn2))?;

                // Wait for transaction propagtion.
                tokio::time::sleep(Duration::from_secs(2)).await;

                anyhow::Ok(())
            });
        }
        tasks.spawn(async move {
            let (peer_hub, _peer_hub_handle) = config.run_with_local_discovery().await?;
            // Wait for connection and transactions.
            tokio::time::timeout(Duration::from_secs(8), async {
                // Check if transactions are propagated.
                let recv_txn1 = loop {
                    let Some(txn) = peer_hub.get_transaction(txn1.id()).await? else {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    };
                    break txn;
                };
                assert_eq!(recv_txn1, txn1);
                let recv_txn2 = loop {
                    let Some(txn) = peer_hub.get_transaction(txn2.id()).await? else {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    };
                    break txn;
                };
                assert_eq!(recv_txn2, txn2);
                anyhow::Ok(())
            })
            .await??;

            anyhow::Ok(())
        });
        while let Some(res) = tasks.join_next().await {
            // FIXME: why join error? Panic. Why panic? Assertion failure.
            let res = res.unwrap();
            assert!(res.is_ok());
        }
    }

    #[tokio::test]
    async fn propagate_blocks() {
        // Receive transactions before timeout.
        let mut tasks = JoinSet::new();
        let config = config_with_random_alpn();
        let blocks = create_empty_chain(
            &config,
            (Config::INCOMPLETE_TESTING_CONFIG.difficulty_adjustment_period() as usize) + 2,
        );

        {
            let blocks = blocks.clone();
            let config = config.clone();
            tasks.spawn(async move {
                let (peer_hub, _peer_hub_handle) = config.run_with_local_discovery().await?;
                // Wait for connection.
                tokio::time::sleep(Duration::from_secs(3)).await;

                // Send blocks.
                for block in blocks {
                    peer_hub.cast(PeerHubActorMessage::NewBlock(None, block))?;
                }

                // Wait for transaction propagtion.
                tokio::time::sleep(Duration::from_secs(3)).await;

                anyhow::Ok(())
            });
        }
        tasks.spawn(async move {
            let (peer_hub, _peer_hub_handle) = config.run_with_local_discovery().await?;
            // Wait for connection and transactions.
            tokio::time::sleep(Duration::from_secs(6)).await;

            // Check if blocks are propagated.
            for block in blocks {
                let recv_block =
                    ractor::call!(peer_hub, PeerHubActorMessage::QueryBlock, block.id())?;
                assert_eq!(recv_block, Some(block));
            }

            anyhow::Ok(())
        });
        while let Some(res) = tasks.join_next().await {
            let res = res.unwrap();
            assert!(res.is_ok());
        }
    }

    #[tokio::test]
    async fn leading_block_with_forks() {
        // Receive transactions before timeout.
        let mut tasks = JoinSet::new();
        let config = config_with_random_alpn();

        let old_chain = create_empty_chain(
            &config,
            (Config::INCOMPLETE_TESTING_CONFIG.difficulty_adjustment_period() as usize) + 2,
        );

        let mut fork1 = old_chain[..old_chain.len() - 4].to_vec();
        for _ in 0..6 {
            let block = create_empty_block(&fork1);
            fork1.push(block);
        }
        let correct_leading_block = fork1.last().unwrap().id();
        let mut fork2 = old_chain[..old_chain.len() - 4].to_vec();
        for _ in 0..5 {
            let block = create_empty_block(&fork2);
            fork2.push(block);
        }
        {
            let config = config.clone();
            tasks.spawn(async move {
                let (peer_hub, _peer_hub_handle) = config.run_with_local_discovery().await?;
                // Wait for connection.
                tokio::time::sleep(Duration::from_secs(3)).await;

                // Send old_chain.
                for block in old_chain {
                    peer_hub.cast(PeerHubActorMessage::NewBlock(None, block))?;
                }
                for block in fork1 {
                    peer_hub.cast(PeerHubActorMessage::NewBlock(None, block))?;
                }
                for block in fork2 {
                    peer_hub.cast(PeerHubActorMessage::NewBlock(None, block))?;
                }

                // Wait for transaction propagtion.
                tokio::time::sleep(Duration::from_secs(3)).await;

                anyhow::Ok(())
            });
        }
        tasks.spawn(async move {
            let (peer_hub, _peer_hub_handle) = config.run_with_local_discovery().await?;
            // Wait for connection and transactions.
            tokio::time::sleep(Duration::from_secs(6)).await;

            // Check if the leading block is correct.
            let leading_id = ractor::call!(peer_hub, PeerHubActorMessage::QueryLeadingBlock)?;
            assert_eq!(leading_id, correct_leading_block);

            anyhow::Ok(())
        });
        while let Some(res) = tasks.join_next().await {
            let res = res.unwrap();
            assert!(res.is_ok());
        }
    }
}
