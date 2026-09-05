use graphylite::{GraphyLite, types::{Value, Node, Edge, PathResult}};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString, PyFloat, PyInt, PyBool};
use pyo3::exceptions::{PyValueError, PyRuntimeError};
use pyo3::IntoPy;
use std::collections::HashMap;

// ---------- Value conversion ----------

fn py_to_value(obj: &Bound<PyAny>) -> PyResult<Value> {
    if obj.is_none() {
        return Ok(Value::Null);
    }
    // bool must be checked before int because bool is subclass of int in Python
    if let Ok(b) = obj.downcast::<PyBool>() {
        return Ok(Value::Boolean(b.is_true()));
    }
    if let Ok(i) = obj.downcast::<PyInt>() {
        // Try i64 first, fallback to float?
        if let Ok(v) = i.extract::<i64>() {
            return Ok(Value::Integer(v));
        }
        if let Ok(v) = i.extract::<f64>() {
            return Ok(Value::Float(v));
        }
        return Err(PyValueError::new_err("invalid integer"));
    }
    if let Ok(f) = obj.downcast::<PyFloat>() {
        let v: f64 = f.extract()?;
        return Ok(Value::Float(v));
    }
    if let Ok(s) = obj.downcast::<PyString>() {
        return Ok(Value::String(s.to_string()));
    }
    // fallback: try to extract as string
    if let Ok(s) = obj.extract::<String>() {
        return Ok(Value::String(s));
    }
    Err(PyValueError::new_err(format!("unsupported param type: {}", obj.get_type().name()?)))
}

fn value_to_py(py: Python, v: &Value) -> PyResult<Py<PyAny>> {
    match v {
        Value::String(s) => Ok(s.into_py(py)),
        Value::Integer(i) => Ok(i.into_py(py)),
        Value::Float(f) => Ok(f.into_py(py)),
        Value::Boolean(b) => Ok(b.into_py(py)),
        Value::Null => Ok(py.None()),
    }
}

fn node_to_py(py: Python, n: &Node) -> PyResult<Py<PyAny>> {
    let d = PyDict::new_bound(py);
    d.set_item("id", n.id)?;
    let labels = PyList::new_bound(py, n.labels.iter().map(|s| PyString::new_bound(py, s)));
    d.set_item("labels", labels)?;
    let props = PyDict::new_bound(py);
    for (k, v) in &n.properties {
        props.set_item(k, value_to_py(py, v)?)?;
    }
    d.set_item("properties", props)?;
    Ok(d.into())
}

fn edge_to_py(py: Python, e: &Edge) -> PyResult<Py<PyAny>> {
    let d = PyDict::new_bound(py);
    d.set_item("id", e.id)?;
    d.set_item("src", e.src)?;
    d.set_item("dst", e.dst)?;
    d.set_item("type", &e.edge_type)?;
    let props = PyDict::new_bound(py);
    for (k, v) in &e.properties {
        props.set_item(k, value_to_py(py, v)?)?;
    }
    d.set_item("properties", props)?;
    Ok(d.into())
}

fn path_to_py(py: Python, p: &PathResult) -> PyResult<Py<PyAny>> {
    let d = PyDict::new_bound(py);
    d.set_item("length", p.length)?;
    let nodes = PyList::new_bound(py, p.nodes.iter().map(|n| node_to_py(py, n).unwrap()));
    // unwrap is safe because conversion shouldn't fail; if it does, we map to PyErr separately
    let edges = PyList::new_bound(py, p.edges.iter().map(|e| edge_to_py(py, e).unwrap()));
    d.set_item("nodes", nodes)?;
    d.set_item("edges", edges)?;
    Ok(d.into())
}

