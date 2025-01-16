#![feature(never_type)]
#![feature(array_try_from_fn)]

mod block;
mod new_peer_watcher;
mod peer;
mod peerhub;
mod serdes;
mod transaction;
mod util;

use tracing::error;

pub use block::{Block, UnconfirmedBlock};
pub use peerhub::PeerHubActorMessage;
pub use transaction::{CoinAddress, Transaction};

use anyhow::Result;
use iroh::{
    Endpoint,
    discovery::{
        ConcurrentDiscovery, dns::DnsDiscovery, local_swarm_discovery::LocalSwarmDiscovery,
        pkarr::PkarrPublisher,
    },
};
use new_peer_watcher::{IncomingConnectionListener, NewPeerStreamSubscriber};
use peerhub::PeerHubActor;
use ractor::{Actor, ActorRef};
use tokio::task::{JoinHandle, JoinSet};

pub struct Config {
    secret_key: iroh::SecretKey,
    pub discovery: Vec<Box<dyn iroh::discovery::Discovery>>,
    pub alpn: Vec<u8>,
}

impl Default for Config {
    fn default() -> Self {
        let secret_key = iroh::SecretKey::generate(rand::rngs::OsRng);
        let discovery: Vec<Box<dyn iroh::discovery::Discovery>> = vec![
            Box::new(PkarrPublisher::n0_dns(secret_key.clone())),
            Box::new(DnsDiscovery::n0_dns()),
            Box::new(LocalSwarmDiscovery::new(secret_key.public()).unwrap()),
        ];
        Self {
            secret_key,
            discovery,
            alpn: b"ddcoin/1.0".to_vec(),
        }
    }
}

impl Config {
    pub fn with_local_discovery(alpn: &[u8]) -> Self {
        let secret_key = iroh::SecretKey::generate(rand::rngs::OsRng);
        let discovery: Vec<Box<dyn iroh::discovery::Discovery>> = vec![
            // FIXME: avoid this panic case.
            Box::new(LocalSwarmDiscovery::new(secret_key.public()).unwrap()),
        ];
        Self {
            secret_key,
            discovery,
            alpn: alpn.to_vec(),
        }
    }

    pub async fn run(self) -> Result<(ActorRef<PeerHubActorMessage>, JoinHandle<()>)> {
        // Create an endpoint, it allows creating and accepting
        // connections in the iroh p2p world
        let discovery = ConcurrentDiscovery::from_services(self.discovery);
        let endpoint = Endpoint::builder()
            .secret_key(self.secret_key)
            .discovery(Box::new(discovery))
            .alpns(vec![self.alpn.clone()])
            .bind()
            .await?;
        let discovery = endpoint.discovery().unwrap();
        let peer_stream = discovery.subscribe().unwrap();
        let (peer_hub_actor, _peer_hub_actor_handle) =
            Actor::spawn(None, PeerHubActor, endpoint.node_id()).await?;

        let mut tasks = JoinSet::new();
        let new_peer_stream_subscriber = NewPeerStreamSubscriber::new(
            peer_stream,
            peer_hub_actor.clone(),
            endpoint.clone(),
            &self.alpn,
        );
        tasks.spawn(new_peer_stream_subscriber.run());
        let incoming_connection_listener =
            IncomingConnectionListener::new(endpoint, peer_hub_actor.clone());
        tasks.spawn(incoming_connection_listener.run());

        let handle = tokio::spawn(async move {
            while let Some(res) = tasks.join_next().await {
                let Ok(Err(e)) = res else {
                    error!("Peer listener task has error in joining");
                    continue;
                };
                eprintln!("Peer listener task failed: {:?}", e);
            }
        });
        Ok((peer_hub_actor, handle))
    }
}

