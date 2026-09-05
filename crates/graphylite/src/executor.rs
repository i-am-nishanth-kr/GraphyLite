use crate::error::{GliteError, Result};
use crate::parser::{ParsedQuery, ReturnExpr, Direction, eval_expr};
use crate::storage::{self, NODES_TABLE, EDGES_TABLE, OUT_EDGES_TABLE, IN_EDGES_TABLE, LABEL_INDEX_TABLE};
use crate::types::{Edge, Node, NodeData, EdgeData, Row, Value, PathResult};
use redb::ReadableTable;
use std::collections::HashMap;

#[derive(Debug)]
pub struct ExecutionResult {
    pub rows: Vec<Row>,
    pub affected: usize,
}

// Main entry for read/write execution using transactions

pub fn execute_write(db: &redb::Database, pq: ParsedQuery, params: &HashMap<String, Value>) -> Result<ExecutionResult> {
    let write_txn = db.begin_write()?;
    let mut affected = 0usize;
    let mut rows = vec![];

    // Handle CREATE with reuse support (R1)
    // - var_to_id persists across all CREATE paths within same query
    // - if MATCH bindings exist, CREATE reuses matched node ids (e.g., MATCH (a),(b) CREATE (a)-[:KNOWS]->(b))
    if !pq.creates.is_empty() {
        // Determine base bindings from MATCH to seed CREATE reuse
        let base_bindings: Vec<HashMap<String, u64>> = if !pq.matches.is_empty() || pq.shortest_path.is_some() {
            let b = find_matches_write(&write_txn, &pq, params)?;
            if b.is_empty() { vec![HashMap::new()] } else { b }
        } else {
            vec![HashMap::new()]
        };

        // For each base binding, execute all CREATE paths sharing the same var_to_id map.
        // This enables MATCH+CREATE cross-product edge creation.
        let mut all_create_maps: Vec<HashMap<String, u64>> = Vec::new();
        for base in &base_bindings {
            let mut var_to_id: HashMap<String, u64> = base.clone();
            for create in &pq.creates {
                for path in &create.paths {
                    // Track node ids created in this path in order for edge resolution fallback
                    let mut path_node_ids: Vec<u64> = Vec::new();
                    for node_pat in &path.nodes {
                        if let Some(var) = &node_pat.variable {
                            if var_to_id.contains_key(var) {
                                // Reuse existing node (MATCH+CREATE or intra-query reuse)
                                // Path still needs id for edge creation ordering
                                let existing = *var_to_id.get(var).unwrap();
                                path_node_ids.push(existing);
                                // Optional: if properties specified with reuse, treat as additional props update?
                                // For now skip creation to avoid duplicate (R1 fix)
                                continue;
                            }
                        }
                        // Create new node
                        let mut props = HashMap::new();
                        for (k, expr) in &node_pat.properties {
                            let v = eval_expr(expr, params)?;
                            props.insert(k.clone(), v);
                        }
                        let node_data = NodeData{ labels: node_pat.labels.clone(), properties: props };
                        let bytes = storage::serialize_node(&node_data)?;
                        let nid = storage::next_node_id(&write_txn)?;
                        {
                            let mut tbl = write_txn.open_table(NODES_TABLE)?;
                            tbl.insert(nid, bytes.as_slice())?;
                        }
                        {
                            let mut lbl_tbl = write_txn.open_table(LABEL_INDEX_TABLE)?;
                            for lbl in &node_pat.labels {
                                let key = storage::encode_label_key(lbl, nid);
                                lbl_tbl.insert(key.as_slice(), nid)?;
                            }
                        }
                        if let Some(var) = &node_pat.variable {
                            var_to_id.insert(var.clone(), nid);
                        }
                        path_node_ids.push(nid);
                        affected += 1;
                    }
                    // Create edges for this path
                    for (idx, edge_pat) in path.edges.iter().enumerate() {
                        // Resolve src/dst: prefer variable lookup, fallback to positional path_node_ids
                        let src_node_pat = &path.nodes[idx];
                        let dst_node_pat = &path.nodes[idx+1];
                        let src_id = if let Some(sv) = &src_node_pat.variable {
                            if let Some(id) = var_to_id.get(sv) { *id } else { path_node_ids[idx] }
                        } else {
                            path_node_ids[idx]
                        };
                        let dst_id = if let Some(dv) = &dst_node_pat.variable {
                            if let Some(id) = var_to_id.get(dv) { *id } else { path_node_ids[idx+1] }
                        } else {
                            path_node_ids[idx+1]
                        };
                        // Ensure nodes were resolved
                        if path_node_ids.is_empty() {
                            return Err(GliteError::Execution("edge source/target unresolved".into()));
                        }
                        let mut props = HashMap::new();
                        for (k, expr) in &edge_pat.properties {
                            let v = eval_expr(expr, params)?;
                            props.insert(k.clone(), v);
                        }
                        let edge_type = edge_pat.edge_type.clone().unwrap_or_else(|| "RELATED".to_string());
                        let edge_data = EdgeData{ src: src_id, dst: dst_id, edge_type: edge_type.clone(), properties: props };
                        let bytes = storage::serialize_edge(&edge_data)?;
                        let eid = storage::next_edge_id(&write_txn)?;
                        {
                            let mut etbl = write_txn.open_table(EDGES_TABLE)?;
                            etbl.insert(eid, bytes.as_slice())?;
                        }
                        {
                            let mut out = write_txn.open_table(OUT_EDGES_TABLE)?;
                            let key = storage::encode_out_key(src_id, &edge_type, dst_id);
                            out.insert(key.as_slice(), eid)?;
                        }
                        {
                            let mut inn = write_txn.open_table(IN_EDGES_TABLE)?;
                            let key = storage::encode_in_key(dst_id, &edge_type, src_id);
                            inn.insert(key.as_slice(), eid)?;
                        }
                        if let Some(v) = &edge_pat.variable {
                            var_to_id.insert(v.clone(), eid);
                        }
                        affected += 1;
                    }
                }
            }
            all_create_maps.push(var_to_id);
        }
        // Handle RETURN after CREATE (one row per created binding)
        if let Some(ret) = &pq.returns {
            for var_to_id in &all_create_maps {
                let mut row = Row::new();
                for item in &ret.items {
                    match &item.expr {
                        ReturnExpr::Variable(var) => {
                            if let Some(id) = var_to_id.get(var) {
                                if let Some(node) = fetch_node_write(&write_txn, *id)? {
                                    row.nodes.insert(item.alias.clone().unwrap_or_else(|| var.clone()), node);
                                } else if let Some(edge) = fetch_edge_write(&write_txn, *id)? {
                                    row.edges.insert(item.alias.clone().unwrap_or_else(|| var.clone()), edge);
                                }
                            }
                        },
                        ReturnExpr::Property{ var, prop } => {
                            if let Some(id) = var_to_id.get(var) {
                                if let Some(node) = fetch_node_write(&write_txn, *id)? {
                                    if let Some(val) = node.properties.get(prop) {
                                        let key = item.alias.clone().unwrap_or_else(|| format!("{}.{}", var, prop));
                                        row.values.insert(key, val.clone());
                                    }
                                } else if let Some(edge) = fetch_edge_write(&write_txn, *id)? {
                                    if let Some(val) = edge.properties.get(prop) {
                                        let key = item.alias.clone().unwrap_or_else(|| format!("{}.{}", var, prop));
                                        row.values.insert(key, val.clone());
                                    }
                                }
                            }
                        },
                        ReturnExpr::Count(var) => {
                            let key = item.alias.clone().unwrap_or_else(|| format!("count({})", var));
                            row.values.insert(key, Value::Integer(1));
                        },
                        _ => {}
                    }
                }
                if !row.nodes.is_empty() || !row.values.is_empty() || !row.edges.is_empty() || !ret.items.is_empty() {
                    // For count aggregation need later handling, push per-binding row for now
                    if ret.items.iter().any(|i| matches!(i.expr, ReturnExpr::Count(_))) {
                        rows.push(row);
                    } else if !row.nodes.is_empty() || !row.values.is_empty() || !row.edges.is_empty() {
                        rows.push(row);
                    } else if all_create_maps.len()==1 {
                        // empty return not pushing unless needed
                    }
                }
            }
            // aggregate COUNT if present
            if ret.items.iter().any(|i| matches!(i.expr, ReturnExpr::Count(_))) && !rows.is_empty() {
                let cnt = rows.len() as i64;
                let mut agg = Row::new();
                for item in &ret.items {
                    if let ReturnExpr::Count(var) = &item.expr {
                        let key = item.alias.clone().unwrap_or_else(|| format!("count({})", var));
                        agg.values.insert(key, Value::Integer(cnt));
                    }
                }
                rows = vec![agg];
            } else if ret.items.iter().any(|i| matches!(i.expr, ReturnExpr::Count(_))) && rows.is_empty() && !all_create_maps.is_empty() {
                let cnt = all_create_maps.len() as i64;
                let mut agg = Row::new();
                for item in &ret.items {
                    if let ReturnExpr::Count(var)=&item.expr {
                        let key = item.alias.clone().unwrap_or_else(|| format!("count({})", var));
                        agg.values.insert(key, Value::Integer(cnt));
                    }
                }
                rows = vec![agg];
            }
            // If CREATE+RETURN already produced rows, commit and return early (skip generic MATCH+SET/DELETE return duplication)
            if !rows.is_empty() || !pq.sets.is_empty() || !pq.deletes.is_empty() {
                // If there are also SET/DELETEs, fall through to handle them (but avoid double RETURN for MATCH-only)
                if pq.sets.is_empty() && pq.deletes.is_empty() && !base_bindings.is_empty() && !pq.matches.is_empty() && pq.creates.len()>0 {
                    // For MATCH+CREATE+RETURN case, we already produced RETURN rows; if original logic would also produce rows
                    // we need to prevent duplicate. We'll override later handling by marking creates handled.
                }
            }
            if pq.sets.is_empty() && pq.deletes.is_empty() && pq.returns.is_some() {
                // Pure CREATE (or MATCH+CREATE) with RETURN: we have rows, can commit now unless SET/DELETE pending
                // But to allow MATCH+CREATE without RETURN to continue for SET, we check.
                // If no SET/DELETE, commit early.
                write_txn.commit()?;
                return Ok(ExecutionResult{ rows, affected });
            }
        } else {
            // No RETURN after CREATE but CREATE executed; if no SET/DELETE, commit and return
            if pq.sets.is_empty() && pq.deletes.is_empty() && pq.returns.is_none() {
                write_txn.commit()?;
                return Ok(ExecutionResult{ rows, affected });
            }
        }
        // If we have SET/DELETE together with CREATE+MATCH, fall through to generic handling below
        // Need to keep affected/rows accumulated; generic handling will append further.
    }

    // Handle MATCH + WHERE + SET/DELETE + RETURN for write transactions as well (if match exists without create)
    // For simplicity, if there are matches and (sets or deletes), execute them in same write txn
    if (!pq.matches.is_empty() || pq.shortest_path.is_some()) && (!pq.sets.is_empty() || !pq.deletes.is_empty() || pq.returns.is_some()) {
        // We need to execute matching using write txn's tables? redb write tx can read.
        // We'll perform in-txn read using write_txn.
        // To avoid code duplication, we will extract logic to functions that work on generic table access.
        // For MVP we commit creates first then reopen? Instead we will handle SET/DELETE/RETURN after creates within same txn using helpers.
        if !pq.sets.is_empty() || !pq.deletes.is_empty() || pq.returns.is_some() {
            // We need to find matches; we will use write_txn's read view
            // We'll implement helper to collect matches using write_txn
            let match_rows = find_matches_write(&write_txn, &pq, params)?;
            // Apply SET
            for set in &pq.sets {
                for binding in &match_rows {
                    for assign in &set.assignments {
                        if let Some(nid) = binding.get(&assign.var) {
                            if let Some(node) = fetch_node_write(&write_txn, *nid)? {
                                let mut nd = NodeData{ labels: node.labels.clone(), properties: node.properties.clone() };
                                let val = eval_expr(&assign.value, params)?;
                                nd.properties.insert(assign.prop.clone(), val);
                                let bytes = storage::serialize_node(&nd)?;
                                {
                                    let mut tbl = write_txn.open_table(NODES_TABLE)?;
                                    tbl.insert(*nid, bytes.as_slice())?;
                                }
                                affected += 1;
                            } else if let Some(edge) = fetch_edge_write(&write_txn, *nid)? {
                                let mut ed = EdgeData{ src: edge.src, dst: edge.dst, edge_type: edge.edge_type.clone(), properties: edge.properties.clone() };
                                let val = eval_expr(&assign.value, params)?;
                                ed.properties.insert(assign.prop.clone(), val);
                                let bytes = storage::serialize_edge(&ed)?;
                                {
                                    let mut etbl = write_txn.open_table(EDGES_TABLE)?;
                                    etbl.insert(*nid, bytes.as_slice())?;
                                }
                                affected += 1;
                            }
                        }
                    }
                }
            }
            // Apply DELETE
            for del in &pq.deletes {
                for binding in &match_rows {
                    for var in &del.variables {
                        if let Some(id) = binding.get(var) {
                            // delete node if exists
                            let is_node = fetch_node_write(&write_txn, *id)?.is_some();
                            if is_node {
                                {
                                    let mut tbl = write_txn.open_table(NODES_TABLE)?;
                                    tbl.remove(*id)?;
                                }
                                affected += 1;
                                // clean label index
                                let keys_to_remove: Vec<Vec<u8>> = {
                                    let lbl = write_txn.open_table(LABEL_INDEX_TABLE)?;
                                    lbl.iter()?.filter_map(|e| {
                                        let (k, v) = e.ok()?;
                                        if v.value() == *id { Some(k.value().to_vec()) } else { None }
                                    }).collect()
                                };
                                for k in keys_to_remove {
                                    let mut lbl = write_txn.open_table(LABEL_INDEX_TABLE)?;
                                    lbl.remove(k.as_slice())?;
                                }
                                // remove out edges
                                let out_keys: Vec<(Vec<u8>, u64)> = {
                                    let out = write_txn.open_table(OUT_EDGES_TABLE)?;
                                    out.iter()?.filter_map(|e| {
                                        let (k,v) = e.ok()?;
                                        let (src, _typ, _dst) = storage::decode_out_key(k.value()).ok()?;
                                        if src == *id { Some((k.value().to_vec(), v.value())) } else { None }
                                    }).collect()
                                };
                                for (k, eid) in out_keys {
                                    {
                                        let mut out = write_txn.open_table(OUT_EDGES_TABLE)?;
                                        out.remove(k.as_slice())?;
                                    }
                                    {
                                        let mut etbl = write_txn.open_table(EDGES_TABLE)?;
                                        let _ = etbl.remove(eid);
                                    }
                                }
                                let in_keys: Vec<(Vec<u8>, u64)> = {
                                    let inn = write_txn.open_table(IN_EDGES_TABLE)?;
                                    inn.iter()?.filter_map(|e| {
                                        let (k,v) = e.ok()?;
                                        let (dst, _typ, _src) = storage::decode_out_key(k.value()).ok()?;
                                        if dst == *id { Some((k.value().to_vec(), v.value())) } else { None }
                                    }).collect()
                                };
                                for (k, eid) in in_keys {
                                    {
                                        let mut inn = write_txn.open_table(IN_EDGES_TABLE)?;
                                        inn.remove(k.as_slice())?;
                                    }
                                    {
                                        let mut etbl = write_txn.open_table(EDGES_TABLE)?;
                                        let _ = etbl.remove(eid);
                                    }
                                }
                            }
                            // try edge delete
                            if let Some(edge) = fetch_edge_write(&write_txn, *id)? {
                                {
                                    let mut etbl = write_txn.open_table(EDGES_TABLE)?;
                                    etbl.remove(*id)?;
                                }
                                {
                                    let mut out = write_txn.open_table(OUT_EDGES_TABLE)?;
                                    let key = storage::encode_out_key(edge.src, &edge.edge_type, edge.dst);
                                    out.remove(key.as_slice())?;
                                }
                                {
                                    let mut inn = write_txn.open_table(IN_EDGES_TABLE)?;
                                    let key2 = storage::encode_in_key(edge.dst, &edge.edge_type, edge.src);
                                    inn.remove(key2.as_slice())?;
                                }
                                affected += 1;
                            }
                        }
                    }
                }
            }
            // Build RETURN rows from match_rows if not already created via CREATE return
            if pq.creates.is_empty() {
                if let Some(ret) = &pq.returns {
                    for binding in &match_rows {
                        // Check shortest path: if pq.shortest_path is set, compute path
                        if let Some(sp) = &pq.shortest_path {
                            // binding should contain start/end ids; but for shortest path queries where MATCH is p=shortestPath(...), match_rows is empty placeholder
                            // We need to resolve start/end node ids via variable lookup (maybe via separate match)
                            // For MVP support RETURN shortestPath((a)-[*]->(b)) where a,b are matched via previous MATCH
                            // So binding has a,b
                            let start_id = binding.get(&sp.start_var).cloned().ok_or(GliteError::Execution(format!("start var {} not bound", sp.start_var)))?;
                            let end_id = binding.get(&sp.end_var).cloned().ok_or(GliteError::Execution(format!("end var {} not bound", sp.end_var)))?;
                            // Need to do BFS using write txn? We have read helper that expects ReadTransaction. We'll duplicate logic with write txn.
                            // For now, perform BFS by scanning write txn tables manually
                            // Use helper that works with write_txn snapshot via reimplemented bfs_write
                            let path_opt = bfs_write(&write_txn, start_id, end_id, sp.edge_type.as_deref(), sp.direction.clone())?;
                            let mut row = Row::new();
                            if let Some((nodes, edges)) = path_opt {
                                let pr = PathResult{ length: edges.len(), nodes: nodes.clone(), edges: edges.clone() };
                                let key = sp.path_var.clone().unwrap_or_else(|| "path".to_string());
                                // Also if RETURN item alias is present use alias
                                let alias = ret.items.iter().find(|it| matches!(it.expr, ReturnExpr::ShortestPath(_))).and_then(|it| it.alias.clone()).unwrap_or(key.clone());
                                row.paths.insert(alias, pr);
                                // Also populate values for length?
                                rows.push(row);
                            } else {
                                // No path found => empty row? For BFS, return empty result set
                            }
                        } else {
                            let mut row = Row::new();
                            for item in &ret.items {
                                match &item.expr {
                                    ReturnExpr::Variable(var) => {
                                        if let Some(id) = binding.get(var) {
                                            if let Some(node) = fetch_node_write(&write_txn, *id)? {
                                                let key = item.alias.clone().unwrap_or_else(|| var.clone());
                                                row.nodes.insert(key.clone(), node);
                                            } else if let Some(edge) = fetch_edge_write(&write_txn, *id)? {
                                                let key = item.alias.clone().unwrap_or_else(|| var.clone());
                                                row.edges.insert(key, edge);
                                            }
                                        }
                                    },
                                    ReturnExpr::Property{ var, prop } => {
                                        if let Some(id) = binding.get(var) {
                                            if let Some(node) = fetch_node_write(&write_txn, *id)? {
                                                if let Some(val) = node.properties.get(prop) {
                                                    let key = item.alias.clone().unwrap_or_else(|| format!("{}.{}", var, prop));
                                                    row.values.insert(key, val.clone());
                                                }
                                            } else if let Some(edge) = fetch_edge_write(&write_txn, *id)? {
                                                if let Some(val) = edge.properties.get(prop) {
                                                    let key = item.alias.clone().unwrap_or_else(|| format!("{}.{}", var, prop));
                                                    row.values.insert(key, val.clone());
                                                }
                                            }
                                        }
                                    },
                                    ReturnExpr::Count(var) => {
                                        // For COUNT we will aggregate later; for write path just per-binding
                                        let key = item.alias.clone().unwrap_or_else(|| format!("count({})", var));
                                        row.values.insert(key, Value::Integer(1));
                                    },
                                    ReturnExpr::ShortestPath(_) => {} // handled above
                                }
                            }
                            // also include where distinct?
                            rows.push(row);
                        }
                    }
                    // handle COUNT aggregation if needed
                    if ret.items.iter().any(|i| matches!(i.expr, ReturnExpr::Count(_))) {
                        // aggregate all rows into one row with count
                        let cnt = rows.len() as i64;
                        let mut agg = Row::new();
                        for item in &ret.items {
                            if let ReturnExpr::Count(var) = &item.expr {
                                let key = item.alias.clone().unwrap_or_else(|| format!("count({})", var));
                                agg.values.insert(key, Value::Integer(cnt));
                            }
                        }
                        rows = vec![agg];
                    }
                }
            }
        }
    }

    write_txn.commit()?;
    Ok(ExecutionResult{ rows, affected })
}