fn row_to_py(py: Python, row: &graphylite::types::Row) -> PyResult<Py<PyAny>> {
    let d = PyDict::new_bound(py);
    // values
    for (k, v) in &row.values {
        d.set_item(k, value_to_py(py, v)?)?;
    }
    // nodes
    for (k, n) in &row.nodes {
        d.set_item(k, node_to_py(py, n)?)?;
    }
    for (k, e) in &row.edges {
        d.set_item(k, edge_to_py(py, e)?)?;
    }
    for (k, p) in &row.paths {
        d.set_item(k, path_to_py(py, p)?)?;
    }
    // Also expose explicit sub-dicts for introspection
    // Keep flat for Cypher-like RETURN semantics (like RETURN n, n.name)
    Ok(d.into())
}

fn params_from_pydict(dict: Option<&Bound<PyDict>>) -> PyResult<HashMap<String, Value>> {
    let mut map = HashMap::new();
    if let Some(d) = dict {
        for (k, v) in d.iter() {
            let key: String = k.extract()?;
            let val = py_to_value(&v)?;
            map.insert(key, val);
        }
    }
    Ok(map)
}

// ---------- PyGraphyLite ----------

#[pyclass]
pub struct PyGraphyLite {
    inner: GraphyLite,
}

#[pymethods]
impl PyGraphyLite {
    #[new]
    #[pyo3(signature = (path))]
    fn new(path: String) -> PyResult<Self> {
        let db = GraphyLite::open(path).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        Ok(Self { inner: db })
    }

    #[staticmethod]
    fn open_in_memory() -> PyResult<Self> {
        let db = GraphyLite::open_in_memory().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        Ok(Self { inner: db })
    }

    #[staticmethod]
    #[pyo3(signature = (path))]
    fn open(path: String) -> PyResult<Self> {
        Self::new(path)
    }

    /// Execute a Cypher query. Returns {"rows": [...], "affected": int}
    #[pyo3(signature = (query, params=None))]
    fn execute(&self, py: Python, query: String, params: Option<&Bound<PyDict>>) -> PyResult<Py<PyAny>> {
        let pmap = params_from_pydict(params)?;
        let res = self.inner.execute(&query, pmap).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        let rows = PyList::empty_bound(py);
        for row in &res.rows {
            rows.append(row_to_py(py, row)?)?;
        }
        let d = PyDict::new_bound(py);
        d.set_item("rows", rows)?;
        d.set_item("affected", res.affected)?;
        Ok(d.into())
    }

    /// Convenience: query returns list of rows
    #[pyo3(signature = (query, params=None))]
    fn query(&self, py: Python, query: String, params: Option<&Bound<PyDict>>) -> PyResult<Py<PyAny>> {
        let pmap = params_from_pydict(params)?;
        let rows = self.inner.query(&query, pmap).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        let pylist = PyList::empty_bound(py);
        for row in rows {
            pylist.append(row_to_py(py, &row)?)?;
        }
        Ok(pylist.into())
    }

    fn list_labels(&self) -> PyResult<Vec<String>> {
        self.inner.list_labels().map_err(|e| PyRuntimeError::new_err(e.to_string()))
    }

    fn list_relationship_types(&self) -> PyResult<Vec<String>> {
        self.inner.list_relationship_types().map_err(|e| PyRuntimeError::new_err(e.to_string()))
    }

    fn schema(&self) -> PyResult<String> {
        self.inner.schema().map_err(|e| PyRuntimeError::new_err(e.to_string()))
    }

    /// Execute multiple queries in sequence (simple batch)
    #[pyo3(signature = (queries))]
    fn execute_batch(&self, py: Python, queries: Vec<(String, Option<Bound<PyDict>>)>) -> PyResult<Py<PyAny>> {
        let mut batch = Vec::new();
        for (q, p) in queries {
            let m = params_from_pydict(p.as_ref().map(|d| d as &Bound<PyDict>))?;
            batch.push((q, m));
        }
        let results = self.inner.execute_batch(batch).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        let pylist = PyList::empty_bound(py);
        for res in results {
            let rows = PyList::empty_bound(py);
            for row in &res.rows {
                rows.append(row_to_py(py, row)?)?;
            }
            let d = PyDict::new_bound(py);
            d.set_item("rows", rows)?;
            d.set_item("affected", res.affected)?;
            pylist.append(d)?;
        }
        Ok(pylist.into())
    }

