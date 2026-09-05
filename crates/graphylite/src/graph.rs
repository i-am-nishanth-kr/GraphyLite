use crate::error::{GliteError, Result};
use crate::storage::{IN_EDGES_TABLE, OUT_EDGES_TABLE, NODES_TABLE, EDGES_TABLE};
use crate::types::{Edge, Node};
#[allow(unused_imports)]
use redb::ReadableTable;
use std::collections::{HashMap, HashSet, VecDeque, BinaryHeap};
use std::cmp::Ordering;

/// Helper to fetch Node by id via read txn
fn fetch_node(txn: &redb::ReadTransaction, id: u64) -> Result<Option<Node>> {
    let bytes_opt = {
        let table = txn.open_table(NODES_TABLE)?;
        table.get(id)?.map(|g| g.value().to_vec())
    };
    if let Some(bytes) = bytes_opt {
        let data: crate::types::NodeData = bincode::deserialize(&bytes).map_err(|e| GliteError::Serialization(e.to_string()))?;
        Ok(Some(Node{ id, labels: data.labels, properties: data.properties }))
    } else { Ok(None) }
}

fn fetch_edge(txn: &redb::ReadTransaction, id: u64) -> Result<Option<Edge>> {
    let bytes_opt = {
        let table = txn.open_table(EDGES_TABLE)?;
        table.get(id)?.map(|g| g.value().to_vec())
    };
    if let Some(bytes) = bytes_opt {
        let data: crate::types::EdgeData = bincode::deserialize(&bytes).map_err(|e| GliteError::Serialization(e.to_string()))?;
        Ok(Some(Edge{ id, src: data.src, dst: data.dst, edge_type: data.edge_type, properties: data.properties }))
    } else { Ok(None) }
}

fn outgoing_edges(txn: &redb::ReadTransaction, src: u64, edge_type: Option<&str>) -> Result<Vec<Edge>> {
    let edge_ids: Vec<u64> = {
        let table = txn.open_table(OUT_EDGES_TABLE)?;
        let mut ids = vec![];
        if let Some(filter) = edge_type {
            let prefix = crate::storage::encode_out_prefix_typed(src, filter);
            let end = crate::storage::prefix_end(&prefix);
            let iter: Box<dyn Iterator<Item=std::result::Result<_, redb::StorageError>> + '_> = if end.is_empty() {
                Box::new(table.range(prefix.as_slice()..)?)
            } else {
                Box::new(table.range(prefix.as_slice()..end.as_slice())?)
            };
            for entry in iter {
                let ( _k, v) = entry?;
                ids.push(v.value());
            }
        } else {
            let prefix = crate::storage::encode_out_prefix(src);
            let end = crate::storage::prefix_end(&prefix);
            let iter: Box<dyn Iterator<Item=std::result::Result<_, redb::StorageError>> + '_> = if end.is_empty() {
                Box::new(table.range(prefix.as_slice()..)?)
            } else {
                Box::new(table.range(prefix.as_slice()..end.as_slice())?)
            };
            for entry in iter {
                let ( _k, v) = entry?;
                ids.push(v.value());
            }
        }
        ids
    };
    let mut res = vec![];
    for eid in edge_ids {
        if let Some(e) = fetch_edge(txn, eid)? { res.push(e); }
    }
    Ok(res)
}

fn incoming_edges(txn: &redb::ReadTransaction, dst: u64, edge_type: Option<&str>) -> Result<Vec<Edge>> {
    let edge_ids: Vec<u64> = {
        let table = txn.open_table(IN_EDGES_TABLE)?;
        let mut ids = vec![];
        if let Some(filter) = edge_type {
            let prefix = crate::storage::encode_in_prefix_typed(dst, filter);
            let end = crate::storage::prefix_end(&prefix);
            let iter: Box<dyn Iterator<Item=std::result::Result<_, redb::StorageError>> + '_> = if end.is_empty() {
                Box::new(table.range(prefix.as_slice()..)?)
            } else {
                Box::new(table.range(prefix.as_slice()..end.as_slice())?)
            };
            for entry in iter {
                let ( _k, v) = entry?;
                ids.push(v.value());
            }
        } else {
            let prefix = crate::storage::encode_in_prefix(dst);
            let end = crate::storage::prefix_end(&prefix);
            let iter: Box<dyn Iterator<Item=std::result::Result<_, redb::StorageError>> + '_> = if end.is_empty() {
                Box::new(table.range(prefix.as_slice()..)?)
            } else {
                Box::new(table.range(prefix.as_slice()..end.as_slice())?)
            };
            for entry in iter {
                let ( _k, v) = entry?;
                ids.push(v.value());
            }
        }
        ids
    };
    let mut res = vec![];
    for eid in edge_ids {
        if let Some(e) = fetch_edge(txn, eid)? { res.push(e); }
    }
    Ok(res)
}

