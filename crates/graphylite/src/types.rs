use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Null,
}

impl Value {
    pub fn as_string(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::String(_) => "String",
            Value::Integer(_) => "Integer",
            Value::Float(_) => "Float",
            Value::Boolean(_) => "Boolean",
            Value::Null => "Null",
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::String(s) => write!(f, "\"{s}\""),
            Value::Integer(i) => write!(f, "{i}"),
            Value::Float(fl) => write!(f, "{fl}"),
            Value::Boolean(b) => write!(f, "{b}"),
            Value::Null => write!(f, "null"),
        }
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self { Value::String(s) }
}
impl From<&str> for Value {
    fn from(s: &str) -> Self { Value::String(s.to_string()) }
}
impl From<i64> for Value {
    fn from(i: i64) -> Self { Value::Integer(i) }
}
impl From<i32> for Value {
    fn from(i: i32) -> Self { Value::Integer(i as i64) }
}
impl From<f64> for Value {
    fn from(v: f64) -> Self { Value::Float(v) }
}
impl From<bool> for Value {
    fn from(b: bool) -> Self { Value::Boolean(b) }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeData {
    pub labels: Vec<String>,
    pub properties: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeData {
    pub src: u64,
    pub dst: u64,
    pub edge_type: String,
    pub properties: HashMap<String, Value>,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: u64,
    pub labels: Vec<String>,
    pub properties: HashMap<String, Value>,
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub id: u64,
    pub src: u64,
    pub dst: u64,
    pub edge_type: String,
    pub properties: HashMap<String, Value>,
}

#[derive(Debug, Clone)]
pub struct Row {
    /// column name -> value. Supports scalar Value or serialized Node/Edge json
    pub values: HashMap<String, Value>,
    /// Node objects returned (for RETURN n)
    pub nodes: HashMap<String, Node>,
    /// Edge objects
    pub edges: HashMap<String, Edge>,
    /// Path objects for shortestPath
    pub paths: HashMap<String, PathResult>,
}

impl Row {
    pub fn new() -> Self {
        Self { values: HashMap::new(), nodes: HashMap::new(), edges: HashMap::new(), paths: HashMap::new() }
    }
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }
}

#[derive(Debug, Clone)]
pub struct PathResult {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub length: usize,
}

impl std::fmt::Display for PathResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Path(len={}, nodes=[", self.length)?;
        for (i, n) in self.nodes.iter().enumerate() {
            if i>0 { write!(f, ", ")?; }
            write!(f, "({}:{:?})", n.id, n.labels)?;
        }
        write!(f, "])")
    }
}