pub fn execute_read(db: &redb::Database, pq: ParsedQuery, params: &HashMap<String, Value>) -> Result<ExecutionResult> {
    let read_txn = db.begin_read()?;

    // If no matches and no shortest_path but has RETURN with maybe constant? For MVP, if pq.matches empty and pq.shortest_path Some, need to find start/end via scanning nodes?
    // We'll handle find_matches_read

    let bindings = find_matches_read(&read_txn, &pq, params)?;

    let mut rows = vec![];

    if let Some(sp) = &pq.shortest_path {
        // shortestPath via read txn, using graph algorithms
        // bindings may contain start/end bindings if MATCH produced them, else we need to search for start/end separately
        // For case MATCH p=shortestPath((a:Label {prop})-[:TYPE*]->(b:Label {prop})) where path nodes have labels/props, we need to enumerate candidates for start/end then BFS for each pair?
        // Simpler: if pq.matches has empty paths (placeholder), we need to create bindings for start/end via candidate enumeration.
        // We'll handle: if bindings is empty but sp is set, try to synthesize bindings by finding candidate nodes for start and end
        if bindings.is_empty() {
            // Attempt to recover start/end candidates from sp: we need to parse original pattern? For now assume sp.start_var and end_var have implied patterns from the path nodes (labels/props not stored)
            // For this edge case, we will brute force: find all nodes, try BFS between each pair? Too expensive.
            //Instead, we require that shortestPath MATCH also had explicit MATCH nodes? We'll handle by returning empty.
            // For now, fallback: if no bindings, treat as no results
        } else {
            for bind in bindings {
                let start_id = bind.get(&sp.start_var).cloned().ok_or(GliteError::Execution(format!("missing start var {}", sp.start_var)))?;
                let end_id = bind.get(&sp.end_var).cloned().ok_or(GliteError::Execution(format!("missing end var {}", sp.end_var)))?;
                let path_opt = match sp.algorithm {
                    crate::parser::PathAlgorithm::Bfs => crate::graph::bfs_shortest_path(&read_txn, start_id, end_id, sp.edge_type.as_deref(), sp.direction.clone())?,
                    crate::parser::PathAlgorithm::Dijkstra => {
                        let wprop = sp.weight_prop.clone().unwrap_or_else(|| "weight".to_string());
                        crate::graph::dijkstra_shortest_path(&read_txn, start_id, end_id, sp.edge_type.as_deref(), &wprop, sp.direction.clone())?.map(|(n,e,_c)| (n,e))
                    },
                    crate::parser::PathAlgorithm::Bidirectional => crate::graph::bidirectional_bfs(&read_txn, start_id, end_id, sp.edge_type.as_deref())?,
                };
                if let Some((nodes, edges)) = path_opt {
                    let mut row = Row::new();
                    let pr = PathResult{ length: edges.len(), nodes, edges };
                    let alias = pq.returns.as_ref().and_then(|r| r.items.iter().find(|it| matches!(it.expr, ReturnExpr::ShortestPath(_))).and_then(|it| it.alias.clone())).unwrap_or_else(|| sp.path_var.clone().unwrap_or_else(|| "path".to_string()));
                    row.paths.insert(alias, pr);
                    rows.push(row);
                }
            }
        }
    } else if let Some(ret) = &pq.returns {
        for bind in &bindings {
            let mut row = Row::new();
            for item in &ret.items {
                match &item.expr {
                    ReturnExpr::Variable(var) => {
                        if let Some(id) = bind.get(var) {
                            if let Some(node) = fetch_node_read(&read_txn, *id)? {
                                let key = item.alias.clone().unwrap_or_else(|| var.clone());
                                row.nodes.insert(key, node);
                            } else if let Some(edge) = fetch_edge_read(&read_txn, *id)? {
                                let key = item.alias.clone().unwrap_or_else(|| var.clone());
                                row.edges.insert(key, edge);
                            }
                        }
                    },
                    ReturnExpr::Property{ var, prop } => {
                        if let Some(id) = bind.get(var) {
                            if let Some(node) = fetch_node_read(&read_txn, *id)? {
                                if let Some(v) = node.properties.get(prop) {
                                    let key = item.alias.clone().unwrap_or_else(|| format!("{}.{}", var, prop));
                                    row.values.insert(key, v.clone());
                                }
                            } else if let Some(edge) = fetch_edge_read(&read_txn, *id)? {
                                if let Some(v) = edge.properties.get(prop) {
                                    let key = item.alias.clone().unwrap_or_else(|| format!("{}.{}", var, prop));
                                    row.values.insert(key, v.clone());
                                }
                            }
                        }
                    },
                    ReturnExpr::Count(var) => {
                        let key = item.alias.clone().unwrap_or_else(|| format!("count({})", var));
                        row.values.insert(key, Value::Integer(1));
                    },
                    ReturnExpr::ShortestPath(_) => {}
                }
            }
            rows.push(row);
        }
        // distinct handling
        if ret.distinct {
            // dedup by values string representation
            let mut seen = std::collections::HashSet::new();
            let mut distinct_rows = vec![];
            for r in rows {
                let sig = format!("{:?}{:?}{:?}{:?}", r.values, r.nodes.keys().collect::<Vec<_>>(), r.edges.keys().collect::<Vec<_>>(), r.paths.keys().collect::<Vec<_>>());
                if seen.insert(sig) { distinct_rows.push(r); }
            }
            rows = distinct_rows;
        }
        // count aggregation
        if ret.items.iter().any(|i| matches!(i.expr, ReturnExpr::Count(_))) {
            let cnt = rows.len() as i64;
            let mut agg = Row::new();
            for item in &ret.items {
                if let ReturnExpr::Count(var) = &item.expr {
                    let key = item.alias.clone().unwrap_or_else(|| format!("count({})", var));
                    agg.values.insert(key, Value::Integer(cnt));
                } else {
                    // mixed count + other? ignore for MVP
                }
            }
            rows = if bindings.is_empty() && cnt==0 { vec![agg] } else if cnt>0 { vec![agg] } else { vec![] };
            if bindings.is_empty() {
                // if no bindings but count requested, return 0 count row
                if rows.is_empty() {
                    let mut agg2 = Row::new();
                    for item in &ret.items {
                        if let ReturnExpr::Count(var) = &item.expr {
                            let key = item.alias.clone().unwrap_or_else(|| format!("count({})", var));
                            agg2.values.insert(key, Value::Integer(0));
                        }
                    }
                    rows = vec![agg2];
                }
            }
        }
    } else {
        // No RETURN? Just return empty rows but keep affected
    }

    let affected = rows.len();
    Ok(ExecutionResult{ rows, affected })
}