fn neighbors(txn: &redb::ReadTransaction, node_id: u64, edge_type: Option<&str>, direction: &crate::parser::Direction) -> Result<Vec<(u64, Edge)>> {
    let mut result = vec![];
    match direction {
        crate::parser::Direction::Outgoing => {
            for e in outgoing_edges(txn, node_id, edge_type)? {
                result.push((e.dst, e));
            }
        },
        crate::parser::Direction::Incoming => {
            for e in incoming_edges(txn, node_id, edge_type)? {
                result.push((e.src, e));
            }
        },
        crate::parser::Direction::Undirected => {
            for e in outgoing_edges(txn, node_id, edge_type)? {
                result.push((e.dst, e.clone()));
            }
            for e in incoming_edges(txn, node_id, edge_type)? {
                // avoid duplicate if loop?
                result.push((e.src, e));
            }
        }
    }
    Ok(result)
}

/// BFS unweighted shortest path using VecDeque and HashSet, lazy disk-backed
pub fn bfs_shortest_path(txn: &redb::ReadTransaction, start: u64, end: u64, edge_type: Option<&str>, direction: crate::parser::Direction) -> Result<Option<(Vec<Node>, Vec<Edge>)>> {
    if start == end {
        let n = fetch_node(txn, start)?.ok_or(GliteError::NotFound(format!("node {}", start)))?;
        return Ok(Some((vec![n], vec![])));
    }
    let mut queue = VecDeque::new();
    let mut visited: HashSet<u64> = HashSet::new();
    let mut parent: HashMap<u64, (u64, Edge)> = HashMap::new();

    visited.insert(start);
    queue.push_back(start);

    while let Some(cur) = queue.pop_front() {
        for (nbr, edge) in neighbors(txn, cur, edge_type, &direction)? {
            if visited.contains(&nbr) { continue; }
            visited.insert(nbr);
            parent.insert(nbr, (cur, edge.clone()));
            if nbr == end {
                // reconstruct path
                let mut nodes_rev = vec![];
                let mut edges_rev = vec![];
                let mut cur_id = end;
                nodes_rev.push(fetch_node(txn, cur_id)?.unwrap());
                while cur_id != start {
                    let (prev, e) = parent.get(&cur_id).unwrap().clone();
                    edges_rev.push(e);
                    cur_id = prev;
                    nodes_rev.push(fetch_node(txn, cur_id)?.unwrap());
                }
                nodes_rev.reverse();
                edges_rev.reverse();
                return Ok(Some((nodes_rev, edges_rev)));
            }
            queue.push_back(nbr);
        }
    }
    Ok(None)
}

#[derive(Debug)]
struct HeapItem { cost: f64, node: u64 }
impl PartialEq for HeapItem { fn eq(&self, other: &Self) -> bool { self.cost == other.cost } }
impl Eq for HeapItem {}
impl PartialOrd for HeapItem { fn partial_cmp(&self, other: &Self) -> Option<Ordering> { other.cost.partial_cmp(&self.cost) } }
impl Ord for HeapItem { fn cmp(&self, other: &Self) -> Ordering { self.partial_cmp(other).unwrap() } }

/// Dijkstra weighted shortest path, lazy fetching edge payloads
pub fn dijkstra_shortest_path(txn: &redb::ReadTransaction, start: u64, end: u64, edge_type: Option<&str>, weight_prop: &str, direction: crate::parser::Direction) -> Result<Option<(Vec<Node>, Vec<Edge>, f64)>> {
    let mut dist: HashMap<u64, f64> = HashMap::new();
    let mut parent: HashMap<u64, (u64, Edge)> = HashMap::new();
    let mut heap = BinaryHeap::new();
    dist.insert(start, 0.0);
    heap.push(HeapItem{ cost: 0.0, node: start });
    let mut visited = HashSet::new();

    while let Some(HeapItem{ cost, node }) = heap.pop() {
        if visited.contains(&node) { continue; }
        visited.insert(node);
        if node == end {
            // reconstruct
            let mut nodes_rev = vec![fetch_node(txn, node)?.unwrap()];
            let mut edges_rev = vec![];
            let mut cur = node;
            while cur != start {
                let (prev, e) = parent.get(&cur).cloned().ok_or(GliteError::Execution("dijkstra parent missing".into()))?;
                edges_rev.push(e);
                cur = prev;
                nodes_rev.push(fetch_node(txn, cur)?.unwrap());
            }
            nodes_rev.reverse();
            edges_rev.reverse();
            return Ok(Some((nodes_rev, edges_rev, cost)));
        }
        if cost > *dist.get(&node).unwrap_or(&f64::INFINITY) { continue; }
        for (nbr, edge) in neighbors(txn, node, edge_type, &direction)? {
            let w = edge.properties.get(weight_prop).and_then(|v| match v {
                crate::types::Value::Float(f) => Some(*f),
                crate::types::Value::Integer(i) => Some(*i as f64),
                _ => None,
            }).unwrap_or(1.0);
            let next_cost = cost + w;
            if next_cost < *dist.get(&nbr).unwrap_or(&f64::INFINITY) {
                dist.insert(nbr, next_cost);
                parent.insert(nbr, (node, edge.clone()));
                heap.push(HeapItem{ cost: next_cost, node: nbr });
            }
        }
    }
    Ok(None)
}

