# GraphyLite (`glite`) — Embedded Labeled Property Graph

Lightweight embedded graph database in Rust, Cypher subset, persisted with `redb`. Ships as Rust library `graphylite`, interactive CLI `glite`, and Python bindings `pygraphylite` (PyO3/maturin).

> **Status:** `cargo check` / `cargo test` pass (74 tests: 14 integration + 60 exhaustive Cypher). `cargo build` produces `target/debug/glite`.

## Features

- **Storage** `src/storage.rs:7`: `redb 2.6` tables `nodes(u64->[u8])`, `edges(u64->[u8])`, `out_edges(&[u8]->u64)`, `in_edges(&[u8]->u64)`, `label_index(&[u8]->u64)`, `meta`. `bincode` serialization, auto-increment IDs (edge offset `1_000_000_000` to avoid collision), `Arc<Database>` SWMR, prefix-range scans `encode_*_prefix` / `prefix_end`.
- **Parser** `src/parser.rs:23`: Cypher subset validated by `decypher`, supports `CREATE` (node/edge props `$param`), `MATCH` path `(a:Label {prop})-[r:TYPE]->(b)` with `,`, `WHERE (= <> > < >= <= AND)`, `SET`, `DELETE`, `RETURN (var, var.prop AS alias, count(n), DISTINCT, shortestPath, p=shortestPath)`, variable-length `[*]`, `[*1..5]`, `[*..5]`.
- **Executor** `src/executor.rs:15`: `execute_write` (CREATE reuse via `var_to_id` + `MATCH+CREATE` cross-product), `execute_read` (label-index `IndexScan`, cross-product, `WHERE`, `RETURN` projection, `count` aggregation), lazy BFS/Dijkstra/Bidirectional via `src/graph.rs:93`.
- **Graph** `src/graph.rs:93`: `bfs_shortest_path`, `dijkstra_shortest_path` (weight prop), `bidirectional_bfs` (proper `parent_bwd` reconstruction).
- **CLI** `src/main.rs:1`: `clap` + `rustyline` multiline (`;`), `.tables/.labels` `.schema` `.mode table|json|csv` `.timer` `.import/.export` `.help`.

## Workspace Layout

```
Cargo.toml:1 (members)
crates/graphylite/src/
  lib.rs:20      public API GraphyLite
  error.rs:4     GliteError
  types.rs:8     Value/Node/Edge/Row/PathResult
  storage.rs:7   redb tables + encode
  parser.rs:23   Cypher parser
  executor.rs:15 planner
  graph.rs:93    BFS/Dijkstra/Bidirectional
crates/glite/src/main.rs:1         REPL
crates/pygraphylite/src/lib.rs:1   Python bindings
crates/graphylite/tests/
  basic.rs:1              14 integration tests
  cypher_exhaustive.rs:1  60 exhaustive Cypher tests (simple → complex)
```

## Quickstart — Rust

**Dependency** (local path for development):
```toml
[dependencies]
graphylite = { path = "crates/graphylite" }
```
*For crates.io: `graphylite = "0.1"` (when published)*