pub async fn run() -> Result<(ActorRef<PeerHubActorMessage>, JoinHandle<()>)> {
    Config::default().run().await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anyhow::Result;
    use ed25519_dalek::SigningKey;
    use tokio::task::JoinSet;
    use uuid::Uuid;

    use crate::{
        Block, CoinAddress, Config, PeerHubActorMessage, Transaction, UnconfirmedBlock,
        block::{BlockId, NUM_OF_BLOCKS_BEFORE_INCREMENT_ZEROS, SequenceNo, Sha256Hash},
        serdes::transport,
        transaction::{Signature, TransactionId},
        util::hex_to_bytes,
    };

    fn random_alpn() -> Vec<u8> {
        let mut alpn = vec![0; 8];
        for byte in alpn.iter_mut() {
            *byte = rand::random::<u8>();
        }
        alpn
    }

    fn create_transaction() -> Transaction {
        const RECEIVER_PUB_KEY: &str =
            "01a4b29a7fc6127080b9eb962ec4f18a3a61d5e011cc3fa821d5d1d1f30d0ddb";
        let receiver_pub_key = hex_to_bytes(RECEIVER_PUB_KEY);
        let amount = rand::random::<u64>();
        let fee = rand::random::<u64>();
        let receiver_pub_key = CoinAddress::from_bytes(&receiver_pub_key).unwrap();

        let mut csprng = rand::rngs::OsRng;
        let mut signing_key = SigningKey::generate(&mut csprng);
        Transaction::new(&mut signing_key, receiver_pub_key, amount, fee)
    }

    fn create_block(prev_block: &Block) -> Block {
        let mut csprng = rand::rngs::OsRng;
        let mut signing_key = SigningKey::generate(&mut csprng);
        let miner = signing_key.verifying_key().into();
        let txn1 = create_transaction();
        let txn2 = create_transaction();
        let unconfirmed = UnconfirmedBlock {
            sequence_no: prev_block.seqno() + 1,
            id: Uuid::new_v4().into(),
            prev_id: prev_block.id(),
            prev_sha256: prev_block.sha256(),
            transactions: vec![txn1, txn2],
            miner,
        };

        unconfirmed.try_confirm(&mut signing_key).unwrap()
    }

    fn create_invalid_transaction() -> Transaction {
        #[allow(dead_code)]
        struct TransactionInnerViewer {
            id: TransactionId,
            sender: CoinAddress,
            receiver: CoinAddress,
            amount: u64,
            fee: u64,
        }
        #[allow(dead_code)]
        struct TransactionViewer {
            inner: TransactionInnerViewer,
            signature: Signature,
        }

        let original_txn = create_transaction();
        let mut txn_viewer: TransactionViewer = unsafe { std::mem::transmute(original_txn) };
        // Malicious action here.
        txn_viewer.inner.amount = txn_viewer.inner.amount.wrapping_add(1);
        unsafe { std::mem::transmute(txn_viewer) }
    }

    fn create_invalid_block(prev_block: &Block) -> Block {
        #[allow(dead_code)]
        struct BlockInnerViewer {
            sequence_no: SequenceNo,
            id: BlockId,
            prev_id: BlockId,
            prev_sha256: Sha256Hash,
            transactions: Vec<Transaction>,
            miner: CoinAddress,
            nonce: u64,
        }
        #[allow(dead_code)]
        struct BlockViewer {
            inner: BlockInnerViewer,
            signature: Signature,
        }
        let block = create_block(prev_block);
        let mut block_viewer: BlockViewer = unsafe { std::mem::transmute(block) };
        // Malicious action here.
        let mut csprng = rand::rngs::OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let new_miner = signing_key.verifying_key().into();
        block_viewer.inner.miner = new_miner;
        unsafe { std::mem::transmute(block_viewer) }
    }

    #[test]
    fn invalid_transaction() {
        let bad_txn = create_invalid_transaction();
        let bytes = transport::encode(&bad_txn).unwrap();
        let received_txn: Result<Transaction> = transport::decode(&bytes);
        assert!(received_txn.is_err());
    }

    #[test]
    fn invalid_block() {
        let prev_block = Block::GENESIS;
        let bad_block = create_invalid_block(&prev_block);
        let bytes = transport::encode(&bad_block).unwrap();
        let received_block: Result<Block> = transport::decode(&bytes);
        assert!(received_block.is_err());
    }

    #[tokio::test]
    async fn connect_to_peers() {
        async fn query_peer_number(alpn: Vec<u8>) -> Result<usize> {
            let config = Config::with_local_discovery(&alpn);
            let (peer_hub, _task_handle) = config.run().await?;
            // Wait for connection.
            tokio::time::sleep(Duration::from_secs(5)).await;

            let peers = ractor::call!(peer_hub, PeerHubActorMessage::QueryPeers)?;
            Ok(peers.len())
        }
        let mut tasks = JoinSet::new();
        let alpn = random_alpn();
        tasks.spawn(query_peer_number(alpn.clone()));
        tasks.spawn(query_peer_number(alpn.clone()));
        tasks.spawn(query_peer_number(alpn.clone()));
        tasks.spawn(query_peer_number(alpn.clone()));
        while let Some(res) = tasks.join_next().await {
            let res = res.unwrap().unwrap();
            assert_eq!(res, 3);
        }
    }

    #[tokio::test]
    async fn propagate_transactions() {
        // Receive transactions before timeout.
        let mut tasks = JoinSet::new();
        let alpn = random_alpn();
        let txn1 = create_transaction();
        let txn2 = create_transaction();
        {
            let txn1 = txn1.clone();
            let txn2 = txn2.clone();
            let alpn = alpn.clone();
            tasks.spawn(async move {
                let config = Config::with_local_discovery(&alpn);
                let (peer_hub, _peer_hub_handle) = config.run().await?;
                // Wait for connection.
                tokio::time::sleep(Duration::from_secs(2)).await;

                // Send two transactions.
                peer_hub.cast(PeerHubActorMessage::NewTransaction(txn1))?;
                peer_hub.cast(PeerHubActorMessage::NewTransaction(txn2))?;

                // Wait for transaction propagtion.
                tokio::time::sleep(Duration::from_secs(3)).await;

                anyhow::Ok(())
            });
        }
        tasks.spawn(async move {
            let config = Config::with_local_discovery(&alpn);
            let (peer_hub, _peer_hub_handle) = config.run().await?;
            // Wait for connection and transactions.
            tokio::time::sleep(Duration::from_secs(5)).await;

            // Check if transactions are propagated.
            let recv_txn1 =
                ractor::call!(peer_hub, PeerHubActorMessage::QueryTransaction, txn1.id())?;
            assert_eq!(recv_txn1, Some(txn1));
            let recv_txn2 =
                ractor::call!(peer_hub, PeerHubActorMessage::QueryTransaction, txn2.id())?;
            assert_eq!(recv_txn2, Some(txn2));

            anyhow::Ok(())
        });
        while let Some(res) = tasks.join_next().await {
            let res = res.unwrap();
            assert!(res.is_ok());
        }
    }

    #[tokio::test]
    async fn propagate_blocks() {
        // Receive transactions before timeout.
        let mut tasks = JoinSet::new();
        let alpn = random_alpn();
        let mut blocks = Vec::with_capacity((NUM_OF_BLOCKS_BEFORE_INCREMENT_ZEROS as usize) + 2);
        for i in 0..blocks.capacity() {
            let prev_block = if i == 0 {
                &Block::GENESIS
            } else {
                &blocks[i - 1]
            };
            let block = create_block(prev_block);
            blocks.push(block);
        }
        {
            let blocks = blocks.clone();
            let alpn = alpn.clone();
            tasks.spawn(async move {
                let config = Config::with_local_discovery(&alpn);
                let (peer_hub, _peer_hub_handle) = config.run().await?;
                let local_node_id = ractor::call!(peer_hub, PeerHubActorMessage::QueryLocalNodeId)?;
                // Wait for connection.
                tokio::time::sleep(Duration::from_secs(2)).await;

                // Send blocks.
                for block in blocks {
                    peer_hub.cast(PeerHubActorMessage::NewBlock(local_node_id, block))?;
                }

                // Wait for transaction propagtion.
                tokio::time::sleep(Duration::from_secs(3)).await;

                anyhow::Ok(())
            });
        }
        tasks.spawn(async move {
            let config = Config::with_local_discovery(&alpn);
            let (peer_hub, _peer_hub_handle) = config.run().await?;
            // Wait for connection and transactions.
            tokio::time::sleep(Duration::from_secs(5)).await;

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
}
