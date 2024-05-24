use clap::Parser;
use futures_util::StreamExt;
use jsonrpsee::{core::RpcResult, proc_macros::rpc};
use reth::{
    builder::NodeHandle, cli::Cli, dirs::{DataDirPath, MaybePlatformPath, PlatformPath}, primitives::BlockNumberOrTag, providers::{BlockNumReader, BlockReaderIdExt, CanonStateSubscriptions, ChainSpecProvider}, revm::primitives::FixedBytes, transaction_pool::TransactionPool
};
use reth_node_ethereum::node::EthereumNode;
use std::{collections::HashMap, path::Path, sync::Arc};
use tokio::sync::Mutex;

fn main() {
    Cli::<RethAnalCliExt>::parse()
        .run(|builder, args| async move {

            // launch the node
            let NodeHandle {
                node,
                node_exit_future,
            } = builder.node(EthereumNode::default()).launch().await?;

            // ChainInfo
            let chain_spec= node.provider.chain_spec();

            // Get sqlite3 db location
            let db_path = PlatformPath::<DataDirPath>::default().with_chain(chain_spec.chain).data_dir_path();
            let db_anal_sqlite3 = db_path.join(args.anal_db).clone();
            let db_anal_sqlite3 = db_anal_sqlite3.as_path();
            tracing::info!("DB Anal sqlite3 {:?}", db_anal_sqlite3);

            // create a new subscription to pending transactions and new canon state
            let mut pending_transactions = node.pool.new_pending_pool_transactions_listener();
            let mut canon_state_notifications = node.provider.subscribe_to_canonical_state();

            // Provider clone
            let provider = node.provider.clone();


            // Simple KV store to denote if transactions are seen in the mempool
            let seen_txs: Arc<Mutex<HashMap<FixedBytes<32>, bool>>> =
                Arc::new(Mutex::new(HashMap::new()));

            // Listens to mempool transactions and saves them to KV
            let seen_txs_mempool = seen_txs.clone();
            node.task_executor.spawn(Box::pin(async move {
                // Waiting for new transactions
                while let Some(event) = pending_transactions.next().await {
                    let tx = event.transaction;
                    let tx_hash = tx.hash();

                    // Stores the transaction hash into the KV store
                    let mut guard = seen_txs_mempool.lock().await;
                    guard.insert(*tx_hash, true);
                }
            }));

            // On new canon state, just update everything (dont care if its a reorg or commit)
            let seen_txs_canon = seen_txs.clone();
            node.task_executor.spawn(Box::pin(async move {
                while let Ok(_) = canon_state_notifications.recv().await {
                    if let Ok(Some(latest_block)) = provider
                        .clone()
                        .block_by_number_or_tag(BlockNumberOrTag::Latest)
                    {
                        // Compares with the list of transactions we've seen
                        let body = latest_block.body;

                        let mut public_txs: Vec<FixedBytes<32>> = Vec::new();
                        let mut private_txs: Vec<FixedBytes<32>> = Vec::new();

                        // Unblock guard and then saves the public txs
                        let guard = seen_txs_canon.lock().await;
                        for tx in body.iter() {
                            let cur_hash = tx.hash();
                            if guard.contains_key(&cur_hash) {
                                public_txs.push(cur_hash);
                            } else {
                                private_txs.push(cur_hash);
                            }
                        }

                        // Save to sqlite3
                    }
                }
            }));

            tracing::info!("Reth Analyzer enabled");

            node_exit_future.await
        })
        .unwrap();
}

#[derive(Debug, Clone, Default, clap::Args)]
struct RethAnalCliExt {
    /// Analytics database name
    #[arg(long, default_value = "reth-anal.sqlite3")]
    pub anal_db: String,
}

// Trait for the new namespace + method
#[cfg_attr(not(test), rpc(server, namespace = "anal"))]
#[cfg_attr(test, rpc(server, namespace = "anal"))]
pub trait RethAnalExtApi {
    /// Returns the number of transactions in the pool.
    #[method(name = "blockTxConfidentiality")]
    fn block_tx_confidentiality(&self) -> RpcResult<usize>;
}

/// The type that implements the `txpool` rpc namespace trait
pub struct RethAnalExt {}

impl RethAnalExtApiServer for RethAnalExt {
    fn block_tx_confidentiality(&self) -> RpcResult<usize> {
        return Ok(0);
    }
}