/// Bidirectional BFS for large graphs
pub fn bidirectional_bfs(txn: &redb::ReadTransaction, start: u64, end: u64, edge_type: Option<&str>) -> Result<Option<(Vec<Node>, Vec<Edge>)>> {
    if start == end {
        let n = fetch_node(txn, start)?.unwrap();
        return Ok(Some((vec![n], vec![])));
    }
    let mut q_fwd = VecDeque::new();
    let mut q_bwd = VecDeque::new();
    let mut visited_fwd: HashMap<u64, Option<(u64, Edge)>> = HashMap::new();
    let mut visited_bwd: HashMap<u64, Option<(u64, Edge)>> = HashMap::new();
    // parent maps for reconstruction: node -> (parent, edge)
    let mut parent_fwd: HashMap<u64, (u64, Edge)> = HashMap::new();
    let mut parent_bwd: HashMap<u64, (u64, Edge)> = HashMap::new();

    q_fwd.push_back(start);
    visited_fwd.insert(start, None);
    q_bwd.push_back(end);
    visited_bwd.insert(end, None);

    let mut meeting: Option<u64> = None;

    while !q_fwd.is_empty() && !q_bwd.is_empty() {
        // expand forward
        if let Some(cur) = q_fwd.pop_front() {
            for (nbr, edge) in neighbors(txn, cur, edge_type, &crate::parser::Direction::Outgoing)? {
                if visited_fwd.contains_key(&nbr) { continue; }
                visited_fwd.insert(nbr, Some((cur, edge.clone())));
                parent_fwd.insert(nbr, (cur, edge.clone()));
                q_fwd.push_back(nbr);
                if visited_bwd.contains_key(&nbr) {
                    meeting = Some(nbr);
                    break;
                }
            }
            if meeting.is_some() { break; }
        }
        if meeting.is_some() { break; }
        // expand backward (reverse direction)
        if let Some(cur) = q_bwd.pop_front() {
            // For backward search, we traverse incoming edges reversed (i.e., predecessors)
            for (nbr, edge) in neighbors(txn, cur, edge_type, &crate::parser::Direction::Incoming)? {
                if visited_bwd.contains_key(&nbr) { continue; }
                visited_bwd.insert(nbr, Some((cur, edge.clone())));
                parent_bwd.insert(nbr, (cur, edge.clone()));
                q_bwd.push_back(nbr);
                if visited_fwd.contains_key(&nbr) {
                    meeting = Some(nbr);
                    break;
                }
            }
            if meeting.is_some() { break; }
        }
    }

    if let Some(mid) = meeting {
        // Reconstruct forward part start->mid
        let mut cur = mid;
        let mut rev_nodes = vec![fetch_node(txn, cur)?.ok_or(GliteError::NotFound(format!("node {}", cur)))?];
        let mut rev_edges = vec![];
        while cur != start {
            let (prev, e) = parent_fwd.get(&cur).cloned().ok_or(GliteError::Execution("bidirectional parent_fwd missing".into()))?;
            rev_edges.push(e);
            cur = prev;
            rev_nodes.push(fetch_node(txn, cur)?.ok_or(GliteError::NotFound(format!("node {}", cur)))?);
        }
        rev_nodes.reverse();
        rev_edges.reverse();
        let mut fwd_nodes = rev_nodes;
        let mut fwd_edges = rev_edges;

        // Reconstruct backward part mid->end via parent_bwd forward chain (mid -> successor -> ... -> end)
        // parent_bwd maps node -> (successor towards end, edge)
        if mid != end {
            let mut cur2 = mid;
            let mut bwd_nodes = Vec::new();
            let mut bwd_edges = Vec::new();
            let mut steps = 0usize;
            // Guard against infinite loops (graph cycles)
            while cur2 != end {
                if steps > 10000 {
                    return Err(GliteError::Execution("bidirectional reconstruction loop".into()));
                }
                let (next, edge) = parent_bwd.get(&cur2).cloned().ok_or(GliteError::Execution(format!("bidirectional parent_bwd missing for {}", cur2)))?;
                bwd_edges.push(edge);
                cur2 = next;
                bwd_nodes.push(fetch_node(txn, cur2)?.ok_or(GliteError::NotFound(format!("node {}", cur2)))?);
                steps += 1;
            }
            fwd_nodes.extend(bwd_nodes);
            fwd_edges.extend(bwd_edges);
        }
        return Ok(Some((fwd_nodes, fwd_edges)));
    }
    Ok(None)
}
