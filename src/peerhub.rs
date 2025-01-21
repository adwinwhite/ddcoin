use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    fmt::Display,
    ops::{Deref, DerefMut},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use iroh::{NodeId, endpoint::SendStream};
use ractor::{Actor, RpcReplyPort};
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

use crate::{
    Block,
    block::{BlockId, SequenceNo},
    peer::{BroadcastMessage, PeerMessage},
    transaction::{Transaction, TransactionId},
};

// TODO: make this configurable.
const PEER_SIZE_LIMIT: usize = 3;

pub struct PeerHubActor;

struct BlockMap {
    map: HashMap<BlockId, Block>,
}

impl BlockMap {
    // FIXME: prevent malicious actor sending cycles and causing infinite loop.
    fn block_root(&self, block_id: &BlockId) -> BlockId {
        let mut current_id = *block_id;
        loop {
            if current_id.is_genesis() {
                return Block::GENESIS.id();
            }
            if let Some(block) = self.get(&current_id) {
                current_id = block.prev_id();
            } else {
                return *block_id;
            }
        }
    }

    fn is_dangling(&self, block_id: &BlockId) -> bool {
        !self.block_root(block_id).is_genesis()
    }
}

impl Deref for BlockMap {
    type Target = HashMap<BlockId, Block>;
    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

impl DerefMut for BlockMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.map
    }
}

impl From<HashMap<BlockId, Block>> for BlockMap {
    fn from(map: HashMap<BlockId, Block>) -> Self {
        Self { map }
    }
}

pub struct PeerHubActorState {
    peers: HashMap<NodeId, SendStream>,
    // FIXME: Do we still need this?
    annoucements: HashMap<TransactionId, NodeId>,
    mempool: HashMap<TransactionId, Transaction>,
    blocks: BlockMap,
    // TODO: Maybe use smallvec.
    next_blocks: HashMap<BlockId, HashSet<BlockId>>,
    dangling_blocks: HashSet<BlockId>,
    leading_block: BlockId,
    leading_block_sequence_no: SequenceNo,
    local_node_id: NodeId,
}