// Helpers for read txn fetching
fn fetch_node_read(txn: &redb::ReadTransaction, id: u64) -> Result<Option<Node>> {
    let bytes_opt = {
        let tbl = txn.open_table(NODES_TABLE)?;
        tbl.get(id)?.map(|g| g.value().to_vec())
    };
    if let Some(bytes) = bytes_opt {
        let nd: NodeData = bincode::deserialize(&bytes).map_err(|e| GliteError::Serialization(e.to_string()))?;
        Ok(Some(Node{ id, labels: nd.labels, properties: nd.properties }))
    } else { Ok(None) }
}
fn fetch_edge_read(txn: &redb::ReadTransaction, id: u64) -> Result<Option<Edge>> {
    let bytes_opt = {
        let tbl = txn.open_table(EDGES_TABLE)?;
        tbl.get(id)?.map(|g| g.value().to_vec())
    };
    if let Some(bytes) = bytes_opt {
        let ed: EdgeData = bincode::deserialize(&bytes).map_err(|e| GliteError::Serialization(e.to_string()))?;
        Ok(Some(Edge{ id, src: ed.src, dst: ed.dst, edge_type: ed.edge_type, properties: ed.properties }))
    } else { Ok(None) }
}

// Find matches for read
fn find_matches_read(txn: &redb::ReadTransaction, pq: &ParsedQuery, params: &HashMap<String, Value>) -> Result<Vec<HashMap<String, u64>>> {
    if pq.matches.is_empty() && pq.shortest_path.is_none() {
        // No MATCH: maybe RETURN with no bindings => return single empty binding for constant returns
        if pq.returns.is_some() {
            return Ok(vec![HashMap::new()]);
        }
        return Ok(vec![]);
    }
    // If shortestPath assignment placeholder with empty paths, we need to handle separately: no real MATCH pattern, but sp defines start/end patterns?
    // For placeholder we have empty paths, so we will return single empty binding to let caller handle? but need start/end candidate enumeration.
    // We'll treat empty placeholder as no filter: return single empty binding, but graph will try to resolve start/end via params? Not correct.
    // Instead we will treat empty placeholder specially: if pq.matches has one empty entry, we ignore and try to find candidates for sp directly?
    // For MVP, if placeholder exists, we will attempt to generate bindings for start/end via scanning nodes matching implied labels? Since we don't have labels, fallback to generating all node pairs?
    // We'll just return vec with empty mapping for now, and caller will attempt BFS with missing vars => will error gracefully as no results.
    let mut all_bindings: Option<Vec<HashMap<String, u64>>> = None;

    for m in &pq.matches {
        if m.paths.is_empty() { continue; }
        for path in &m.paths {
            let path_bindings = find_bindings_for_path_read(txn, path, params, pq.where_clause.as_ref())?;
            // cross product with previous
            if let Some(existing) = all_bindings.take() {
                let mut new_combined = vec![];
                for eb in existing {
                    for pb in &path_bindings {
                        // check consistency: if overlapping var, ids must match
                        let mut merged = eb.clone();
                        let mut ok = true;
                        for (k,v) in pb {
                            if let Some(existing_v) = merged.get(k) {
                                if existing_v != v { ok=false; break; }
                            } else { merged.insert(k.clone(), *v); }
                        }
                        if ok { new_combined.push(merged); }
                    }
                }
                all_bindings = Some(new_combined);
            } else {
                all_bindings = Some(path_bindings);
            }
        }
    }

    let mut bindings = all_bindings.unwrap_or_default();

    // If no path bindings but we have where clause with variables, we may need to handle MATCH (a), (b) as separate paths already covered.
    // Apply WHERE filtering (already filtered per path, but cross-path WHERE may need global filter)
    if let Some(wc) = &pq.where_clause {
        bindings = bindings.into_iter().filter(|b| eval_where_read(txn, b, wc, params).unwrap_or(false)).collect();
    }

    // If still empty and pq.returns exists but matches was empty due to placeholder shortestPath assignment, we need to produce bindings for start/end separately?
    if bindings.is_empty() && pq.shortest_path.is_some() {
        // Try to find candidate bindings for start/end variables by scanning nodes (if we have no info, brute force all pairs? Limit to first few nodes to avoid explosion)
        // For now create single empty binding to allow graph to attempt? Will fail but return no rows.
        // To make shortestPath((a)-[*]->(b)) where a,b are variables without prior MATCH: we could enumerate all node pairs and try BFS? That's too many.
        // We'll leave empty.
    }

    // If no matches but RETURN expects variable without binding? Then bindings stays empty -> no rows.
    // If matches existed but path had no results, bindings empty is correct.

    // For queries like MATCH (n) RETURN n with no where, bindings should contain all nodes matched.
    Ok(bindings)
}

