use clap::Parser;
use jsonrpsee::{
    core::RpcResult,
    proc_macros::rpc,
    types::{error::UNKNOWN_ERROR_CODE, ErrorObjectOwned},
};
use reth::{
    builder::NodeHandle,
    cli::Cli,
    primitives::{hex::ToHexExt, BlockNumber, BlockNumberOrTag, SealedBlock},
    providers::{
        BlockNumReader, BlockReaderIdExt, CanonStateNotification, CanonStateSubscriptions,
    },
    revm::primitives::FixedBytes,
    transaction_pool::TransactionPool,
};
use reth_node_ethereum::node::EthereumNode;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct BlockPrivy {
    pub number: BlockNumber,
    pub public_txs: Vec<String>,
    pub private_txs: Vec<String>,
}

fn main() {
    Cli::<RethAnalCliExt>::parse()
        .run(|builder, args| async move {
            // Get sqlite3 db location
            let db_path = builder.data_dir().data_dir_path();
            let db_anal_sqlite3 = db_path.join(args.anal_db).clone();
            let db_anal_sqlite3 = db_anal_sqlite3.as_path();
            let sqlite_conn = Connection::open(db_anal_sqlite3).unwrap();
            sqlite_conn.execute(
                "CREATE TABLE IF NOT EXISTS tx_privy (
                    number INTEGER PRIMARY KEY,
                    public_txs TEXT,
                    private_txs TEXT
                )",
                [],
            )?;

            // launch the node
            let sqlite_conn_arc = Arc::new(Mutex::new(sqlite_conn));
            let sqlite_conn_ext = sqlite_conn_arc.clone();
            let NodeHandle {
                node,
                node_exit_future,
            } = builder
                .node(EthereumNode::default())
                .extend_rpc_modules(move |ctx| {
                    let ext = RethAnalExt {
                        provider: ctx.provider().clone(),
                        sqlite_conn: sqlite_conn_ext.clone(),
                    };
                    ctx.modules.merge_configured(ext.into_rpc())?;
                    Ok(())
                })
                .launch()
                .await?;

            // create a new subscription to transactions and new canon state
            let mut tx_listener= node.pool.new_transactions_listener();
            let mut canon_state_listener = node.provider.subscribe_to_canonical_state();

            // Provider clone
            let txpool = node.pool.clone();

            // Simple KV store to denote if transactions are seen in the mempool
            // Not querying from mempool as once the block is updated, it'll be removed
            // from the pending mempool
            let seen_txs: Arc<Mutex<HashMap<FixedBytes<32>, bool>>> =
                Arc::new(Mutex::new(HashMap::new()));

            // Listens to mempool transactions and saves them to KV
            let seen_txs_mempool = seen_txs.clone();
            node.task_executor.spawn(Box::pin(async move {
                // Waiting for new transactions
                while let Some(event) = tx_listener.recv().await {
                    let tx = event.transaction;
                    let tx_hash = tx.hash();

                    // Stores the transaction hash into the KV store
                    let mut guard = seen_txs_mempool.lock().await;
                    guard.insert(*tx_hash, true);
                    drop(guard);

                    // Remove the transaction from the txpool so we dun max it out lol
                    txpool.remove_transactions(vec![*tx_hash]);
                }
            }));

            // On new canon state, just update everything (dont care if its a reorg or commit)
            let seen_txs_canon = seen_txs.clone();
            let sqlite_conn_inserter = sqlite_conn_arc.clone();
            node.task_executor.spawn(Box::pin(async move {
                while let Ok(e) = canon_state_listener.recv().await {
                    let mut blocks: Vec<SealedBlock> = Vec::new();

                    match e {
                        CanonStateNotification::Commit { new } =>{
                            for (_, v) in new.blocks().into_iter() {
                                blocks.push(v.block.clone());
                            }
                        },
                        CanonStateNotification::Reorg { old: _, new } => {
                            for (_, v) in new.blocks().into_iter() {
                                blocks.push(v.block.clone());
                            }
                        },
                    };

                    for block in blocks {
                        let block_number = block.number;
                        let body = block.body;

                        let mut public_txs: Vec<String> = Vec::new();
                        let mut private_txs: Vec<String> = Vec::new();

                        // Unblock guard and then saves the public txs
                        let mut guard = seen_txs_canon.lock().await;
                        for tx in body.iter() {
                            let cur_hash = tx.hash();
                            if guard.contains_key(&cur_hash) {
                                // Remove txhash (no memory leak)
                                guard.remove(&cur_hash);

                                public_txs.push(cur_hash.encode_hex_with_prefix());
                            } else {
                                private_txs.push(cur_hash.encode_hex_with_prefix());
                            }
                        }
                        drop(guard);

                        // Save to sqlite3
                        let guard = sqlite_conn_inserter.lock().await;
                        // Delete block if it already exists
                        let _ = guard.execute("DELETE FROM tx_privy WHERE number = ?", params![block_number]);

                        // Insert new block into sqlite3
                        let public_txs = public_txs.join(",");
                        let private_txs = private_txs.join(",");

                        let _ = guard.execute(
                        "INSERT INTO tx_privy (number, public_txs, private_txs) VALUES (?1, ?2, ?3)",
                            params![block_number, public_txs, private_txs],
                        );
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
#[cfg_attr(test, rpc(server, client, namespace = "anal"))]
pub trait RethAnalExtApi {
    #[method(name = "getBlockTxPrivyByNumber")]
    async fn get_block_tx_privy_by_number(&self, bn: BlockNumberOrTag) -> RpcResult<BlockPrivy>;
}

pub struct RethAnalExt<Provider> {
    pub provider: Provider,
    pub sqlite_conn: Arc<Mutex<Connection>>,
}

#[async_trait::async_trait]
impl<Provider> RethAnalExtApiServer for RethAnalExt<Provider>
where
    Provider: BlockReaderIdExt + Clone + Unpin + 'static,
{
    async fn get_block_tx_privy_by_number(&self, bn: BlockNumberOrTag) -> RpcResult<BlockPrivy> {
        let conn = self.sqlite_conn.lock().await;

        let mut stmt = conn
            .prepare("SELECT number, public_txs, private_txs FROM tx_privy WHERE number = ?")
            .unwrap();

        let bn = match bn.is_number() {
            true => bn.as_number().unwrap(),
            false => match self.provider.last_block_number() {
                Ok(n) => n,
                Err(e) => {
                    return Err(ErrorObjectOwned::owned(
                        UNKNOWN_ERROR_CODE,
                        e.to_string(),
                        None::<()>,
                    ));
                }
            },
        };

        let mut privy_iter = stmt
            .query_map([bn], |row| {
                let number: u64 = row.get(0)?;

                let public_txs: String = row.get(1)?;
                let public_txs: Vec<&str> = public_txs.split(",").collect();
                let public_txs = public_txs.into_iter().map(|x| x.to_string()).collect();

                let private_txs: String = row.get(2)?;
                let private_txs: Vec<&str> = private_txs.split(",").collect();
                let private_txs = private_txs.into_iter().map(|x| x.to_string()).collect();

                Ok(BlockPrivy {
                    number,
                    public_txs,
                    private_txs,
                })
            })
            .map_err(|x| ErrorObjectOwned::owned(UNKNOWN_ERROR_CODE, x.to_string(), None::<()>))?;

        if let Some(Ok(result)) = privy_iter.next() {
            return Ok(result);
        }

        return Err(ErrorObjectOwned::owned(
            UNKNOWN_ERROR_CODE,
            "block not indexed",
            None::<()>,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_sql() {
        let db = Connection::open_in_memory().unwrap();

        let _ = db
            .execute(
                "CREATE TABLE IF NOT EXISTS tx_privy (
                number INTEGER PRIMARY KEY,
                public_txs TEXT,
                private_txs TEXT
            )",
                [],
            )
            .unwrap();

        let _ = db
            .execute(
                "INSERT INTO tx_privy (number, public_txs, private_txs) VALUES (?1, ?2, ?3)",
                params![1, "0x1,0x2,0x3", "0x5,0x6,0x7"],
            )
            .unwrap();

        let _ = db
            .execute("DELETE FROM tx_privy WHERE number = ?", params![1])
            .unwrap();

        let _ = db
            .execute(
                "INSERT INTO tx_privy (number, public_txs, private_txs) VALUES (?1, ?2, ?3)",
                params![1, "0x1,0x2,0x3", "0x5,0x6,0x7"],
            )
            .unwrap();

        let mut stmt = db
            .prepare("SELECT number, public_txs, private_txs FROM tx_privy WHERE number = ?")
            .unwrap();

        let mut privy_iter = stmt
            .query_map([1], |row| {
                let number: u64 = row.get(0)?;

                let public_txs: String = row.get(1)?;
                let public_txs: Vec<&str> = public_txs.split(",").collect();
                let public_txs = public_txs.into_iter().map(|x| x.to_string()).collect();

                let private_txs: String = row.get(2)?;
                let private_txs: Vec<&str> = private_txs.split(",").collect();
                let private_txs = private_txs.into_iter().map(|x| x.to_string()).collect();

                Ok(BlockPrivy {
                    number,
                    public_txs,
                    private_txs,
                })
            })
            .unwrap();

        let result = privy_iter.next().unwrap().unwrap();

        println!("privy_iter {:?}", result);
    }
}