    // Pathfinding APIs
    #[pyo3(signature = (start, end, edge_type=None, direction="outgoing"))]
    fn shortest_path_bfs(&self, py: Python, start: u64, end: u64, edge_type: Option<String>, direction: &str) -> PyResult<Py<PyAny>> {
        let dir = parse_direction(direction)?;
        let res = self.inner.shortest_path_bfs(start, end, edge_type.as_deref(), dir).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        if let Some((nodes, edges)) = res {
            let d = PyDict::new_bound(py);
            let pynodes = PyList::new_bound(py, nodes.iter().map(|n| node_to_py(py, n).unwrap()));
            let pyedges = PyList::new_bound(py, edges.iter().map(|e| edge_to_py(py, e).unwrap()));
            d.set_item("nodes", pynodes)?;
            d.set_item("edges", pyedges)?;
            d.set_item("length", edges.len())?;
            Ok(d.into())
        } else {
            Ok(py.None())
        }
    }

    #[pyo3(signature = (start, end, edge_type=None, weight_prop="weight", direction="outgoing"))]
    fn shortest_path_dijkstra(&self, py: Python, start: u64, end: u64, edge_type: Option<String>, weight_prop: &str, direction: &str) -> PyResult<Py<PyAny>> {
        let dir = parse_direction(direction)?;
        let res = self.inner.shortest_path_dijkstra(start, end, edge_type.as_deref(), weight_prop, dir).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        if let Some((nodes, edges, cost)) = res {
            let d = PyDict::new_bound(py);
            let pynodes = PyList::new_bound(py, nodes.iter().map(|n| node_to_py(py, n).unwrap()));
            let pyedges = PyList::new_bound(py, edges.iter().map(|e| edge_to_py(py, e).unwrap()));
            d.set_item("nodes", pynodes)?;
            d.set_item("edges", pyedges)?;
            d.set_item("cost", cost)?;
            d.set_item("length", edges.len())?;
            Ok(d.into())
        } else {
            Ok(py.None())
        }
    }

    #[pyo3(signature = (start, end, edge_type=None))]
    fn shortest_path_bidirectional(&self, py: Python, start: u64, end: u64, edge_type: Option<String>) -> PyResult<Py<PyAny>> {
        let res = self.inner.shortest_path_bidirectional(start, end, edge_type.as_deref()).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        if let Some((nodes, edges)) = res {
            let d = PyDict::new_bound(py);
            let pynodes = PyList::new_bound(py, nodes.iter().map(|n| node_to_py(py, n).unwrap()));
            let pyedges = PyList::new_bound(py, edges.iter().map(|e| edge_to_py(py, e).unwrap()));
            d.set_item("nodes", pynodes)?;
            d.set_item("edges", pyedges)?;
            d.set_item("length", edges.len())?;
            Ok(d.into())
        } else {
            Ok(py.None())
        }
    }
}

fn parse_direction(s: &str) -> PyResult<graphylite::parser::Direction> {
    match s.to_lowercase().as_str() {
        "outgoing" | "out" | "->" => Ok(graphylite::parser::Direction::Outgoing),
        "incoming" | "in" | "<-" => Ok(graphylite::parser::Direction::Incoming),
        "undirected" | "both" | "--" => Ok(graphylite::parser::Direction::Undirected),
        _ => Err(PyValueError::new_err(format!("unknown direction: {}", s))),
    }
}

#[pymodule]
fn pygraphylite(m: &Bound<PyModule>) -> PyResult<()> {
    m.add_class::<PyGraphyLite>()?;
    // Re-export Value helpers for docs
    m.add("__doc__", "GraphyLite Python bindings — embedded labeled property graph (Cypher subset)")?;
    Ok(())
}
