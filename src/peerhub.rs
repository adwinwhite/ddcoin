use std::{
    collections::{hash_map::Entry, HashMap},
    fmt::Display,
};

use anyhow::{Context, Result};
use iroh::{endpoint::SendStream, NodeId};
use ractor::{Actor, RpcReplyPort};
use tokio::io::AsyncWriteExt;
use tracing::info;

use crate::{
    peer::{BroadcastMessage, PeerMessage},
    transaction::{Transaction, TransactionId},
};

// TODO: make this configurable.
const PEER_SIZE_LIMIT: usize = 3;

pub struct PeerHubActor;

pub struct PeerHubActorState {
    peers: HashMap<NodeId, SendStream>,
    annoucements: HashMap<TransactionId, NodeId>,
    mempool: HashMap<TransactionId, Transaction>,
    local_node_id: NodeId,
}

impl PeerHubActorState {
    pub fn new(local_node_id: NodeId) -> Self {
        Self {
            peers: HashMap::new(),
            mempool: HashMap::new(),
            local_node_id,
            annoucements: HashMap::new(),
        }
    }

    async fn send_to_peer(&mut self, msg: &PeerMessage, node_id: NodeId) -> Result<()> {
        let send_stream = self
            .peers
            .get_mut(&node_id)
            .context("No such peer for this id")?;
        let encoded = crate::serdes::encode(msg)?;
        let len = encoded.len() as u32;
        send_stream.write_all(&len.to_be_bytes()).await?;
        send_stream.write_all(&encoded).await?;
        send_stream.flush().await?;
        Ok(())
    }

    async fn broadcast(&mut self, node_id: NodeId, msg: BroadcastMessage) -> Result<()> {
        let peer_msg = PeerMessage::Broadcast(msg);
        let encoded = crate::serdes::encode(&peer_msg)?;
        let len = encoded.len() as u32;
        for (id, send_stream) in &mut self.peers {
            if *id == node_id {
                continue;
            }
            send_stream.write_all(&len.to_be_bytes()).await?;
            send_stream.write_all(&encoded).await?;
            send_stream.flush().await?;
        }
        Ok(())
    }

    fn knows_transaction(&self, txn_id: &TransactionId) -> bool {
        self.mempool.contains_key(txn_id) || self.annoucements.contains_key(txn_id)
    }
}

// FIXME: optimize enum size here.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum PeerHubActorMessage {
    ShouldConnect(NodeId, RpcReplyPort<bool>),
    NewPeer(NodeId, SendStream, RpcReplyPort<bool>),
    Broadcast(NodeId, BroadcastMessage),
    GetTransaction(NodeId, TransactionId),
    NewTransaction(Transaction),
}

impl Display for PeerHubActorMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeerHubActorMessage::ShouldConnect(node_id, _) => {
                write!(f, "ShouldConnect({})", node_id)
            }
            PeerHubActorMessage::NewPeer(node_id, _, _) => write!(f, "NewPeer({})", node_id),
            PeerHubActorMessage::Broadcast(node_id, broadcast_msg) => {
                write!(f, "Broadcast({}, {})", node_id, broadcast_msg)
            }
            PeerHubActorMessage::GetTransaction(node_id, txn_id) => {
                write!(f, "GetTransaction({}, {})", node_id, txn_id)
            }
            PeerHubActorMessage::NewTransaction(txn) => write!(f, "NewTransaction({})", txn),
        }
    }
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
        _myself: ractor::ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> std::result::Result<(), ractor::ActorProcessingErr> {
        info!("PeerHubActor received: {}", message);
        match message {
            PeerHubActorMessage::ShouldConnect(node_id, reply_port) => {
                info!("PeerHubActor received: ShouldConnect {:?}", node_id);
                if state.peers.len() >= PEER_SIZE_LIMIT {
                    reply_port.send(false)?;
                    info!("Rejecting connection from {:?}", node_id);
                } else {
                    let contains = state.peers.contains_key(&node_id);
                    let should_connect = !contains;
                    reply_port.send(should_connect)?;
                    info!("{} accepting connection from {:?}", should_connect, node_id);
                }
            }
            PeerHubActorMessage::Broadcast(node_id, broadcast_msg) => {
                // TODO: refactor broadcast part. broadcast doesn't happen here right now.
                match broadcast_msg {
                    BroadcastMessage::AnnounceTransaction(txn_id) => {
                        if state.knows_transaction(&txn_id) {
                            info!("Transaction {} already in mempool or annoucements", txn_id);
                        } else {
                            state.annoucements.insert(txn_id, node_id);
                            // Get Transaction.
                            let peer_msg = PeerMessage::GetTransactionRequest(txn_id);
                            state.send_to_peer(&peer_msg, node_id).await?;
                        }
                    }
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
            PeerHubActorMessage::NewTransaction(txn) => {
                // Now we do the new transaction. It may come from user or peers.
                let txn_id = txn.id();
                if state.mempool.contains_key(&txn_id) {
                    info!("Transaction {} already in mempool", txn_id);
                } else {
                    state.mempool.insert(txn.id(), txn);
                    // We broadcast when we do have the data.
                    state
                        .broadcast(
                            state.local_node_id,
                            BroadcastMessage::AnnounceTransaction(txn_id),
                        )
                        .await?;
                    state.annoucements.remove(&txn_id);
                }
            }
            PeerHubActorMessage::GetTransaction(node_id, txn_id) => {
                let txn = state
                    .mempool
                    .get(&txn_id)
                    .context("No such transaction in mempool")?;
                let peer_msg = PeerMessage::GetTransactionResponse(txn.clone());
                state.send_to_peer(&peer_msg, node_id).await?;
            }
        }
        Ok(())
    }
}