```rust
use graphylite::{GraphyLite, params};
use std::collections::HashMap;

let db = GraphyLite::open("graph.glite")?; // or open_in_memory()
 // CREATE
db.execute("CREATE (a:Person {name: $name})", params!("name" => "Alice"))?;
db.execute("CREATE (a:Person {name:'A'})-[:KNOWS]->(b:Person {name:'B'})", HashMap::new())?;
// MATCH + CREATE edge
db.execute("MATCH (a:Person {name:$a}), (b:Person {name:$b}) CREATE (a)-[:KNOWS]->(b)",
    params!("a"=>"Alice","b"=>"Bob"))?;
// MATCH / WHERE / RETURN
let rows = db.query("MATCH (n:Person) WHERE n.age > 27 RETURN n", HashMap::new())?;
for r in &rows { println!("{:?}", r.nodes.get("n").unwrap().properties.get("name")); }
let r = db.query("MATCH (n:Person) RETURN n.name AS name", HashMap::new())?;
let r = db.query("MATCH (n:Person) RETURN count(n)", HashMap::new())?; // -> {"count(n)":2}
// SET / DELETE
db.execute("MATCH (n:Person {name:'Alice'}) SET n.age = 31", HashMap::new())?;
db.execute("MATCH (n:Person {name:'Alice'}) DELETE n", HashMap::new())?;
// Variable-length & shortestPath
db.query("MATCH (a:Person {name:'A'})-[:KNOWS*1..5]->(c) RETURN c", HashMap::new())?;
let r = db.query("MATCH (a:Node {id:1}), (c:Node {id:3}) RETURN shortestPath((a)-[:ROAD*]->(c), 'weight')", HashMap::new())?;
let p = &r[0].paths.values().next().unwrap(); // PathResult {length, nodes, edges}
// Pathfinding API
let a_id = db.query("MATCH (n:Node {id:1}) RETURN n", HashMap::new())?[0].nodes["n"].id;
let c_id = db.query("MATCH (n:Node {id:3}) RETURN n", HashMap::new())?[0].nodes["n"].id;
db.shortest_path_bfs(a_id, c_id, Some("ROAD"), graphylite::parser::Direction::Outgoing)?;
db.shortest_path_dijkstra(a_id, c_id, Some("ROAD"), "weight", graphylite::parser::Direction::Outgoing)?;
db.shortest_path_bidirectional(a_id, c_id, Some("ROAD"))?;
// Schema / batch
println!("{:?}", db.list_labels()?);
println!("{}", db.schema()?);
db.execute_batch(vec![("CREATE (n:Person {name:'A'})".into(), HashMap::new())])?;
```

`Value` `types.rs:5`: `String/Integer/Float/Boolean/Null`, `Row` `types.rs:93` = `values`/`nodes`/`edges`/`paths`.

## Quickstart — Python

No separate server — bindings are `cdylib` via PyO3/maturin `pyproject.toml:1`.

```bash
pip install maturin
maturin develop --manifest-path crates/pygraphylite/Cargo.toml
# or pip install -e .  /  maturin build --release && pip install target/wheels/*.whl
```

```python
from pygraphylite import PyGraphyLite

db = PyGraphyLite.open_in_memory()  # or PyGraphyLite("graph.glite")
db.execute("CREATE (a:Person {name: $name})", {"name": "Alice"})
db.execute("CREATE (b:Person {name: 'Bob'})")
db.execute("MATCH (a:Person {name: $a}), (b:Person {name: $b}) CREATE (a)-[:KNOWS]->(b)",
           {"a": "Alice", "b": "Bob"})

rows = db.query("MATCH (n:Person) RETURN n")
print(rows[0]["n"])  # {'id':1,'labels':['Person'],'properties':{'name':'Alice'}}

print(db.query("MATCH (n:Person) WHERE n.age > 27 RETURN n", {}))
db.execute("MATCH (n:Person {name: 'Alice'}) SET n.age = 30", {})
print(db.query("MATCH (n) RETURN count(n)"))

# shortestPath + Dijkstra
db.execute("CREATE (c:Person {name: 'Carol'})")
db.execute("MATCH (b:Person {name:'Bob'}), (c:Person {name:'Carol'}) CREATE (b)-[:KNOWS]->(c)")
print(db.query("MATCH (a:Person {name:'Alice'}), (c:Person {name:'Carol'}) RETURN shortestPath((a)-[:KNOWS*]->(c))"))
print(db.query("MATCH (a:Node {id:1}), (c:Node {id:3}) RETURN shortestPath((a)-[:ROAD*]->(c), 'weight')"))

# Direct APIs
a_id = db.query("MATCH (n:Node {id:1}) RETURN n")[0]["n"]["id"]
c_id = db.query("MATCH (n:Node {id:3}) RETURN n")[0]["n"]["id"]
print(db.shortest_path_bfs(a_id, c_id, "ROAD", "outgoing"))
print(db.shortest_path_dijkstra(a_id, c_id, "ROAD", "weight", "outgoing"))
print(db.list_labels(), db.schema())
```

See `python/example.py` and `python/README.md` for full.

## CLI — `glite`

