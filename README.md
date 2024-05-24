# reth-anal

Standalone tool for mempool analysis.

This exposes a new RPC method `anal_getBlockTxPrivyByNumber`.

```bash
curl http://localhost:8545 \
  -X POST \
  -H "Content-Type: application/json" \
  --data '{"method":"anal_getBlockTxPrivyByNumber","params":["0x1304d86"],"id":1,"jsonrpc":"2.0"}'
```

Which will return you the following results:

```json
{
  "jsonrpc": "2.0",
  "result": {
    "number": 19940126,
    "public_txs": ["0x123...", "0x456..."],
    "private_txs": ["0x111...", "0x222..."],
  },
  "id": 1
}
```

## Running

```bash
cargo run -- node --rpc-max-connections 128 \
    --datadir /home/ubuntu/.ethereum/reth \
    --http --http.addr 0.0.0.0 --http.api=admin,debug,eth,net,trace,txpool,web3,rpc,reth,ots \
    --ws --ws.addr 0.0.0.0 --ws.api=admin,debug,eth,net,trace,txpool,web3,rpc,reth,ots \
    --authrpc.jwtsecret /home/ubuntu/.eth2-jwtsecret --authrpc.addr 127.0.0.1 --authrpc.port 8551 --txpool.pending-max-size 500 --txpool.pending-max-count 100000
```