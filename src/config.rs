use std::time::Duration;

use anyhow::Result;
use ethnum::U256;
use iroh::{
    Endpoint, SecretKey,
    discovery::{
        ConcurrentDiscovery, dns::DnsDiscovery, local_swarm_discovery::LocalSwarmDiscovery,
        pkarr::PkarrPublisher,
    },
};
use ractor::{Actor, ActorRef, concurrency::JoinSet};
use tokio::task::JoinHandle;
use tracing::error;

use crate::{
    PeerHubActorMessage,
    new_peer_watcher::{IncomingConnectionListener, NewPeerStreamSubscriber},
    peerhub::PeerHubActor,
    util::{Difficulty, Timestamp, TimestampExt},
};

#[derive(Debug, Clone)]
pub struct Config {
    difficulty_adjustment_period: u64,
    average_block_time: Duration,
    block_txn_limit: usize,
    genesis_difficulty: Difficulty,
    genesis_timestamp: Timestamp,
    alpn: Vec<u8>,
}

impl Default for Config {
    fn default() -> Self {
        // let secret_key = iroh::SecretKey::generate(rand::rngs::OsRng);
        // let discovery: Vec<Box<dyn iroh::discovery::Discovery>> = vec![
        // Box::new(PkarrPublisher::n0_dns(secret_key.clone())),
        // Box::new(DnsDiscovery::n0_dns()),
        // Box::new(LocalSwarmDiscovery::new(secret_key.public()).unwrap()),
        // ];
        Self {
            difficulty_adjustment_period: 2016,
            average_block_time: Duration::from_secs(60 * 10),
            block_txn_limit: 20,
            // FIXME: find a proper difficulty.
            genesis_difficulty: U256::from(u128::MAX).into(),
            genesis_timestamp: Duration::from_secs(1737615026).as_nanos(),
            alpn: b"ddcoin/1.0".to_vec(),
        }
    }
}

impl Config {
    #[cfg(feature = "test_util")]
    pub const TESTING_PARTIAL: Self = Self {
        alpn: Vec::new(),
        difficulty_adjustment_period: 10,
        average_block_time: Duration::from_millis(10),
        block_txn_limit: 20,
        genesis_difficulty: Difficulty::new(U256::from_words(u128::MAX >> 1, u128::MAX)),
        genesis_timestamp: Duration::from_secs(1737615026).as_nanos(),
    };

    #[cfg(feature = "test_util")]
    pub fn with_alpn(alpn: &[u8]) -> Self {
        Self {
            alpn: alpn.to_vec(),
            genesis_timestamp: Timestamp::now(),
            ..Self::TESTING_PARTIAL
        }
    }

    // Generate getters for fields.
    pub fn difficulty_adjustment_period(&self) -> u64 {
        self.difficulty_adjustment_period
    }

    pub fn average_block_time(&self) -> Duration {
        self.average_block_time
    }

    pub fn block_txn_limit(&self) -> usize {
        self.block_txn_limit
    }

    pub fn genesis_difficulty(&self) -> &Difficulty {
        &self.genesis_difficulty
    }

    pub fn genesis_timestamp(&self) -> Timestamp {
        self.genesis_timestamp
    }

    pub fn alpn(&self) -> &Vec<u8> {
        &self.alpn
    }

    pub async fn run_with_local_discovery(
        self,
    ) -> Result<(ActorRef<PeerHubActorMessage>, JoinHandle<()>)> {
        let secret_key = iroh::SecretKey::generate(rand::rngs::OsRng);
        let discovery: Vec<Box<dyn iroh::discovery::Discovery>> = vec![
            // FIXME: avoid this panic case.
            Box::new(LocalSwarmDiscovery::new(secret_key.public()).unwrap()),
        ];

        self.run_with_secret_and_discovery(secret_key, discovery)
            .await
    }

    pub async fn run(self) -> Result<(ActorRef<PeerHubActorMessage>, JoinHandle<()>)> {
        let secret_key = iroh::SecretKey::generate(rand::rngs::OsRng);
        let discovery: Vec<Box<dyn iroh::discovery::Discovery>> = vec![
            Box::new(PkarrPublisher::n0_dns(secret_key.clone())),
            Box::new(DnsDiscovery::n0_dns()),
            Box::new(LocalSwarmDiscovery::new(secret_key.public()).unwrap()),
        ];
        self.run_with_secret_and_discovery(secret_key, discovery)
            .await
    }

    async fn run_with_secret_and_discovery(
        self,
        secret_key: SecretKey,
        discovery: Vec<Box<dyn iroh::discovery::Discovery>>,
    ) -> Result<(ActorRef<PeerHubActorMessage>, JoinHandle<()>)> {
        // Create an endpoint, it allows creating and accepting
        // connections in the iroh p2p world
        let discovery = ConcurrentDiscovery::from_services(discovery);
        let endpoint = Endpoint::builder()
            .secret_key(secret_key)
            .discovery(Box::new(discovery))
            .alpns(vec![self.alpn.clone()])
            .bind()
            .await?;
        let discovery = endpoint.discovery().unwrap();
        let peer_stream = discovery.subscribe().unwrap();
        let alpn = self.alpn.clone();
        let (peer_hub_actor, _peer_hub_actor_handle) =
            Actor::spawn(None, PeerHubActor, (endpoint.node_id(), self)).await?;

        let mut tasks = JoinSet::new();
        let new_peer_stream_subscriber = NewPeerStreamSubscriber::new(
            peer_stream,
            peer_hub_actor.clone(),
            endpoint.clone(),
            &alpn,
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