impl PeerHubActorState {
    pub fn new(local_node_id: NodeId) -> Self {
        let mut blocks = HashMap::new();
        blocks.insert(Block::GENESIS.id(), Block::GENESIS);
        Self {
            peers: HashMap::new(),
            annoucements: HashMap::new(),
            mempool: HashMap::new(),
            blocks: blocks.into(),
            next_blocks: HashMap::new(),
            dangling_blocks: HashSet::new(),
            leading_block: Block::GENESIS.id(),
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

    // TODO: wipe out blocks that based on invalid blocks.
    // TODO: remove duplicate check on chained blocks.
    fn update_leading_block(&mut self) -> bool {
        // Check through the dangling blocks to update leading block.
        let leading_block = self
            .dangling_blocks
            .extract_if(|id| !self.blocks.is_dangling(id))
            .map(|id| self.blocks.get(&id).unwrap())
            .max_by_key(|block| block.seqno());

        if let Some(new_leading_block) = leading_block {
            if new_leading_block.seqno() > self.leading_block_sequence_no {
                self.leading_block = new_leading_block.id();
                self.leading_block_sequence_no = new_leading_block.seqno();
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    fn update_next_blocks(&mut self, block: &Block) {
        let block_id = block.id();
        let block_prev_id = block.prev_id();
        match self.next_blocks.entry(block_prev_id) {
            Entry::Vacant(e) => {
                let mut set = HashSet::new();
                set.insert(block_id);
                e.insert(set);
            }
            Entry::Occupied(mut e) => {
                e.get_mut().insert(block_id);
            }
        }

        let next_blocks = &*self.next_blocks.entry(block_id).or_default();
        for n_id in next_blocks {
            if !self.dangling_blocks.contains(n_id) {
                continue;
            }
            // Check time.
            let next_block = self.blocks.get(n_id).unwrap();
            if next_block.timestamp() < block.timestamp() || block.seqno() + 1 != next_block.seqno()
            {
                warn!(
                    "Block {} has invalid timestamp or sha256 hash or seqno",
                    n_id
                );
                self.dangling_blocks.remove(n_id);
            }
        }
    }

    fn confirmed_chain(&self) -> Vec<&Block> {
        let mut block_id = self.leading_block;
        let mut blocks = Vec::new();
        loop {
            let block = self.blocks.get(&block_id).unwrap();
            let prev_block_id = block.prev_id();
            block_id = prev_block_id;
            blocks.push(block);
            if prev_block_id.is_genesis() {
                break;
            }
        }
        blocks
    }

    fn transaction_depth(chain: &Vec<&Block>, txn_id: &TransactionId) -> Option<u64> {
        let found = chain
            .iter()
            .enumerate()
            .find(|(_i, block)| block.transactions().iter().any(|txn| txn.id() == *txn_id));
        found.map(|(i, _)| (chain.len() - 1 - i) as u64)
    }
}
type Amount = u64;

// FIXME: optimize enum size here.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum PeerHubActorMessage {
    ShouldConnect(NodeId, RpcReplyPort<bool>),
    // actively connect or accept.
    NewPeer(NodeId, SendStream, bool, RpcReplyPort<bool>),
    AnnounceTransaction(NodeId, TransactionId),
    AnnounceBlock(NodeId, BlockId),
    GetTransaction(NodeId, TransactionId),
    GetBlock(NodeId, BlockId),
    NewTransaction(Transaction),
    // We receives a new block from node or self.
    NewBlock(Option<NodeId>, Block),
    QueryTransaction(TransactionId, RpcReplyPort<Option<Transaction>>),
    QueryTransactions(Vec<TransactionId>, RpcReplyPort<Vec<Option<Transaction>>>),
    // How many confirmed blocks after the one containing this transaction.
    QueryTransactionDepth(TransactionId, RpcReplyPort<Option<u64>>),
    QueryTransactionDepths(Vec<TransactionId>, RpcReplyPort<Vec<Option<u64>>>),
    QueryBlock(BlockId, RpcReplyPort<Option<Block>>),
    QueryPeers(RpcReplyPort<Vec<NodeId>>),
    QueryLocalNodeId(RpcReplyPort<NodeId>),
    QueryMemPool(RpcReplyPort<Vec<(TransactionId, Amount)>>),
    QueryLeadingBlock(RpcReplyPort<BlockId>),
}

impl Display for PeerHubActorMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeerHubActorMessage::ShouldConnect(node_id, _) => {
                write!(f, "ShouldConnect({})", node_id)
            }
            PeerHubActorMessage::NewPeer(node_id, _, _, _) => write!(f, "NewPeer({})", node_id),
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
            PeerHubActorMessage::NewBlock(node_id, block) => match node_id {
                Some(node_id) => write!(f, "NewBlock({}, {})", node_id, block),
                None => write!(f, "NewBlock(self, {})", block),
            },
            PeerHubActorMessage::QueryBlock(block_id, _rpc_reply_port) => {
                write!(f, "QueryBlock({})", block_id)
            }
            PeerHubActorMessage::QueryLocalNodeId(_) => {
                write!(f, "QueryLocalNodeId")
            }
            PeerHubActorMessage::QueryMemPool(_) => {
                write!(f, "QueryMemPool")
            }
            PeerHubActorMessage::QueryTransactions(ids, _reply) => {
                write!(f, "QueryTransactions([")?;
                for id in ids {
                    write!(f, "{}, ", id)?;
                }
                write!(f, "])")
            }
            PeerHubActorMessage::QueryLeadingBlock(_) => {
                write!(f, "QueryLeadingBlock")
            }
            PeerHubActorMessage::QueryTransactionDepth(txn_id, _reply) => {
                write!(f, "QueryTransactionDepth({})", txn_id)
            }
            PeerHubActorMessage::QueryTransactionDepths(ids, _) => {
                write!(f, "QueryTransactionDepths([")?;
                for id in ids {
                    write!(f, "{}, ", id)?;
                }
                write!(f, "])")
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
                    info!(
                        "Has reached max peers, should not allow connection from {:?}",
                        node_id
                    );
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
            PeerHubActorMessage::NewPeer(node_id, send_stream, connecting, reply) => {
                if state.peers.len() >= PEER_SIZE_LIMIT {
                    reply.send(false)?;
                    info!(
                        "Has reached max peers, rejecting connection from {:?}",
                        node_id
                    );
                    return Ok(());
                }
                match state.peers.entry(node_id) {
                    Entry::Vacant(e) => {
                        e.insert(send_stream);
                        reply.send(true)?;
                    }
                    Entry::Occupied(mut e) => {
                        // duplicate connection. smaller id should be actively connecting.
                        if !((state.local_node_id < node_id) ^ connecting) {
                            e.insert(send_stream);
                            reply.send(true)?;
                        } else {
                            reply.send(false)?;
                        }
                    }
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
                let current = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap(/*now is certainly later than epoch*/)
                    .as_nanos();
                if block.timestamp() > current {
                    warn!("Block {} has invalid timestamp", block.id());
                    return Ok(());
                }

                let block_id = block.id();
                let block_prev_id = block.prev_id();

                state.update_next_blocks(&block);

                // TODO: remove this clone by changing order of execution.
                state.blocks.insert(block.id(), block.clone());
                state.dangling_blocks.insert(block_id);
                // Follow valid block with highest seqno.
                let local_root = state.blocks.block_root(&block_prev_id);
                // This block can trace to genesis block thus is valid.
                if local_root.is_genesis() {
                    state.update_leading_block();
                    state.mempool.retain(|txn_id, _| {
                        !block
                            .transactions()
                            .iter()
                            .map(|t| t.id())
                            .any(|tid| &tid == txn_id)
                    });
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
                    state.send_to_peer(&peer_msg, node_id.unwrap()).await?;
                }
            }
            PeerHubActorMessage::QueryBlock(block_id, reply) => {
                let block = state.blocks.get(&block_id).cloned();
                reply.send(block)?;
            }
            PeerHubActorMessage::QueryLocalNodeId(reply) => {
                reply.send(state.local_node_id)?;
            }
            PeerHubActorMessage::QueryMemPool(reply) => {
                let amounts = state
                    .mempool
                    .iter()
                    .map(|(txn_id, txn)| (*txn_id, txn.amount()))
                    .collect::<Vec<_>>();
                reply.send(amounts)?;
            }
            PeerHubActorMessage::QueryTransactions(ids, reply) => {
                let txns = ids
                    .iter()
                    .map(|id| state.mempool.get(id).cloned())
                    .collect::<Vec<_>>();
                reply.send(txns)?;
            }
            PeerHubActorMessage::QueryLeadingBlock(reply) => {
                reply.send(state.leading_block)?;
            }
            PeerHubActorMessage::QueryTransactionDepth(txn_id, reply) => {
                let chain = state.confirmed_chain();
                let depth = PeerHubActorState::transaction_depth(&chain, &txn_id);
                reply.send(depth)?;
            }
            PeerHubActorMessage::QueryTransactionDepths(ids, reply) => {
                let chain = state.confirmed_chain();
                let depths = ids
                    .iter()
                    .map(|id| PeerHubActorState::transaction_depth(&chain, id))
                    .collect::<Vec<_>>();
                reply.send(depths)?;
            }
        }
        Ok(())
    }
}