fn find_bindings_for_path_read(txn: &redb::ReadTransaction, path: &crate::parser::PatternPath, params: &HashMap<String, Value>, where_clause: Option<&crate::parser::WhereClause>) -> Result<Vec<HashMap<String, u64>>> {
    // path has nodes and edges interleaved: nodes.len() == edges.len()+1
    if path.nodes.is_empty() { return Ok(vec![]); }

    // Step 1: candidate nodes for first pattern node
    let first = &path.nodes[0];
    let candidates = candidate_nodes_read(txn, first, params)?;

    let mut results: Vec<HashMap<String, u64>> = vec![];
    for (nid, _node) in candidates {
        let mut binding = HashMap::new();
        if let Some(var) = &first.variable { binding.insert(var.clone(), nid); }

        // Check properties already filtered in candidate_nodes, but confirm

        // Traverse
        let mut current_node_id = nid;
        let mut current_binding = binding;
        let mut success = true;
        let mut path_ok = true;

        for edge_idx in 0..path.edges.len() {
            let edge_pat = &path.edges[edge_idx];
            let next_node_pat = &path.nodes[edge_idx+1];

            // Find edges from current_node_id matching edge_pat and leading to node matching next_node_pat
            let edges = match edge_pat.direction {
                Direction::Outgoing => {
                    get_out_edges_read(txn, current_node_id, edge_pat.edge_type.as_deref())?
                },
                Direction::Incoming => {
                    get_in_edges_read(txn, current_node_id, edge_pat.edge_type.as_deref())?
                },
                Direction::Undirected => {
                    let mut v = get_out_edges_read(txn, current_node_id, edge_pat.edge_type.as_deref())?;
                    v.extend(get_in_edges_read(txn, current_node_id, edge_pat.edge_type.as_deref())?);
                    v
                }
            };

            // For variable length edge [*], we need to handle BFS-like expansion rather than single hop.
            // For MVP, treat [*] as 1..* hops using BFS up to target matching. But for MATCH pattern with [*], we would need to find all nodes reachable via variable length.
            // Simplify: if is_variable_length, we do BFS to find all reachable nodes matching next_node_pat within some depth (e.g., up to 10 or unlimited until graph exhausted)
            // We'll handle variable length specially.
            if edge_pat.is_variable_length {
                // BFS from current_node_id to find all nodes matching next_node_pat with formalized bounds
                let min = edge_pat.var_len_min.unwrap_or(1);
                let max = edge_pat.var_len_max.unwrap_or(10);
                let reachable = bfs_variable_length(txn, current_node_id, edge_pat.edge_type.as_deref(), edge_pat.direction.clone(), next_node_pat, params, min, max)?;
                if reachable.is_empty() { path_ok = false; break; }
                // For MVP, variable length in MATCH returns multiple bindings (each reachable node)
                // We need to branch results: for each reachable (node_id, edge_path) create a new binding fork
                // But our current loop is linear; to handle branching we need to expand results vector
                // Instead, we will handle this case by producing multiple bindings directly and continue?
                // For simplicity, if variable length and there are multiple, we produce one binding per reachable and ignore further edges (if any)
                // We'll collect all and push to results directly, then mark success handled
                for (rid, edges_path) in reachable {
                    let mut nb = current_binding.clone();
                    if let Some(ev) = &edge_pat.variable {
                        // For variable length, edge variable represents path? We'll just bind first edge id
                        if let Some(first_e) = edges_path.first() { nb.insert(ev.clone(), first_e.id); }
                    }
                    if let Some(v) = &next_node_pat.variable {
                        nb.insert(v.clone(), rid);
                    }
                    // Check where filtering for this binding?
                    // Push
                    // If there are more edges beyond this variable edge, not handled
                    results.push(nb);
                }
                success = false; // signal that results already handled via branching, not linear
                path_ok = false; // to skip linear push
                break;
            } else {
                let mut matched = None;
                for edge in edges {
                    // Check edge properties
                    if !props_match(&edge.properties, &edge_pat.properties, params)? { continue; }
                    let target_id = if edge_pat.direction == Direction::Incoming { edge.src } else if edge_pat.direction == Direction::Outgoing { edge.dst } else {
                        // undirected: need to determine neighbor id
                        if edge.src == current_node_id { edge.dst } else { edge.src }
                    };
                    // Fetch target node
                    let target_node = fetch_node_read(txn, target_id)?.ok_or(GliteError::NotFound(format!("node {}", target_id)))?;
                    // Check labels and props of next node
                    if !labels_match(&target_node.labels, &next_node_pat.labels) { continue; }
                    if !props_match(&target_node.properties, &next_node_pat.properties, params)? { continue; }
                    matched = Some((edge, target_id, target_node));
                    break; // take first match for this branch (but there could be multiple)
                    // For full results we should branch for each edge
                }
                if let Some((edge, tid, _tnode)) = matched {
                    if let Some(ev) = &edge_pat.variable { current_binding.insert(ev.clone(), edge.id); }
                    if let Some(v) = &next_node_pat.variable { current_binding.insert(v.clone(), tid); }
                    current_node_id = tid;
                } else {
                    path_ok = false;
                    break;
                }
            }
        }

        if success && path_ok {
            // Apply where filter per binding if needed? We'll filter later globally but also per path
            if let Some(wc) = where_clause {
                if !eval_where_read(txn, &current_binding, wc, params)? { continue; }
            }
            results.push(current_binding);
        } else if !success {
            // already pushed variable length results
        }
    }

    // For patterns where we branched for multiple edges per node, we currently only took first match (not all combinations). To fully support, we need to enumerate all combinations, which would require recursive branching.
    // For  MVP, single match per start node is okay but may miss results. We'll attempt to handle multiple matches via recursion for simple case of single edge.
    // To properly handle multiple matches, we should replace above loop with recursive enumeration.
    // Given complexity, we will keep first-match semantics but improve by enumerating all possibilities via helper function for general case.

    // If path has one edge, we can enumerate all candidates: we already have results for variable length, but for non-variable we only took first.
    // To improve, we can post-process to expand all edge possibilities when path length ==1 and we have multiple edges per start.
    // We'll leave as is for now; future fix can add full enumeration.

    // If path length is 1 edge, to enumerate all, we need to redo with branching:
    if path.edges.len()==1 && !path.edges[0].is_variable_length && results.len() > 0 {
        // Check if we missed additional edge matches per start node
        // We'll do full enumeration by re-running candidate expansion that generates all combos
        let mut full_results = vec![];
        let first = &path.nodes[0];
        let edge_pat = &path.edges[0];
        let next_pat = &path.nodes[1];
        let cands = candidate_nodes_read(txn, first, params)?;
        for (nid, _) in cands {
            let edges = match edge_pat.direction {
                Direction::Outgoing => get_out_edges_read(txn, nid, edge_pat.edge_type.as_deref())?,
                Direction::Incoming => get_in_edges_read(txn, nid, edge_pat.edge_type.as_deref())?,
                Direction::Undirected => {
                    let mut v = get_out_edges_read(txn, nid, edge_pat.edge_type.as_deref())?;
                    v.extend(get_in_edges_read(txn, nid, edge_pat.edge_type.as_deref())?);
                    v
                }
            };
            for edge in edges {
                if !props_match(&edge.properties, &edge_pat.properties, params)? { continue; }
                let tid = if edge_pat.direction == Direction::Incoming { edge.src } else if edge_pat.direction == Direction::Outgoing { edge.dst } else { if edge.src==nid { edge.dst } else { edge.src } };
                let tnode = match fetch_node_read(txn, tid)? { Some(n)=>n, None=>continue };
                if !labels_match(&tnode.labels, &next_pat.labels) { continue; }
                if !props_match(&tnode.properties, &next_pat.properties, params)? { continue; }
                let mut b = HashMap::new();
                if let Some(v) = &first.variable { b.insert(v.clone(), nid); }
                if let Some(ev) = &edge_pat.variable { b.insert(ev.clone(), edge.id); }
                if let Some(v2) = &next_pat.variable { b.insert(v2.clone(), tid); }
                if let Some(wc) = where_clause { if !eval_where_read(txn, &b, wc, params)? { continue; } }
                full_results.push(b);
            }
        }
        if !full_results.is_empty() { return Ok(full_results); }
    }

    Ok(results)
}

