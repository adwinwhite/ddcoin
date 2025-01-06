#![feature(never_type)]

use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    time::Duration,
};

use anyhow::{Context, Result};
use iroh::{
    discovery::{
        dns::DnsDiscovery, local_swarm_discovery::LocalSwarmDiscovery, pkarr::PkarrPublisher,
        ConcurrentDiscovery, DiscoveryItem,
    },
    endpoint::{RecvStream, SendStream},
    Endpoint, NodeId, SecretKey,
};
use ractor::{Actor, ActorRef, RpcReplyPort};
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;

const ALPN: &[u8] = b"ddcoin/0.1";
const PEER_SIZE_LIMIT: usize = 3;

#[derive(Eq, PartialEq, Hash, Clone)]
pub struct Transaction {
    pub data: String,
}

pub struct PeerHubActor;

#[derive(Default)]
pub struct PeerHubActorState {
    peers: HashMap<NodeId, SendStream>,
}

#[derive(Debug)]
pub enum PeerHubActorMessage {
    ShouldConnect(NodeId, RpcReplyPort<bool>),
    NewPeer(NodeId, SendStream, RpcReplyPort<bool>),
    Broadcast(NodeId, PeerMessage),
}

impl Actor for PeerHubActor {
    type Msg = PeerHubActorMessage;
    type State = PeerHubActorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        _args: Self::Arguments,
    ) -> std::result::Result<Self::State, ractor::ActorProcessingErr> {
        Ok(Default::default())
    }

    async fn handle(
        &self,
        myself: ractor::ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> std::result::Result<(), ractor::ActorProcessingErr> {
        println!("PeerHubActor received message: {:#?}", message);
        match message {
            PeerHubActorMessage::ShouldConnect(node_id, reply_port) => {
                if state.peers.len() >= PEER_SIZE_LIMIT {
                    myself.cast(PeerHubActorMessage::Broadcast(node_id, PeerMessage::Ping))?;
                    reply_port.send(false)?;
                    println!("Rejecting connection from {:?}", node_id);
                } else {
                    let contains = state.peers.contains_key(&node_id);
                    let should_connect = !contains;
                    reply_port.send(should_connect)?;
                    println!("{} accepting connection from {:?}", should_connect, node_id);
                }
            }
            PeerHubActorMessage::Broadcast(node_id, peer_msg) => {
                for (id, send_stream) in &mut state.peers {
                    if *id == node_id {
                        continue;
                    }
                    let encoded = bincode::serialize(&peer_msg)?;
                    send_stream.write(&encoded).await?;
                }
            }
            PeerHubActorMessage::NewPeer(node_id, send_stream, reply) => {
                if let Entry::Vacant(e) = state.peers.entry(node_id) {
                    e.insert(send_stream);
                    reply.send(true)?;
                } else {
                    reply.send(false)?;
                }
            }
        }
        Ok(())
    }
}

type BoxStream<T> = futures_lite::stream::Boxed<T>;

pub struct NewPeerStreamSubscriber {
    stream: BoxStream<DiscoveryItem>,
    peer_hub: ActorRef<PeerHubActorMessage>,
    endpoint: Endpoint,
}

impl NewPeerStreamSubscriber {
    pub fn new(
        stream: BoxStream<DiscoveryItem>,
        peer_hub: ActorRef<PeerHubActorMessage>,
        endpoint: Endpoint,
    ) -> Self {
        Self {
            stream,
            peer_hub,
            endpoint,
        }
    }

