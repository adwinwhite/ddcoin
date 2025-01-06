#![feature(never_type)]

use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    hash::Hash,
};

use anyhow::{Context, Result};
use ed25519_dalek::{ed25519::signature::SignerMut, SigningKey, VerifyingKey};
use iroh::{
    discovery::{
        dns::DnsDiscovery, local_swarm_discovery::LocalSwarmDiscovery, pkarr::PkarrPublisher,
        ConcurrentDiscovery, DiscoveryItem,
    },
    endpoint::{RecvStream, SendStream},
    Endpoint, NodeId, SecretKey,
};
use ractor::{Actor, ActorRef, RpcReplyPort};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use tokio::{io::AsyncWriteExt, select, task::JoinSet};
use tokio_stream::StreamExt;
use uuid::Uuid;

const ALPN: &[u8] = b"ddcoin/0.1";
const PEER_SIZE_LIMIT: usize = 3;

const BINCODE_CONFIG: bincode::config::Configuration = bincode::config::standard();

pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>> {
    let encoded = bincode::serde::encode_to_vec(msg, BINCODE_CONFIG)?;
    Ok(encoded)
}

pub fn decode<T: for<'de> Deserialize<'de>>(buf: &[u8]) -> Result<T> {
    let decoded = bincode::serde::decode_from_slice(buf, BINCODE_CONFIG)?.0;
    Ok(decoded)
}

#[derive(Eq, PartialEq, Hash, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct TransactionId {
    #[serde(with = "uuid::serde::compact")]
    id: Uuid,
}

#[derive(Eq, PartialEq, Hash, Clone, Debug, Serialize)]
pub struct CoinAddress {
    pub_key: VerifyingKey,
}

#[derive(Eq, PartialEq, Clone, Debug)]
pub struct Signature {
    sig: ed25519_dalek::Signature,
}

impl Hash for Signature {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.sig.to_bytes().hash(state);
    }
}

#[derive(Eq, PartialEq, Hash, Clone, Debug, Serialize)]
struct TransactionInner {
    id: TransactionId,
    sender: CoinAddress,
    receiver: CoinAddress,
    amount: u64,
    fee: u64,
}

#[derive(Eq, PartialEq, Hash, Clone, Debug)]
pub struct Transaction {
    inner: TransactionInner,
    signature: Signature,
}

impl Transaction {
    pub fn new(
        mut signing_key: ed25519_dalek::SigningKey,
        receiver: CoinAddress,
        amount: u64,
        fee: u64,
    ) -> Self {
        let id = Uuid::new_v4();
        let sender = CoinAddress {
            pub_key: signing_key.verifying_key(),
        };
        let inner = TransactionInner {
            id: TransactionId { id },
            sender,
            receiver,
            amount,
            fee,
        };
        let msg = encode(&inner).unwrap();
        let signature = signing_key.sign(&msg);
        let signature = Signature { sig: signature };
        Self { inner, signature }
    }
}

pub struct PeerHubActor;

pub struct PeerHubActorState {
    peers: HashMap<NodeId, SendStream>,
    memppool: HashSet<Transaction>,
    local_node_id: NodeId,
}

impl PeerHubActorState {
    pub fn new(local_node_id: NodeId) -> Self {
        Self {
            peers: HashMap::new(),
            memppool: HashSet::new(),
            local_node_id,
        }
    }
}

// FIXME: optimize enum size here.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum PeerHubActorMessage {
    ShouldConnect(NodeId, RpcReplyPort<bool>),
    NewPeer(NodeId, SendStream, RpcReplyPort<bool>),
    Broadcast(NodeId, BroadcastMessage),
    NewTransaction(Transaction),
}

impl Actor for PeerHubActor {
    type Msg = PeerHubActorMessage;
    type State = PeerHubActorState;
    type Arguments = NodeId;