fn candidate_nodes_read(txn: &redb::ReadTransaction, pat: &crate::parser::PatternNode, params: &HashMap<String, Value>) -> Result<Vec<(u64, Node)>> {
    let mut res = vec![];
    if !pat.labels.is_empty() {
        let label = &pat.labels[0];
        let ids: Vec<u64> = {
            let tbl = txn.open_table(LABEL_INDEX_TABLE)?;
            let mut ids = vec![];
            let prefix = storage::encode_label_prefix(label);
            let end = storage::prefix_end(&prefix);
            let iter: Box<dyn Iterator<Item=std::result::Result<_, redb::StorageError>> + '_> = if end.is_empty() {
                Box::new(tbl.range(prefix.as_slice()..)?)
            } else {
                Box::new(tbl.range(prefix.as_slice()..end.as_slice())?)
            };
            for entry in iter {
                let (_k, v) = entry?;
                ids.push(v.value());
            }
            ids
        };
        for nid in ids {
            if let Some(node) = fetch_node_read(txn, nid)? {
                if !labels_match(&node.labels, &pat.labels) { continue; }
                if !props_match(&node.properties, &pat.properties, params)? { continue; }
                res.push((nid, node));
            }
        }
    } else {
        let entries: Vec<(u64, Vec<u8>)> = {
            let tbl = txn.open_table(NODES_TABLE)?;
            let mut v = vec![];
            for entry in tbl.iter()? {
                let (k, val) = entry?;
                v.push((k.value(), val.value().to_vec()));
            }
            v
        };
        for (nid, bytes) in entries {
            let nd: NodeData = bincode::deserialize(&bytes).map_err(|e| GliteError::Serialization(e.to_string()))?;
            let node = Node{ id: nid, labels: nd.labels.clone(), properties: nd.properties.clone() };
            if !labels_match(&node.labels, &pat.labels) { continue; }
            if !props_match(&node.properties, &pat.properties, params)? { continue; }
            res.push((nid, node));
        }
    }
    Ok(res)
}

