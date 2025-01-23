use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    fmt::Display,
    ops::{Deref, DerefMut},
};

use anyhow::{Context, Result};
use iroh::{NodeId, endpoint::SendStream};
use ractor::{Actor, RpcReplyPort};
use tokio::io::AsyncWriteExt;
use tracing::{error, info, warn};

use crate::{
    Block, Config,
    block::{BlockId, SequenceNo},
    peer::{BroadcastMessage, PeerMessage},
    transaction::{Transaction, TransactionId},
    util::{Timestamp, TimestampExt},
};

// TODO: make this configurable.
const PEER_SIZE_LIMIT: usize = 3;

pub struct PeerHubActor;

// TODO: eliminate this wrapper since we can define associated functions in State.
struct BlockMap {
    map: HashMap<BlockId, Block>,
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

pub(crate) struct BlockChain<'a> {
    chain: Box<[&'a Block]>,
}

impl<'a> BlockChain<'a> {
    pub fn new<'b>(chain: &'b [&'a Block]) -> BlockChain<'a> {
        BlockChain {
            chain: chain.into(),
        }
    }

    pub fn transaction_depth(&self, txn_id: &TransactionId) -> Option<u64> {
        let found = self
            .chain
            .iter()
            .enumerate()
            .find(|(_i, block)| block.transactions().iter().any(|txn| txn.id() == *txn_id));
        found.map(|(i, _)| (self.chain.len() - 1 - i) as u64)
    }
}

impl<'a> Deref for BlockChain<'a> {
    type Target = [&'a Block];
    fn deref(&self) -> &Self::Target {
        &self.chain
    }
}

pub struct PeerHubActorState {
    config: Config,
    genesis_id: BlockId,
    peers: HashMap<NodeId, SendStream>,
    // FIXME: Do we still need this?
    annoucements: HashMap<TransactionId, NodeId>,
    mempool: HashMap<TransactionId, Transaction>,
    valid_blocks: BlockMap,
    unconfirmed_blocks: BlockMap,
    invalid_blocks: HashSet<BlockId>,
    // TODO: Maybe use smallvec.
    next_blocks: HashMap<BlockId, HashSet<BlockId>>,
    leading_block: BlockId,
    local_node_id: NodeId,
}

impl PeerHubActorState {
    pub fn new(local_node_id: NodeId, config: Config) -> Self {
        let mut blocks = HashMap::new();
        let genesis_block =
            Block::create_genesis(*config.genesis_difficulty(), config.genesis_timestamp());
        let genesis_id = genesis_block.id();
        blocks.insert(genesis_id, genesis_block);
        Self {
            config,
            genesis_id,
            peers: HashMap::new(),
            annoucements: HashMap::new(),
            mempool: HashMap::new(),
            valid_blocks: blocks.into(),
            unconfirmed_blocks: HashMap::new().into(),
            invalid_blocks: HashSet::new(),
            next_blocks: HashMap::new(),
            leading_block: genesis_id,
            local_node_id,
        }
    }