    async fn pre_start(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> std::result::Result<Self::State, ractor::ActorProcessingErr> {
        Ok(PeerHubActorState::new(args))
    }

    async fn handle(
        &self,
        myself: ractor::ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> std::result::Result<(), ractor::ActorProcessingErr> {
        match message {
            PeerHubActorMessage::ShouldConnect(node_id, reply_port) => {
                println!("PeerHubActor received: ShouldConnect {:?}", node_id);
                if state.peers.len() >= PEER_SIZE_LIMIT {
                    reply_port.send(false)?;
                    println!("Rejecting connection from {:?}", node_id);
                } else {
                    let contains = state.peers.contains_key(&node_id);
                    let should_connect = !contains;
                    reply_port.send(should_connect)?;
                    println!("{} accepting connection from {:?}", should_connect, node_id);
                }
            }
            PeerHubActorMessage::Broadcast(node_id, broadcast_msg) => {
                println!(
                    "PeerHubActor received: Broadcast {:?} - {:?}",
                    node_id, broadcast_msg
                );
                let peer_msg = PeerMessage::Broadcast(broadcast_msg);
                let encoded = encode(&peer_msg)?;
                let len = encoded.len() as u32;
                for (id, send_stream) in &mut state.peers {
                    if *id == node_id {
                        continue;
                    }
                    send_stream.write_all(&len.to_be_bytes()).await?;
                    send_stream.write_all(&encoded).await?;
                    send_stream.flush().await?;
                }
            }
            PeerHubActorMessage::NewPeer(node_id, send_stream, reply) => {
                println!("PeerHubActor received: NewPeer {:?}", node_id);
                if let Entry::Vacant(e) = state.peers.entry(node_id) {
                    e.insert(send_stream);
                    reply.send(true)?;
                    // FIXME: remove this test.
                    test_announce_transaction(&myself)?;
                } else {
                    reply.send(false)?;
                }
            }
            PeerHubActorMessage::NewTransaction(txn) => {
                println!("PeerHubActor received: NewTransaction {:?}", txn.inner.id);
                ractor::cast!(
                    myself,
                    PeerHubActorMessage::Broadcast(
                        state.local_node_id,
                        BroadcastMessage::AnnounceTransaction(txn.inner.id)
                    )
                )?;
                state.memppool.insert(txn);
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
        let mut tasks = JoinSet::new();
        loop {
            select! {
                item = self.stream.next() => {
                    let item = item.context("New peer stream ended")?;
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

                    tx.write_all(self.endpoint.node_id().as_bytes()).await?;
                    tx.flush().await?;

                    let not_exist =
                        ractor::call!(self.peer_hub, PeerHubActorMessage::NewPeer, node_id, tx)?;
                    if not_exist {
                        let peer = Peer::new(rx, self.peer_hub.clone(), node_id);
                        tasks.spawn(peer.run());
                    }
                },
                Some(res) = tasks.join_next() => {
                    // TODO: deal with this better. Remember my result is `Ok(Result<!>)`.
                    println!("Peer task finished: {:?}", res);
                }
            }
        }
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
        let mut tasks = JoinSet::new();
        loop {
            select! {
                pending_conn = self.endpoint.accept() => {
                    let pending_conn = pending_conn.context("Endpoint cannot listen anymore")?;
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
                        let peer = Peer::new(rx, self.peer_hub.clone(), node_id);
                        tasks.spawn(peer.run());
                    }
                },
                Some(res) = tasks.join_next() => {
                    // TODO: deal with this better. Remember my result is `Ok(Result<!>)`.
                    println!("Peer task finished: {:?}", res);
                }
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum BroadcastMessage {
    AnnounceTransaction(TransactionId),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum PeerMessage {
    Broadcast(BroadcastMessage),
}

pub struct Peer {
    recv_stream: RecvStream,
    peer_hub: ActorRef<PeerHubActorMessage>,
    node_id: NodeId,
}

impl Peer {
    pub fn new(
        recv_stream: RecvStream,
        peer_hub: ActorRef<PeerHubActorMessage>,
        node_id: NodeId,
    ) -> Self {
        Self {
            recv_stream,
            peer_hub,
            node_id,
        }
    }

    pub async fn run(mut self) -> Result<!> {
        println!("Peer starts listening: {:?}", self.node_id);
        let mut len_buf = [0u8; 4];
        loop {
            self.recv_stream.read_exact(&mut len_buf).await?;
            let size = u32::from_be_bytes(len_buf) as usize;
            let mut buf = vec![0u8; size];
            self.recv_stream.read_exact(&mut buf).await?;
            let message: PeerMessage = decode(&buf)?;
            match message {
                PeerMessage::Broadcast(msg) => {
                    match msg {
                        BroadcastMessage::AnnounceTransaction(txn_id) => {
                            // TODO: fetch transaction by id.
                            println!("Peer received: transaction announcement - {:?}", txn_id);
                        }
                    }
                    ractor::cast!(
                        self.peer_hub,
                        PeerHubActorMessage::Broadcast(self.node_id, msg)
                    )?;
                }
            }
        }
    }
}

fn test_announce_transaction(peer_hub: &ActorRef<PeerHubActorMessage>) -> Result<()> {
    let mut csprng = OsRng;
    let signing_key = SigningKey::generate(&mut csprng);
    let receiver_address = CoinAddress {
        pub_key: VerifyingKey::from_bytes(&[
            215, 90, 152, 1, 130, 177, 10, 183, 213, 75, 254, 211, 201, 100, 7, 58, 14, 225, 114,
            243, 218, 166, 35, 37, 175, 2, 26, 104, 247, 7, 81, 26,
        ])?,
    };
    let txn = Transaction::new(signing_key, receiver_address, 100, 1);
    println!("Created new transaction: {:?}", txn.inner.id);
    peer_hub.cast(PeerHubActorMessage::NewTransaction(txn))?;

    Ok(())
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
    let (peer_hub_actor, _peer_hub_actor_handle) =
        Actor::spawn(None, PeerHubActor, endpoint.node_id()).await?;

    let mut tasks = JoinSet::new();
    let new_peer_stream_subscriber =
        NewPeerStreamSubscriber::new(peer_stream, peer_hub_actor.clone(), endpoint.clone());
    tasks.spawn(new_peer_stream_subscriber.run());
    let incoming_connection_listener =
        IncomingConnectionListener::new(endpoint, peer_hub_actor.clone());
    tasks.spawn(incoming_connection_listener.run());

    while let Some(res) = tasks.join_next().await {
        if let Err(e) = res {
            println!("Task failed: {:?}", e);
        }
    }

    Ok(())
}