fn labels_match(node_labels: &[String], pat_labels: &[String]) -> bool {
    for pl in pat_labels {
        if !node_labels.contains(pl) { return false; }
    }
    true
}

fn props_match(node_props: &HashMap<String, Value>, pat_props: &HashMap<String, crate::parser::Expr>, params: &HashMap<String, Value>) -> Result<bool> {
    for (k, expr) in pat_props {
        let expected = eval_expr(expr, params)?;
        match node_props.get(k) {
            Some(v) => if v != &expected { return Ok(false); },
            None => return Ok(false),
        }
    }
    Ok(true)
}

fn get_out_edges_read(txn: &redb::ReadTransaction, src: u64, typ: Option<&str>) -> Result<Vec<Edge>> {
    let ids: Vec<u64> = {
        let tbl = txn.open_table(OUT_EDGES_TABLE)?;
        let mut ids = vec![];
        if let Some(filter) = typ {
            let prefix = storage::encode_out_prefix_typed(src, filter);
            let end = storage::prefix_end(&prefix);
            let iter: Box<dyn Iterator<Item=std::result::Result<_, redb::StorageError>> + '_> = if end.is_empty() {
                Box::new(tbl.range(prefix.as_slice()..)?)
            } else {
                Box::new(tbl.range(prefix.as_slice()..end.as_slice())?)
            };
            for entry in iter { let (_, v) = entry?; ids.push(v.value()); }
        } else {
            let prefix = storage::encode_out_prefix(src);
            let end = storage::prefix_end(&prefix);
            let iter: Box<dyn Iterator<Item=std::result::Result<_, redb::StorageError>> + '_> = if end.is_empty() {
                Box::new(tbl.range(prefix.as_slice()..)?)
            } else {
                Box::new(tbl.range(prefix.as_slice()..end.as_slice())?)
            };
            for entry in iter { let (_, v) = entry?; ids.push(v.value()); }
        }
        ids
    };
    let mut res = vec![];
    for eid in ids { if let Some(e)=fetch_edge_read(txn, eid)? { res.push(e); } }
    Ok(res)
}
fn get_in_edges_read(txn: &redb::ReadTransaction, dst: u64, typ: Option<&str>) -> Result<Vec<Edge>> {
    let ids: Vec<u64> = {
        let tbl = txn.open_table(IN_EDGES_TABLE)?;
        let mut ids = vec![];
        if let Some(filter) = typ {
            let prefix = storage::encode_in_prefix_typed(dst, filter);
            let end = storage::prefix_end(&prefix);
            let iter: Box<dyn Iterator<Item=std::result::Result<_, redb::StorageError>> + '_> = if end.is_empty() {
                Box::new(tbl.range(prefix.as_slice()..)?)
            } else {
                Box::new(tbl.range(prefix.as_slice()..end.as_slice())?)
            };
            for entry in iter { let (_, v) = entry?; ids.push(v.value()); }
        } else {
            let prefix = storage::encode_in_prefix(dst);
            let end = storage::prefix_end(&prefix);
            let iter: Box<dyn Iterator<Item=std::result::Result<_, redb::StorageError>> + '_> = if end.is_empty() {
                Box::new(tbl.range(prefix.as_slice()..)?)
            } else {
                Box::new(tbl.range(prefix.as_slice()..end.as_slice())?)
            };
            for entry in iter { let (_, v) = entry?; ids.push(v.value()); }
        }
        ids
    };
    let mut res = vec![];
    for eid in ids { if let Some(e)=fetch_edge_read(txn, eid)? { res.push(e); } }
    Ok(res)
}

fn eval_where_read(txn: &redb::ReadTransaction, binding: &HashMap<String, u64>, wc: &crate::parser::WhereClause, params: &HashMap<String, Value>) -> Result<bool> {
    for cond in &wc.conditions {
        let left_id = binding.get(&cond.left_var).ok_or(GliteError::Execution(format!("where var {} not bound", cond.left_var)))?;
        // Need to fetch property value for left
        let left_val = {
            if let Some(node) = fetch_node_read(txn, *left_id)? {
                node.properties.get(&cond.left_prop).cloned()
            } else if let Some(edge) = fetch_edge_read(txn, *left_id)? {
                edge.properties.get(&cond.left_prop).cloned()
            } else { None }
        };
        let right_val = match &cond.right {
            crate::parser::ExprOrProp::Expr(e) => Some(eval_expr(e, params)?),
            crate::parser::ExprOrProp::Prop{ var, prop } => {
                let rid = binding.get(var).ok_or(GliteError::Execution(format!("where var {} not bound", var)))?;
                if let Some(node) = fetch_node_read(txn, *rid)? {
                    node.properties.get(prop).cloned()
                } else if let Some(edge) = fetch_edge_read(txn, *rid)? {
                    edge.properties.get(prop).cloned()
                } else { None }
            }
        };
        let cmp = compare_values(left_val.as_ref(), right_val.as_ref(), &cond.op);
        if !cmp { return Ok(false); }
    }
    Ok(true)
}

fn compare_values(left: Option<&Value>, right: Option<&Value>, op: &crate::parser::Operator) -> bool {
    match (left, right) {
        (Some(Value::Null), Some(Value::Null)) => matches!(op, crate::parser::Operator::Eq | crate::parser::Operator::Gte | crate::parser::Operator::Lte),
        (Some(Value::Null), _) | (_, Some(Value::Null)) => {
            match op {
                crate::parser::Operator::Eq => false,
                crate::parser::Operator::NotEq => true,
                _ => false,
            }
        },
        (Some(l), Some(r)) => {
            // Handle NaN: any comparison with NaN is false except NotEq true
            if let (Value::Float(af), Value::Float(bf)) = (l, r) {
                if af.is_nan() || bf.is_nan() {
                    return match op {
                        crate::parser::Operator::NotEq => true,
                        crate::parser::Operator::Eq => false,
                        _ => false,
                    };
                }
            }
            match op {
                crate::parser::Operator::Eq => l==r,
                crate::parser::Operator::NotEq => l!=r,
                crate::parser::Operator::Gt => cmp_order(l,r) == Some(std::cmp::Ordering::Greater),
                crate::parser::Operator::Lt => cmp_order(l,r) == Some(std::cmp::Ordering::Less),
                crate::parser::Operator::Gte => {
                    let o = cmp_order(l,r);
                    o == Some(std::cmp::Ordering::Greater) || o == Some(std::cmp::Ordering::Equal)
                },
                crate::parser::Operator::Lte => {
                    let o = cmp_order(l,r);
                    o == Some(std::cmp::Ordering::Less) || o == Some(std::cmp::Ordering::Equal)
                },
            }
        },
        _ => false,
    }
}

fn cmp_order(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a,b) {
        (Value::Integer(ai), Value::Integer(bi)) => Some(ai.cmp(bi)),
        (Value::Float(af), Value::Float(bf)) => {
            if af.is_nan() || bf.is_nan() { None } else { af.partial_cmp(bf) }
        },
        (Value::Integer(ai), Value::Float(bf)) => {
            if bf.is_nan() { None } else { (*ai as f64).partial_cmp(bf) }
        },
        (Value::Float(af), Value::Integer(bi)) => {
            if af.is_nan() { None } else { af.partial_cmp(&(*bi as f64)) }
        },
        (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
        (Value::Boolean(a), Value::Boolean(b)) => Some(a.cmp(b)),
        _ => None,
    }
}

