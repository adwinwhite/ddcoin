use std::{
    collections::{HashMap, hash_map::Entry},
    fmt::Display,
};

use anyhow::{Context, Result};
use iroh::{NodeId, endpoint::SendStream};
use ractor::{Actor, RpcReplyPort};
use tokio::io::AsyncWriteExt;
use tracing::info;

use crate::{
    Block,
    block::{BlockId, SequenceNo},
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
    blocks: HashMap<BlockId, Block>,
    leading_block: BlockId,
    leading_block_sequence_no: SequenceNo,
    local_node_id: NodeId,
}

impl PeerHubActorState {
    pub fn new(local_node_id: NodeId) -> Self {
        let mut blocks = HashMap::new();
        blocks.insert(BlockId::GENESIS, Block::GENESIS);
        Self {
            peers: HashMap::new(),
            annoucements: HashMap::new(),
            mempool: HashMap::new(),
            blocks,
            leading_block: BlockId::GENESIS,
            leading_block_sequence_no: 0,
            local_node_id,
        }
    }

    async fn send_to_peer(&mut self, msg: &PeerMessage, node_id: NodeId) -> Result<()> {
        let send_stream = self
            .peers
            .get_mut(&node_id)
            .context("No such peer for this id")?;
        let encoded = crate::serdes::transport::encode(msg)?;
        let len = encoded.len() as u32;
        send_stream.write_all(&len.to_be_bytes()).await?;
        send_stream.write_all(&encoded).await?;
        send_stream.flush().await?;
        Ok(())
    }

    async fn broadcast(&mut self, node_id: NodeId, msg: BroadcastMessage) -> Result<()> {
        let peer_msg = PeerMessage::Broadcast(msg);
        let encoded = crate::serdes::transport::encode(&peer_msg)?;
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

    // FIXME: prevent malicious actor sending cycles and causing infinite loop.
    fn block_root(&self, block_id: &BlockId) -> BlockId {
        let mut current_id = *block_id;
        loop {
            if current_id == BlockId::GENESIS {
                return BlockId::GENESIS;
            }
            if let Some(block) = self.blocks.get(&current_id) {
                current_id = block.prev_id();
            } else {
                return *block_id;
            }
        }
    }
}

// FIXME: optimize enum size here.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum PeerHubActorMessage {
    ShouldConnect(NodeId, RpcReplyPort<bool>),
    NewPeer(NodeId, SendStream, RpcReplyPort<bool>),
    AnnounceTransaction(NodeId, TransactionId),
    AnnounceBlock(NodeId, BlockId),
    GetTransaction(NodeId, TransactionId),
    GetBlock(NodeId, BlockId),
    NewTransaction(Transaction),
    // We receives a new block from node.
    NewBlock(NodeId, Block),
    QueryTransaction(TransactionId, RpcReplyPort<Option<Transaction>>),
    QueryBlock(BlockId, RpcReplyPort<Option<Block>>),
    QueryPeers(RpcReplyPort<Vec<NodeId>>),
    QueryLocalNodeId(RpcReplyPort<NodeId>),
}