    // FIXME: prevent malicious actor sending cycles and causing infinite loop.
    fn block_root(&self, block_id: &BlockId) -> BlockId {
        let mut current_id = *block_id;
        loop {
            if current_id == self.genesis_id {
                return self.genesis_id;
            }
            if let Some(block) = self.unconfirmed_blocks.get(&current_id) {
                current_id = block.prev_id();
            } else {
                return *block_id;
            }
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

    fn leading_block_seqno(&self) -> SequenceNo {
        self.valid_blocks.get(&self.leading_block).unwrap().seqno()
    }

    fn knows_block(&self, block_id: &BlockId) -> bool {
        self.valid_blocks.contains_key(block_id)
            || self.unconfirmed_blocks.contains_key(block_id)
            || self.invalid_blocks.contains(block_id)
    }

    // TODO: rewrite using safe code.
    fn confirmed_chain<'a>(&'a self) -> BlockChain<'a> {
        let mut block_id = self.leading_block;
        let mut blocks =
            Box::<[&'a Block]>::new_uninit_slice((self.leading_block_seqno() + 1) as usize);
        loop {
            let block = self.valid_blocks.get(&block_id).unwrap();
            let prev_block_id = block.prev_id();
            // # Safety
            // We are sure that leading block can trace back to genesis block.
            unsafe {
                blocks[block.seqno() as usize].as_mut_ptr().write(block);
            };

            if block_id == self.genesis_id {
                break;
            }
            block_id = prev_block_id;
        }
        // # Safety
        // Combined with above.
        let blocks = unsafe { blocks.assume_init() };
        BlockChain::new(&blocks)
    }

    fn prev_difficulty_adjusted_block(&self, seqno: SequenceNo) -> &Block {
        let chain = self.confirmed_chain();
        // FIXME: consider chain that's too long.
        let rewind_number = {
            let rewind = seqno % self.config.difficulty_adjustment_period();
            if rewind == 0 {
                self.config.difficulty_adjustment_period()
            } else {
                rewind
            }
        };
        chain[(seqno - rewind_number) as usize]
    }

    fn add_invalid_block(
        invalid_blocks: &mut HashSet<BlockId>,
        next_blocks: &mut HashMap<BlockId, HashSet<BlockId>>,
        block_id: BlockId,
    ) {
        invalid_blocks.insert(block_id);

        // Remove all blocks that are based on this block.
        for next_block_id in next_blocks.remove(&block_id).unwrap_or_default() {
            Self::add_invalid_block(invalid_blocks, next_blocks, next_block_id);
        }
    }

    // Return the ids of new valid blocks.
    fn add_valid_block(&mut self, block: Block) -> Vec<BlockId> {
        // Remove collected transactions in mempool.
        self.mempool.retain(|txn_id, _| {
            !block
                .transactions()
                .iter()
                .map(|t| t.id())
                .any(|tid| &tid == txn_id)
        });

        let seqno = block.seqno();
        let block_id = block.id();
        self.valid_blocks.insert(block_id, block);

        // Check whether we can be the leader.
        if seqno > self.leading_block_seqno() {
            self.leading_block = block_id;
        }

        // Collect valid block ids.
        let mut valid_ids = vec![block_id];

        // Try to validate next blocks.
        for next_block_id in self.next_blocks.remove(&block_id).unwrap_or_default() {
            if let Some(next_block) = self.unconfirmed_blocks.remove(&next_block_id) {
                let ids = self.validate_block(next_block);
                valid_ids.extend(ids);
            }
        }
        valid_ids
    }

    // Preconditions:
    //  block is not in unconfirmed block.
    //  block is not in invalid pool.
    fn validate_block(&mut self, block: Block) -> Vec<BlockId> {
        // Try to validate the block.
        // If success, try validate related unconfirmed blocks.
        // If failure, throw it into invalid pool.
        // If ambiguous, throw it into unconfirmed pool.

        // Check previous block.
        if let Some(prev_block) = self.valid_blocks.get(&block.prev_id()) {
            // Check previous timestamp.
            if block.timestamp() < prev_block.timestamp() {
                warn!(
                    "Block {} has invalid timestamp: ealier than the previous block",
                    block.id()
                );
                Self::add_invalid_block(
                    &mut self.invalid_blocks,
                    &mut self.next_blocks,
                    block.id(),
                );
                return Vec::new();
            }

            // Check previous seqno.
            if block.seqno() != prev_block.seqno() + 1 {
                warn!("Block {} has invalid seqno", block.id());
                Self::add_invalid_block(
                    &mut self.invalid_blocks,
                    &mut self.next_blocks,
                    block.id(),
                );
                return Vec::new();
            }

            // Check difficulty.
            let expected_difficulty =
                if block.seqno() % self.config.difficulty_adjustment_period() == 0 {
                    let prev_adjustment_time = self
                        .prev_difficulty_adjusted_block(block.seqno())
                        .timestamp();
                    let time_span_actual = block.timestamp() - prev_adjustment_time;
                    prev_block
                        .difficulty()
                        .adjust_with_actual_span(time_span_actual, &self.config)
                } else {
                    prev_block.difficulty()
                };
            if block.difficulty() != expected_difficulty {
                warn!(
                    "Block {} has invalid difficulty, expected {}",
                    block.id(),
                    expected_difficulty
                );
                Self::add_invalid_block(
                    &mut self.invalid_blocks,
                    &mut self.next_blocks,
                    block.id(),
                );
                return Vec::new();
            }

            self.add_valid_block(block)
        } else {
            // Ambiguous, the previous block is missing.
            self.next_blocks
                .entry(block.prev_id())
                .or_default()
                .insert(block.id());
            self.unconfirmed_blocks.insert(block.id(), block);
            Vec::new()
        }
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
    QueryDifficultyAdjustTime(SequenceNo, RpcReplyPort<Timestamp>),
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
            PeerHubActorMessage::QueryDifficultyAdjustTime(seqno, _rpc_reply_port) => {
                write!(f, "QueryDifficultyAdjustTime({})", seqno)
            }
        }
    }
}

impl Actor for PeerHubActor {
    type Msg = PeerHubActorMessage;
    type State = PeerHubActorState;
    type Arguments = (NodeId, Config);