// BFS for variable length [*] with formalized bounds
fn bfs_variable_length(txn: &redb::ReadTransaction, start: u64, edge_type: Option<&str>, direction: Direction, target_pat: &crate::parser::PatternNode, params: &HashMap<String, Value>, min: usize, max: usize) -> Result<Vec<(u64, Vec<Edge>)>> {
    let mut visited = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::new();
    visited.insert(start);
    queue.push_back((start, vec![]));
    let mut results = vec![];
    while let Some((cur, path)) = queue.pop_front() {
        for (nbr, edge) in match direction {
            Direction::Outgoing => get_out_edges_read(txn, cur, edge_type)?.into_iter().map(|e| (e.dst, e)).collect::<Vec<_>>(),
            Direction::Incoming => get_in_edges_read(txn, cur, edge_type)?.into_iter().map(|e| (e.src, e)).collect::<Vec<_>>(),
            Direction::Undirected => {
                let mut v = get_out_edges_read(txn, cur, edge_type)?.into_iter().map(|e| (e.dst, e)).collect::<Vec<_>>();
                v.extend(get_in_edges_read(txn, cur, edge_type)?.into_iter().map(|e| (e.src, e)).collect::<Vec<_>>());
                v
            }
        } {
            if visited.contains(&nbr) { continue; }
            let mut new_path = path.clone();
            new_path.push(edge);
            let len = new_path.len();
            // Check if nbr matches target pattern and length within bounds
            if len >= min && len <= max {
                if let Some(node) = fetch_node_read(txn, nbr)? {
                    if labels_match(&node.labels, &target_pat.labels) && props_match(&node.properties, &target_pat.properties, params)? {
                        results.push((nbr, new_path.clone()));
                    }
                }
            } else if len < min {
                // still need to check? If len < min, don't push even if matches, but continue expanding
                // No push
            }
            visited.insert(nbr);
            if len < max {
                queue.push_back((nbr, new_path));
            }
        }
    }
    Ok(results)
}

// Write txn variants (simplified duplication for write path)
fn find_matches_write(txn: &redb::WriteTransaction, pq: &ParsedQuery, params: &HashMap<String, Value>) -> Result<Vec<HashMap<String, u64>>> {
    // Use snapshot via opening read views on write txn? redb WriteTransaction can open tables for reading as well.
    // We'll duplicate logic but using WriteTransaction tables
    // For simplicity, we will collect bindings by scanning write txn tables directly

    if pq.matches.is_empty() && pq.shortest_path.is_none() {
        return Ok(vec![]);
    }
    let mut all_bindings: Option<Vec<HashMap<String,u64>>> = None;
    for m in &pq.matches {
        if m.paths.is_empty() { continue; }
        for path in &m.paths {
            let b = find_bindings_for_path_write(txn, path, params, pq.where_clause.as_ref())?;
            if let Some(existing) = all_bindings.take() {
                let mut new_comb = vec![];
                for eb in existing {
                    for pb in &b {
                        let mut merged = eb.clone();
                        let mut ok=true;
                        for (k,v) in pb { if let Some(ev)=merged.get(k) { if ev!=v { ok=false; break; } } else { merged.insert(k.clone(), *v); } }
                        if ok { new_comb.push(merged); }
                    }
                }
                all_bindings = Some(new_comb);
            } else { all_bindings = Some(b); }
        }
    }
    let mut bindings = all_bindings.unwrap_or_default();
    if let Some(wc) = &pq.where_clause {
        bindings = bindings.into_iter().filter(|b| eval_where_write(txn, b, wc, params).unwrap_or(false)).collect();
    }
    Ok(bindings)
}

fn find_bindings_for_path_write(txn: &redb::WriteTransaction, path: &crate::parser::PatternPath, params: &HashMap<String, Value>, where_clause: Option<&crate::parser::WhereClause>) -> Result<Vec<HashMap<String,u64>>> {
    if path.nodes.is_empty() { return Ok(vec![]); }
    let first = &path.nodes[0];
    let cands = candidate_nodes_write(txn, first, params)?;
    let mut results = vec![];
    for (nid, _) in cands {
        let mut binding = HashMap::new();
        if let Some(v)=&first.variable { binding.insert(v.clone(), nid); }
        let mut cur_id = nid;
        let mut cur_bind = binding;
        let mut ok = true;
        for idx in 0..path.edges.len() {
            let epat = &path.edges[idx];
            let npat = &path.nodes[idx+1];
            let edges = match epat.direction {
                Direction::Outgoing => get_out_edges_write(txn, cur_id, epat.edge_type.as_deref())?,
                Direction::Incoming => get_in_edges_write(txn, cur_id, epat.edge_type.as_deref())?,
                Direction::Undirected => {
                    let mut v = get_out_edges_write(txn, cur_id, epat.edge_type.as_deref())?;
                    v.extend(get_in_edges_write(txn, cur_id, epat.edge_type.as_deref())?);
                    v
                }
            };
            if epat.is_variable_length {
                // variable length not supported in write path for SET/DELETE; skip
                ok=false; break;
            }
            let mut matched = None;
            for e in edges {
                if !props_match(&e.properties, &epat.properties, params)? { continue; }
                let tid = if epat.direction==Direction::Incoming { e.src } else if epat.direction==Direction::Outgoing { e.dst } else { if e.src==cur_id { e.dst } else { e.src } };
                let tnode = fetch_node_write(txn, tid)?.ok_or(GliteError::NotFound(format!("node {}", tid)))?;
                if !labels_match(&tnode.labels, &npat.labels) { continue; }
                if !props_match(&tnode.properties, &npat.properties, params)? { continue; }
                matched = Some((e, tid));
                break;
            }
            if let Some((e, tid)) = matched {
                if let Some(ev)=&epat.variable { cur_bind.insert(ev.clone(), e.id); }
                if let Some(v)=&npat.variable { cur_bind.insert(v.clone(), tid); }
                cur_id = tid;
            } else { ok=false; break; }
        }
        if ok {
            if let Some(wc)=where_clause { if !eval_where_write(txn, &cur_bind, wc, params)? { continue; } }
            results.push(cur_bind);
        }
    }
    // Full enumeration for single edge case as before
    if path.edges.len()==1 && !path.edges[0].is_variable_length {
        let mut full = vec![];
        let first = &path.nodes[0];
        let epat = &path.edges[0];
        let npat = &path.nodes[1];
        let cands = candidate_nodes_write(txn, first, params)?;
        for (nid, _) in cands {
            let edges = match epat.direction {
                Direction::Outgoing => get_out_edges_write(txn, nid, epat.edge_type.as_deref())?,
                Direction::Incoming => get_in_edges_write(txn, nid, epat.edge_type.as_deref())?,
                Direction::Undirected => {
                    let mut v = get_out_edges_write(txn, nid, epat.edge_type.as_deref())?;
                    v.extend(get_in_edges_write(txn, nid, epat.edge_type.as_deref())?);
                    v
                }
            };
            for e in edges {
                if !props_match(&e.properties, &epat.properties, params)? { continue; }
                let tid = if epat.direction==Direction::Incoming { e.src } else if epat.direction==Direction::Outgoing { e.dst } else { if e.src==nid { e.dst } else { e.src } };
                let tnode = match fetch_node_write(txn, tid)? { Some(n)=>n, None=>continue };
                if !labels_match(&tnode.labels, &npat.labels) { continue; }
                if !props_match(&tnode.properties, &npat.properties, params)? { continue; }
                let mut b = HashMap::new();
                if let Some(v)=&first.variable { b.insert(v.clone(), nid); }
                if let Some(ev)=&epat.variable { b.insert(ev.clone(), e.id); }
                if let Some(v2)=&npat.variable { b.insert(v2.clone(), tid); }
                if let Some(wc)=where_clause { if !eval_where_write(txn, &b, wc, params)? { continue; } }
                full.push(b);
            }
        }
        if !full.is_empty() { return Ok(full); }
    }
    Ok(results)
}

fn candidate_nodes_write(txn: &redb::WriteTransaction, pat: &crate::parser::PatternNode, params: &HashMap<String, Value>) -> Result<Vec<(u64, Node)>> {
    let mut res = vec![];
    if !pat.labels.is_empty() {
        let ids: Vec<u64> = {
            let tbl = txn.open_table(LABEL_INDEX_TABLE)?;
            let mut ids = vec![];
            let prefix = storage::encode_label_prefix(&pat.labels[0]);
            let end = storage::prefix_end(&prefix);
            let iter: Box<dyn Iterator<Item=std::result::Result<_, redb::StorageError>> + '_> = if end.is_empty() {
                Box::new(tbl.range(prefix.as_slice()..)?)
            } else {
                Box::new(tbl.range(prefix.as_slice()..end.as_slice())?)
            };
            for entry in iter {
                let (_, v) = entry?;
                ids.push(v.value());
            }
            ids
        };
        for nid in ids {
            if let Some(node)=fetch_node_write(txn, nid)? {
                if !labels_match(&node.labels, &pat.labels) { continue; }
                if !props_match(&node.properties, &pat.properties, params)? { continue; }
                res.push((nid, node));
            }
        }
    } else {
        let entries: Vec<(u64, Vec<u8>)> = {
            let tbl = txn.open_table(NODES_TABLE)?;
            let mut v = vec![];
            for entry in tbl.iter()? {
                let (k, val)=entry?;
                v.push((k.value(), val.value().to_vec()));
            }
            v
        };
        for (nid, bytes) in entries {
            let nd: NodeData = bincode::deserialize(&bytes).map_err(|e| GliteError::Serialization(e.to_string()))?;
            let node = Node{ id: nid, labels: nd.labels.clone(), properties: nd.properties.clone() };
            if !labels_match(&node.labels, &pat.labels) { continue; }
            if !props_match(&node.properties, &pat.properties, params)? { continue; }
            res.push((nid, node));
        }
    }
    Ok(res)
}