    pub async fn run(mut self) -> Result<!> {
        // TODO: retry these when needed.
        let mut known_ids = HashSet::new();
        while let Some(item) = self.stream.next().await {
            let node_id = item.node_addr.node_id;
            if known_ids.contains(&node_id) {
                continue;
            } else {
                known_ids.insert(node_id);
            }
            println!("Found new peer: {:?}", item);

            let should_connect =
                ractor::call!(self.peer_hub, PeerHubActorMessage::ShouldConnect, node_id)?;
            if !should_connect {
                continue;
            }

            let conn = self.endpoint.connect(item.node_addr, ALPN).await?;
            let (mut tx, rx) = conn.open_bi().await?;

            tx.write(self.endpoint.node_id().as_bytes()).await?;

            let not_exist =
                ractor::call!(self.peer_hub, PeerHubActorMessage::NewPeer, node_id, tx)?;
            if not_exist {
                let peer = Peer::new(rx);
                // TODO: store task handle.
                tokio::spawn(peer.run());
            }
        }
        Err(anyhow::anyhow!("New peer stream ended"))
    }
}

pub struct IncomingConnectionListener {
    endpoint: Endpoint,
    peer_hub: ActorRef<PeerHubActorMessage>,
}

impl IncomingConnectionListener {
    pub fn new(endpoint: Endpoint, peer_hub: ActorRef<PeerHubActorMessage>) -> Self {
        Self { endpoint, peer_hub }
    }

    pub async fn run(self) -> Result<!> {
        loop {
            let pending_conn = self
                .endpoint
                .accept()
                .await
                .context("Endpoint cannot listen anymore")?;
            let conn = pending_conn.accept()?.await?;
            let (tx, mut rx) = conn.accept_bi().await?;
            let mut node_id_buf = [0u8; 32];
            rx.read_exact(&mut node_id_buf).await?;
            let node_id = NodeId::from_bytes(&node_id_buf)?;

            let should_connect =
                ractor::call!(self.peer_hub, PeerHubActorMessage::ShouldConnect, node_id)?;
            if !should_connect {
                continue;
            }

            let not_exist =
                ractor::call!(self.peer_hub, PeerHubActorMessage::NewPeer, node_id, tx)?;
            if not_exist {
                let peer = Peer::new(rx);
                // TODO: store task handle.
                tokio::spawn(peer.run());
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum PeerMessage {
    Ping,
    Pong,
}

pub struct Peer {
    recv_stream: RecvStream,
}

impl Peer {
    pub fn new(recv_stream: RecvStream) -> Self {
        Self { recv_stream }
    }

    pub async fn run(mut self) -> Result<!> {
        let mut len_buf = [0u8; 4];
        loop {
            self.recv_stream.read_exact(&mut len_buf).await?;
            let size = u32::from_be_bytes(len_buf) as usize;
            let mut buf = vec![0u8; size];
            self.recv_stream.read_exact(&mut buf).await?;
            let message: PeerMessage = bincode::deserialize(&buf)?;
            match message {
                PeerMessage::Ping => {
                    println!("Ping received");
                }
                PeerMessage::Pong => {
                    println!("Pong received");
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    // Create an endpoint, it allows creating and accepting
    // connections in the iroh p2p world
    let secret_key = SecretKey::generate(rand::rngs::OsRng);
    let discovery = ConcurrentDiscovery::from_services(vec![
        Box::new(PkarrPublisher::n0_dns(secret_key.clone())),
        Box::new(DnsDiscovery::n0_dns()),
        Box::new(LocalSwarmDiscovery::new(secret_key.public())?),
    ]);
    let endpoint = Endpoint::builder()
        .secret_key(secret_key)
        .discovery(Box::new(discovery))
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await?;
    let discovery = endpoint.discovery().unwrap();
    let peer_stream = discovery.subscribe().unwrap();
    let (peer_hub_actor, _peer_hub_actor_handle) = Actor::spawn(None, PeerHubActor, ()).await?;
    let new_peer_stream_subscriber =
        NewPeerStreamSubscriber::new(peer_stream, peer_hub_actor.clone(), endpoint.clone());
    tokio::spawn(new_peer_stream_subscriber.run());
    let incoming_connection_listener = IncomingConnectionListener::new(endpoint, peer_hub_actor);
    tokio::spawn(incoming_connection_listener.run());

    tokio::time::sleep(Duration::from_secs(u64::MAX)).await;
    Ok(())
}
