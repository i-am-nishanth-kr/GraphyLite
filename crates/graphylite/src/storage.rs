use crate::error::{GliteError, Result};
use crate::types::{EdgeData, NodeData};
use redb::{Database, TableDefinition, ReadableTable};
use std::path::Path;
use std::sync::Arc;

// Table definitions - using &[u8] keys and values for flexibility
pub const NODES_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("nodes");
pub const EDGES_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("edges");
pub const OUT_EDGES_TABLE: TableDefinition<&[u8], u64> = TableDefinition::new("out_edges");
pub const IN_EDGES_TABLE: TableDefinition<&[u8], u64> = TableDefinition::new("in_edges");
pub const LABEL_INDEX_TABLE: TableDefinition<&[u8], u64> = TableDefinition::new("label_index");
pub const META_TABLE: TableDefinition<&str, u64> = TableDefinition::new("meta");

pub fn open_db(path: impl AsRef<Path>) -> Result<Arc<Database>> {
    let db = Database::create(path)?;
    init_tables(&db)?;
    Ok(Arc::new(db))
}

pub fn open_in_memory() -> Result<Arc<Database>> {
    let db = Database::builder().create_with_backend(redb::backends::InMemoryBackend::new())?;
    init_tables(&db)?;
    Ok(Arc::new(db))
}

fn init_tables(db: &Database) -> Result<()> {
    let write_txn = db.begin_write()?;
    {
        let _ = write_txn.open_table(NODES_TABLE)?;
        let _ = write_txn.open_table(EDGES_TABLE)?;
        let _ = write_txn.open_table(OUT_EDGES_TABLE)?;
        let _ = write_txn.open_table(IN_EDGES_TABLE)?;
        let _ = write_txn.open_table(LABEL_INDEX_TABLE)?;
        let mut meta = write_txn.open_table(META_TABLE)?;
        if meta.get("next_node_id")?.is_none() {
            meta.insert("next_node_id", 1u64)?;
        }
        if meta.get("next_edge_id")?.is_none() {
            meta.insert("next_edge_id", 1_000_000_000u64)?;
        }
    }
    write_txn.commit()?;
    Ok(())
}

// Encoding helpers
pub fn encode_out_key(src: u64, typ: &str, dst: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + 4 + typ.len() + 8);
    v.extend_from_slice(&src.to_be_bytes());
    v.extend_from_slice(&(typ.len() as u32).to_be_bytes());
    v.extend_from_slice(typ.as_bytes());
    v.extend_from_slice(&dst.to_be_bytes());
    v
}

pub fn encode_in_key(dst: u64, typ: &str, src: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + 4 + typ.len() + 8);
    v.extend_from_slice(&dst.to_be_bytes());
    v.extend_from_slice(&(typ.len() as u32).to_be_bytes());
    v.extend_from_slice(typ.as_bytes());
    v.extend_from_slice(&src.to_be_bytes());
    v
}

pub fn decode_out_key(bytes: &[u8]) -> Result<(u64, String, u64)> {
    if bytes.len() < 8+4+8 { return Err(GliteError::Serialization("out_key too short".into())) }
    let src = u64::from_be_bytes(bytes[0..8].try_into().unwrap());
    let typ_len = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
    if bytes.len() < 8+4+typ_len+8 { return Err(GliteError::Serialization("out_key typ_len mismatch".into())) }
    let typ = String::from_utf8(bytes[12..12+typ_len].to_vec()).map_err(|e| GliteError::Serialization(e.to_string()))?;
    let dst = u64::from_be_bytes(bytes[12+typ_len..12+typ_len+8].try_into().unwrap());
    Ok((src, typ, dst))
}

pub fn encode_label_key(label: &str, node_id: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + label.len() + 8);
    v.extend_from_slice(&(label.len() as u32).to_be_bytes());
    v.extend_from_slice(label.as_bytes());
    v.extend_from_slice(&node_id.to_be_bytes());
    v
}

pub fn decode_label_key(bytes: &[u8]) -> Result<(String, u64)> {
    if bytes.len() < 4+8 { return Err(GliteError::Serialization("label_key too short".into())) }
    let len = u32::from_be_bytes(bytes[0..4].try_into().unwrap()) as usize;
    if bytes.len() < 4+len+8 { return Err(GliteError::Serialization("label_key len mismatch".into())) }
    let label = String::from_utf8(bytes[4..4+len].to_vec()).map_err(|e| GliteError::Serialization(e.to_string()))?;
    let node_id = u64::from_be_bytes(bytes[4+len..4+len+8].try_into().unwrap());
    Ok((label, node_id))
}

// Prefix range helpers for efficient scans (R5)
pub fn encode_label_prefix(label: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + label.len());
    v.extend_from_slice(&(label.len() as u32).to_be_bytes());
    v.extend_from_slice(label.as_bytes());
    v
}

pub fn encode_out_prefix(src: u64) -> Vec<u8> {
    src.to_be_bytes().to_vec()
}

pub fn encode_out_prefix_typed(src: u64, typ: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + 4 + typ.len());
    v.extend_from_slice(&src.to_be_bytes());
    v.extend_from_slice(&(typ.len() as u32).to_be_bytes());
    v.extend_from_slice(typ.as_bytes());
    v
}

pub fn encode_in_prefix(dst: u64) -> Vec<u8> {
    dst.to_be_bytes().to_vec()
}

pub fn encode_in_prefix_typed(dst: u64, typ: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + 4 + typ.len());
    v.extend_from_slice(&dst.to_be_bytes());
    v.extend_from_slice(&(typ.len() as u32).to_be_bytes());
    v.extend_from_slice(typ.as_bytes());
    v
}

pub fn prefix_end(prefix: &[u8]) -> Vec<u8> {
    // Compute exclusive upper bound by incrementing prefix as big-endian bytes
    let mut end = prefix.to_vec();
    for i in (0..end.len()).rev() {
        if end[i] != 0xFF {
            end[i] += 1;
            end.truncate(i+1);
            return end;
        }
    }
    // If all 0xFF, return empty vec signalling no upper bound; caller should fallback to full scan
    Vec::new()
}

pub fn serialize_node(data: &NodeData) -> Result<Vec<u8>> {
    bincode::serialize(data).map_err(|e| GliteError::Serialization(e.to_string()))
}
pub fn deserialize_node(bytes: &[u8]) -> Result<NodeData> {
    bincode::deserialize(bytes).map_err(|e| GliteError::Serialization(e.to_string()))
}
pub fn serialize_edge(data: &EdgeData) -> Result<Vec<u8>> {
    bincode::serialize(data).map_err(|e| GliteError::Serialization(e.to_string()))
}
pub fn deserialize_edge(bytes: &[u8]) -> Result<EdgeData> {
    bincode::deserialize(bytes).map_err(|e| GliteError::Serialization(e.to_string()))
}

// Helper to get next id atomically within a write txn
pub fn next_node_id(txn: &redb::WriteTransaction) -> Result<u64> {
    let mut meta = txn.open_table(META_TABLE)?;
    let cur = meta.get("next_node_id")?.map(|v| v.value()).unwrap_or(1);
    meta.insert("next_node_id", cur + 1)?;
    Ok(cur)
}
pub fn next_edge_id(txn: &redb::WriteTransaction) -> Result<u64> {
    let mut meta = txn.open_table(META_TABLE)?;
    let cur = meta.get("next_edge_id")?.map(|v| v.value()).unwrap_or(1_000_000_000);
    meta.insert("next_edge_id", cur + 1)?;
    Ok(cur)
}