    async fn pre_start(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> std::result::Result<Self::State, ractor::ActorProcessingErr> {
        Ok(PeerHubActorState::new(args.0, args.1))
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
                if state.knows_block(&block_id) {
                    info!("We already knew block of id {}", block_id);
                    return Ok(());
                }

                // Get Block.
                let peer_msg = PeerMessage::GetBlockRequest(block_id);
                state.send_to_peer(&peer_msg, node_id).await?;
            }
            PeerHubActorMessage::GetBlock(node_id, block_id) => {
                // We only expose valid blocks.
                let block = state
                    .valid_blocks
                    .get(&block_id)
                    .cloned()
                    .context("No such block")?;
                let peer_msg = PeerMessage::GetBlockResponse(state.local_node_id, block.clone());
                state.send_to_peer(&peer_msg, node_id).await?;
            }
            PeerHubActorMessage::NewBlock(node_id, block) => {
                // Check whether it's invalid already.
                if state.invalid_blocks.contains(&block.id()) {
                    warn!("Block {} is known as invalid", block.id());
                    return Ok(());
                }

                // Check whether it's repeated block.
                if state.knows_block(&block.id()) {
                    info!("Block {} is already known", block.id());
                    return Ok(());
                }

                // Check time
                let current = Timestamp::now();
                if block.timestamp() > current {
                    warn!(
                        "Block {} has invalid timestamp: later than current",
                        block.id()
                    );
                    PeerHubActorState::add_invalid_block(
                        &mut state.invalid_blocks,
                        &mut state.next_blocks,
                        block.id(),
                    );
                    return Ok(());
                }

                let block_id = block.id();
                let block_prev_id = block.prev_id();
                let valid_ids = state.validate_block(block);

                if valid_ids.is_empty() {
                    // Check whether it's a dangling block.
                    if state.unconfirmed_blocks.contains_key(&block_id) {
                        // Request the missing block.
                        let missing_root = state.block_root(&block_prev_id);
                        debug_assert_ne!(missing_root, state.genesis_id);
                        let peer_msg = PeerMessage::GetBlockRequest(missing_root);
                        if let Some(node_id) = node_id {
                            state.send_to_peer(&peer_msg, node_id).await?;
                        } else {
                            // block mined by myself shouldn't have missing blocks.
                            error!("Block {} is missing its previous block", block_id);
                        }
                    }
                } else {
                    for id in valid_ids {
                        state
                            .broadcast(state.local_node_id, BroadcastMessage::AnnounceBlock(id))
                            .await?;
                    }
                }
            }
            PeerHubActorMessage::QueryBlock(block_id, reply) => {
                let block = state.valid_blocks.get(&block_id).cloned();
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
                let depth = chain.transaction_depth(&txn_id);
                reply.send(depth)?;
            }
            PeerHubActorMessage::QueryTransactionDepths(ids, reply) => {
                let chain = state.confirmed_chain();
                let depths = ids
                    .iter()
                    .map(|id| chain.transaction_depth(id))
                    .collect::<Vec<_>>();
                reply.send(depths)?;
            }
            PeerHubActorMessage::QueryDifficultyAdjustTime(seqno, reply) => {
                let adjustment_block = state.prev_difficulty_adjusted_block(seqno);
                reply.send(adjustment_block.timestamp())?;
            }
        }
        Ok(())
    }
}
