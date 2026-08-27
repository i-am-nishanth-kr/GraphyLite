pub mod error;
pub mod types;
pub mod storage;
pub mod parser;
pub mod executor;
pub mod graph;

use error::{GliteError, Result};
use redb::ReadableTable;
use types::{Row, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

#[derive(Clone)]
pub struct GraphyLite {
    db: Arc<redb::Database>,
}

impl GraphyLite {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = storage::open_db(path)?;
        Ok(Self{ db })
    }

    pub fn open_in_memory() -> Result<Self> {
        let db = storage::open_in_memory()?;
        Ok(Self{ db })
    }

    /// Execute a Cypher query. Returns (num_affected, rows). For pure read queries, num_affected == rows.len().
    pub fn execute<S: AsRef<str>>(&self, query: S, params: HashMap<String, Value>) -> Result<ExecutionResult> {
        let pq = parser::parse_query(query.as_ref(), &params)?;
        // Decide read vs write
        let is_write = !pq.creates.is_empty() || !pq.sets.is_empty() || !pq.deletes.is_empty();
        if is_write {
            let res = executor::execute_write(&self.db, pq, &params)?;
            Ok(ExecutionResult{ rows: res.rows, affected: res.affected })
        } else {
            let res = executor::execute_read(&self.db, pq, &params)?;
            Ok(ExecutionResult{ rows: res.rows, affected: res.affected })
        }
    }

    /// Convenience query returns rows
    pub fn query<S: AsRef<str>>(&self, query: S, params: HashMap<String, Value>) -> Result<Vec<Row>> {
        let r = self.execute(query, params)?;
        Ok(r.rows)
    }

    /// Helper to list labels (for CLI .labels)
    pub fn list_labels(&self) -> Result<Vec<String>> {
        let rtxn = self.db.begin_read()?;
        let tbl = rtxn.open_table(storage::LABEL_INDEX_TABLE)?;
        let mut set = std::collections::HashSet::new();
        for entry in tbl.iter()? {
            let (k, _) = entry?;
            let (lbl, _nid) = storage::decode_label_key(k.value())?;
            set.insert(lbl);
        }
        let mut v: Vec<String> = set.into_iter().collect();
        v.sort();
        Ok(v)
    }

    pub fn list_relationship_types(&self) -> Result<Vec<String>> {
        let rtxn = self.db.begin_read()?;
        let tbl = rtxn.open_table(storage::EDGES_TABLE)?;
        let mut set = std::collections::HashSet::new();
        for entry in tbl.iter()? {
            let (_, v) = entry?;
            let ed: types::EdgeData = bincode::deserialize(v.value()).map_err(|e| GliteError::Serialization(e.to_string()))?;
            set.insert(ed.edge_type);
        }
        let mut v: Vec<String> = set.into_iter().collect();
        v.sort();
        Ok(v)
    }

    pub fn schema(&self) -> Result<String> {
        let labels = self.list_labels()?;
        let rels = self.list_relationship_types()?;
        Ok(format!("Labels: {:?}\nRelationshipTypes: {:?}\nIndexes: label_index (Label, NodeID), out_edges (Src, Type, Dst), in_edges (Dst, Type, Src)", labels, rels))
    }

    // Pathfinding public APIs
    pub fn shortest_path_bfs(&self, start: u64, end: u64, edge_type: Option<&str>, direction: parser::Direction) -> Result<Option<(Vec<types::Node>, Vec<types::Edge>)>> {
        let rtxn = self.db.begin_read()?;
        graph::bfs_shortest_path(&rtxn, start, end, edge_type, direction)
    }
    pub fn shortest_path_dijkstra(&self, start: u64, end: u64, edge_type: Option<&str>, weight_prop: &str, direction: parser::Direction) -> Result<Option<(Vec<types::Node>, Vec<types::Edge>, f64)>> {
        let rtxn = self.db.begin_read()?;
        graph::dijkstra_shortest_path(&rtxn, start, end, edge_type, weight_prop, direction)
    }
    pub fn shortest_path_bidirectional(&self, start: u64, end: u64, edge_type: Option<&str>) -> Result<Option<(Vec<types::Node>, Vec<types::Edge>)>> {
        let rtxn = self.db.begin_read()?;
        graph::bidirectional_bfs(&rtxn, start, end, edge_type)
    }
}

pub struct ExecutionResult {
    pub rows: Vec<Row>,
    pub affected: usize,
}

#[macro_export]
macro_rules! params {
    ( $( $key:expr => $value:expr ),* $(,)? ) => {{
        let mut map = std::collections::HashMap::new();
        $(
            map.insert($key.to_string(), $crate::types::Value::from($value));
        )*
        map
    }};
}

// Re-export Value for convenience
pub use types::Value as ParamValue;
