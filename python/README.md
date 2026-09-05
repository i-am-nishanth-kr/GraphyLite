# pygraphylite — Python bindings

Embedded labeled property graph, Cypher subset, powered by `redb`.

## Install (dev)

```powershell
pip install maturin
maturin develop --manifest-path crates/pygraphylite/Cargo.toml
# or pip install -e .
```

Requires Rust toolchain.

## Quickstart

```python
from pygraphylite import PyGraphyLite

db = PyGraphyLite.open_in_memory()  # or PyGraphyLite("graph.glite")
db.execute("CREATE (a:Person {name: $name})", {"name": "Alice"})
rows = db.query("MATCH (n:Person) RETURN n")
print(rows[0]["n"]["properties"]["name"])  # Alice

# Edge via MATCH+CREATE (R1)
db.execute("CREATE (b:Person {name: 'Bob'})")
db.execute("MATCH (a:Person {name: 'Alice'}), (b:Person {name: 'Bob'}) CREATE (a)-[:KNOWS]->(b)")
print(db.query("MATCH (a)-[r:KNOWS]->(b) RETURN r"))

# Params, WHERE, SET, DELETE, count, shortestPath, variable-length *1..5
db.query("MATCH (n) WHERE n.age > 27 RETURN n", {})
db.execute("MATCH (n) SET n.age = 30", {})
print(db.schema(), db.list_labels(), db.list_relationship_types())
```

## API

`PyGraphyLite(path)` / `open_in_memory()` / `open(path)`
- `execute(query, params=None) -> {"rows": [...], "affected": int}`
- `query(query, params=None) -> [row_dict]`
- `list_labels() -> [str]`, `list_relationship_types() -> [str]`, `schema() -> str`
- `shortest_path_bfs(start, end, edge_type=None, direction="outgoing")`
- `shortest_path_dijkstra(start, end, edge_type=None, weight_prop="weight", direction="outgoing")`
- `shortest_path_bidirectional(start, end, edge_type=None)`
- `execute_batch([(query, params), ...])`

Rows are flat dicts: `{"n": {"id":1,"labels":[...],"properties":{...}}, "count(n)": 2, "r": {"id":1000000000,"src":1,"dst":2,"type":"KNOWS",...}}`

## Example

See `python/example.py`.

## Build wheel

```powershell
maturin build --release
pip install target/wheels/pygraphylite-*.whl
```