```bash
cargo build --workspace
./target/debug/glite --help
./target/debug/glite graph.glite --command "CREATE (a:Person {name:'Alice'}) RETURN a"
./target/debug/glite # REPL
glite> MATCH (n:Person) RETURN n;
glite> MATCH (n) WHERE n.age > 27 RETURN n.name;
glite> .schema
glite> .mode json   # table|json|csv  main.rs:60
glite> .timer off
glite> .import dump.cypher   # main.rs:60
glite> .export out.txt
glite> .help
```

## Building from Source

**Prerequisites:** Rust stable, Python 3.8+ for bindings.

```powershell
git clone <repo>
cd GraphyLite
cargo check --workspace          # 0 warnings
cargo test --workspace           # 74 tests (14 + 60)
cargo build --workspace          # glite binary
./target/debug/glite --help

# Python dev
pip install maturin
maturin develop --manifest-path crates/pygraphylite/Cargo.toml
python python/example.py
py -m maturin build --release --manifest-path crates/pygraphylite/Cargo.toml
# wheel at target/wheels/pygraphylite-*.whl
```

If `maturin develop` complains about virtualenv, use `maturin build` + `pip install <wheel>`.

## Cypher Subset

| Clause | Example |
|---|---|
| `CREATE` | `(a:Person {name:$name})`, `(a)-[r:KNOWS {weight:5}]->(b)`, multi-path `,(b)-[:KNOWS]->(c)` |
| `MATCH` | `(n:Person)`, `(a)-[r:TYPE]->(b)`, `(a),(b)` (cross-join), `-[r:TYPE*]->`, `-[r:TYPE*1..5]->` |
| `WHERE` | `n.age = 30`, `<> != > < >= <=`, `AND`, `n.prop = $param`, `n.a = m.b`, `Null`/`NaN` aware |
| `SET` | `n.age = 31, n.city = 'Paris'` |
| `DELETE` | `DELETE n`, `DELETE r` (cascades) |
| `RETURN` | `n`, `n.name AS alias`, `count(n)`, `DISTINCT`, `shortestPath((a)-[:TYPE*]->(b))`, `shortestPath((a)-[:TYPE*]->(b), 'weight')`, `p=shortestPath(...)` |
| `Direction` | `->`, `<-`, `--` (undirected) |

Unsupported: `ORDER BY`, `LIMIT`, `SKIP`, `WITH` (parsed as RETURN), `MERGE` (use `MATCH+CREATE`).

## Testing

Exhaustive suite `crates/graphylite/tests/cypher_exhaustive.rs` (60 tests, simple→complex across 14 categories):

- **CREATE node** (bare, labels, multi-label, all Value types, params)
- **CREATE edge/path** (multi-path, reuse `b` intra-query, `MATCH+CREATE` + multiple bindings)
- **MATCH** (all, label+prop, single hop, undirected, multi-pattern)
- **WHERE** (`= <> != > < >= <=`, `AND`, prop-vs-prop, param, Float/mixed, Null, NaN)
- **SET/DELETE** (single, param, multi), DELETE (node cascade, edge, noop)
- **RETURN** (var, prop, alias, `count`, `DISTINCT`, `CREATE RETURN`, `shortestPath`)
- **Variable-length** `*`, `*1..2`, no-match
- **Direction**
- **Params** (types, missing error, WHERE+CREATE)
- **Schema/prefix scan** (`LabelA` 30 nodes, `out_edges` filter)
- **Pathfinding** BFS vs bidirectional vs Dijkstra cheapest
- **Batch**, affected
- **Error handling** (parse error, unbound var)
- **Integration** social graph (5 people chain, `*`, shortestPath, SET/DELETE/count)

Plus 14 integration tests in `crates/graphylite/tests/basic.rs`. Total: 74 tests.

Run: `cargo test --workspace -- --nocapture` or `cargo test -p graphylite --test cypher_exhaustive`.

## Publishing

`README.md` + `LICENSE` + `pyproject.toml` ready for GitHub. For crates.io: `cargo publish -p graphylite`. For PyPI: `maturin publish`.

## License

MIT OR Apache-2.0