fn fetch_node_write(txn: &redb::WriteTransaction, id: u64) -> Result<Option<Node>> {
    let bytes_opt = {
        let tbl = txn.open_table(NODES_TABLE)?;
        let opt = tbl.get(id)?;
        let x = opt.map(|g| g.value().to_vec());
        x
    };
    if let Some(bytes) = bytes_opt {
        let nd: NodeData = bincode::deserialize(&bytes).map_err(|e| GliteError::Serialization(e.to_string()))?;
        Ok(Some(Node{ id, labels: nd.labels, properties: nd.properties }))
    } else { Ok(None) }
}
fn fetch_edge_write(txn: &redb::WriteTransaction, id: u64) -> Result<Option<Edge>> {
    let bytes_opt = {
        let tbl = txn.open_table(EDGES_TABLE)?;
        let opt = tbl.get(id)?;
        let x = opt.map(|g| g.value().to_vec());
        x
    };
    if let Some(bytes) = bytes_opt {
        let ed: EdgeData = bincode::deserialize(&bytes).map_err(|e| GliteError::Serialization(e.to_string()))?;
        Ok(Some(Edge{ id, src: ed.src, dst: ed.dst, edge_type: ed.edge_type, properties: ed.properties }))
    } else { Ok(None) }
}
fn get_out_edges_write(txn: &redb::WriteTransaction, src: u64, typ: Option<&str>) -> Result<Vec<Edge>> {
    let ids: Vec<u64> = {
        let tbl = txn.open_table(OUT_EDGES_TABLE)?;
        let mut ids = vec![];
        if let Some(f)=typ {
            let prefix = storage::encode_out_prefix_typed(src, f);
            let end = storage::prefix_end(&prefix);
            let iter: Box<dyn Iterator<Item=std::result::Result<_, redb::StorageError>> + '_> = if end.is_empty() {
                Box::new(tbl.range(prefix.as_slice()..)?)
            } else {
                Box::new(tbl.range(prefix.as_slice()..end.as_slice())?)
            };
            for entry in iter { let (_, v)=entry?; ids.push(v.value()); }
        } else {
            let prefix = storage::encode_out_prefix(src);
            let end = storage::prefix_end(&prefix);
            let iter: Box<dyn Iterator<Item=std::result::Result<_, redb::StorageError>> + '_> = if end.is_empty() {
                Box::new(tbl.range(prefix.as_slice()..)?)
            } else {
                Box::new(tbl.range(prefix.as_slice()..end.as_slice())?)
            };
            for entry in iter { let (_, v)=entry?; ids.push(v.value()); }
        }
        ids
    };
    let mut res=vec![];
    for eid in ids { if let Some(e)=fetch_edge_write(txn, eid)? { res.push(e); } }
    Ok(res)
}
fn get_in_edges_write(txn: &redb::WriteTransaction, dst: u64, typ: Option<&str>) -> Result<Vec<Edge>> {
    let ids: Vec<u64> = {
        let tbl = txn.open_table(IN_EDGES_TABLE)?;
        let mut ids = vec![];
        if let Some(f)=typ {
            let prefix = storage::encode_in_prefix_typed(dst, f);
            let end = storage::prefix_end(&prefix);
            let iter: Box<dyn Iterator<Item=std::result::Result<_, redb::StorageError>> + '_> = if end.is_empty() {
                Box::new(tbl.range(prefix.as_slice()..)?)
            } else {
                Box::new(tbl.range(prefix.as_slice()..end.as_slice())?)
            };
            for entry in iter { let (_, v)=entry?; ids.push(v.value()); }
        } else {
            let prefix = storage::encode_in_prefix(dst);
            let end = storage::prefix_end(&prefix);
            let iter: Box<dyn Iterator<Item=std::result::Result<_, redb::StorageError>> + '_> = if end.is_empty() {
                Box::new(tbl.range(prefix.as_slice()..)?)
            } else {
                Box::new(tbl.range(prefix.as_slice()..end.as_slice())?)
            };
            for entry in iter { let (_, v)=entry?; ids.push(v.value()); }
        }
        ids
    };
    let mut res=vec![];
    for eid in ids { if let Some(e)=fetch_edge_write(txn, eid)? { res.push(e); } }
    Ok(res)
}

fn eval_where_write(txn: &redb::WriteTransaction, binding: &HashMap<String,u64>, wc: &crate::parser::WhereClause, params: &HashMap<String, Value>) -> Result<bool> {
    for cond in &wc.conditions {
        let left_id = binding.get(&cond.left_var).ok_or(GliteError::Execution(format!("where var {} not bound", cond.left_var)))?;
        let left_val = {
            if let Some(n)=fetch_node_write(txn, *left_id)? { n.properties.get(&cond.left_prop).cloned() }
            else if let Some(e)=fetch_edge_write(txn, *left_id)? { e.properties.get(&cond.left_prop).cloned() }
            else { None }
        };
        let right_val = match &cond.right {
            crate::parser::ExprOrProp::Expr(e)=> Some(eval_expr(e, params)?),
            crate::parser::ExprOrProp::Prop{ var, prop } => {
                let rid = binding.get(var).ok_or(GliteError::Execution(format!("where var {} not bound", var)))?;
                if let Some(n)=fetch_node_write(txn, *rid)? { n.properties.get(prop).cloned() }
                else if let Some(e)=fetch_edge_write(txn, *rid)? { e.properties.get(prop).cloned() }
                else { None }
            }
        };
        if !compare_values(left_val.as_ref(), right_val.as_ref(), &cond.op) { return Ok(false); }
    }
    Ok(true)
}

fn bfs_write(txn: &redb::WriteTransaction, start: u64, end: u64, edge_type: Option<&str>, direction: Direction) -> Result<Option<(Vec<Node>, Vec<Edge>)>> {
    use std::collections::{VecDeque, HashSet, HashMap};
    if start==end {
        let n = fetch_node_write(txn, start)?.ok_or(GliteError::NotFound(format!("node {}", start)))?;
        return Ok(Some((vec![n], vec![])));
    }
    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();
    let mut parent: HashMap<u64, (u64, Edge)> = HashMap::new();
    visited.insert(start);
    queue.push_back(start);
    while let Some(cur) = queue.pop_front() {
        let neighbors: Vec<(u64, Edge)> = match direction {
            Direction::Outgoing => get_out_edges_write(txn, cur, edge_type)?.into_iter().map(|e| (e.dst, e)).collect(),
            Direction::Incoming => get_in_edges_write(txn, cur, edge_type)?.into_iter().map(|e| (e.src, e)).collect(),
            Direction::Undirected => {
                let mut v = get_out_edges_write(txn, cur, edge_type)?.into_iter().map(|e| (e.dst, e)).collect::<Vec<_>>();
                v.extend(get_in_edges_write(txn, cur, edge_type)?.into_iter().map(|e| (e.src, e)).collect::<Vec<_>>());
                v
            }
        };
        for (nbr, edge) in neighbors {
            if visited.contains(&nbr) { continue; }
            visited.insert(nbr);
            parent.insert(nbr, (cur, edge.clone()));
            if nbr==end {
                let mut nodes_rev = vec![fetch_node_write(txn, nbr)?.unwrap()];
                let mut edges_rev = vec![];
                let mut cur_id = nbr;
                while cur_id != start {
                    let (prev, e) = parent.get(&cur_id).unwrap().clone();
                    edges_rev.push(e);
                    cur_id = prev;
                    nodes_rev.push(fetch_node_write(txn, cur_id)?.unwrap());
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