impl Display for PeerHubActorMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeerHubActorMessage::ShouldConnect(node_id, _) => {
                write!(f, "ShouldConnect({})", node_id)
            }
            PeerHubActorMessage::NewPeer(node_id, _, _) => write!(f, "NewPeer({})", node_id),
            PeerHubActorMessage::AnnounceTransaction(node_id, txn_id) => {
                write!(f, "AnnounceTransaction({}, {})", node_id, txn_id)
            }
            PeerHubActorMessage::GetTransaction(node_id, txn_id) => {
                write!(f, "GetTransaction({}, {})", node_id, txn_id)
            }
            PeerHubActorMessage::NewTransaction(txn) => write!(f, "NewTransaction({})", txn),
            PeerHubActorMessage::QueryTransaction(txn_id, _rpc_reply_port) => {
                write!(f, "QueryTransaction({})", txn_id)
            }
            PeerHubActorMessage::QueryPeers(_reply) => {
                write!(f, "QueryPeers")
            }
            PeerHubActorMessage::AnnounceBlock(node_id, block_id) => {
                write!(f, "AnnounceBlock({}, {})", node_id, block_id)
            }
            PeerHubActorMessage::GetBlock(node_id, block_id) => {
                write!(f, "GetBlock({}, {})", node_id, block_id)
            }
            PeerHubActorMessage::NewBlock(node_id, block) => {
                write!(f, "NewBlock({}, {})", node_id, block)
            }
            PeerHubActorMessage::QueryBlock(block_id, _rpc_reply_port) => {
                write!(f, "QueryBlock({})", block_id)
            }
            PeerHubActorMessage::QueryLocalNodeId(_) => {
                write!(f, "QueryLocalNodeId")
            }
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
                if state.peers.len() >= PEER_SIZE_LIMIT {
                    reply_port.send(false)?;
                    info!("Rejecting connection from {:?}", node_id);
                } else {
                    let contains = state.peers.contains_key(&node_id);
                    let should_connect = !contains;
                    reply_port.send(should_connect)?;
                    info!(
                        "{}: we should allow connection with {:?}",
                        should_connect, node_id
                    );
                }
            }
            PeerHubActorMessage::AnnounceTransaction(node_id, txn_id) => {
                if state.knows_transaction(&txn_id) {
                    info!("Transaction {} already in mempool or annoucements", txn_id);
                } else {
                    state.annoucements.insert(txn_id, node_id);
                    // Get Transaction.
                    let peer_msg = PeerMessage::GetTransactionRequest(txn_id);
                    state.send_to_peer(&peer_msg, node_id).await?;
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
            PeerHubActorMessage::QueryTransaction(txn_id, reply) => {
                let txn = state.mempool.get(&txn_id).cloned();
                reply.send(txn)?;
            }
            PeerHubActorMessage::QueryPeers(reply) => {
                reply.send(state.peers.keys().cloned().collect::<Vec<_>>())?;
            }
            PeerHubActorMessage::AnnounceBlock(node_id, block_id) => {
                if state.blocks.contains_key(&block_id) {
                    info!("We already have block of id {}", block_id);
                    return Ok(());
                }

                // Get Block.
                let peer_msg = PeerMessage::GetBlockRequest(block_id);
                state.send_to_peer(&peer_msg, node_id).await?;
            }
            PeerHubActorMessage::GetBlock(node_id, block_id) => {
                let block = state
                    .blocks
                    .get(&block_id)
                    .cloned()
                    .context("No such block")?;
                let peer_msg = PeerMessage::GetBlockResponse(state.local_node_id, block.clone());
                state.send_to_peer(&peer_msg, node_id).await?;
            }
            PeerHubActorMessage::NewBlock(node_id, block) => {
                if block.seqno() <= state.leading_block_sequence_no {
                    info!("We already have block of sequence no {}", block.seqno());
                    return Ok(());
                }

                let block_id = block.id();
                let block_prev_id = block.prev_id();
                let seqno = block.seqno();
                state.blocks.insert(block.id(), block);
                // Follow valid block with highest seqno.
                let local_root = state.block_root(&block_prev_id);
                if local_root == BlockId::GENESIS {
                    state.leading_block = block_id;
                    state.leading_block_sequence_no = seqno;
                    state
                        .broadcast(
                            state.local_node_id,
                            BroadcastMessage::AnnounceBlock(block_id),
                        )
                        .await?;
                } else {
                    // Get missing block.
                    // If the miner is ourself, we shouldn't have missing blocks thus won't execute
                    // below.
                    let peer_msg = PeerMessage::GetBlockRequest(local_root);
                    state.send_to_peer(&peer_msg, node_id).await?;
                }
            }
            PeerHubActorMessage::QueryBlock(block_id, reply) => {
                let block = state.blocks.get(&block_id).cloned();
                reply.send(block)?;
            }
            PeerHubActorMessage::QueryLocalNodeId(reply) => {
                reply.send(state.local_node_id)?;
            }
        }
        Ok(())
    }
}
