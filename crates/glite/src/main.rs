use clap::Parser;
use comfy_table::{Table, ContentArrangement};
use graphylite::{GraphyLite, types::Value};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

#[derive(Parser, Debug)]
#[command(name="glite", about="GraphyLite interactive CLI", version)]
struct Cli {
    #[arg(default_value="graph.glite")]
    db_path: PathBuf,

    #[arg(short, long)]
    command: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum OutputMode { Table, Json, Csv }
impl std::str::FromStr for OutputMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "table" => Ok(Self::Table),
            "json" => Ok(Self::Json),
            "csv" => Ok(Self::Csv),
            _ => Err(format!("unknown mode {}", s))
        }
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let db = GraphyLite::open(&cli.db_path)?;
    println!("GraphyLite v0.1.0 connected to {:?}", cli.db_path);

    if let Some(cmd) = cli.command {
        handle_query(&db, &cmd, &mut OutputMode::Table, true)?;
        return Ok(());
    }

    // REPL
    let mut rl = DefaultEditor::new()?;
    let history_path = dirs_next();
    if let Some(p) = &history_path {
        let _ = rl.load_history(p);
    }
    let mut mode = OutputMode::Table;
    let mut timer = true;
    let mut buffer = String::new();

    loop {
        let prompt = if buffer.is_empty() { "glite> " } else { "  ...> " };
        match rl.readline(prompt) {
            Ok(line) => {
                let trimmed = line.trim();
                if buffer.is_empty() && trimmed.starts_with('.') {
                    // dot command
                    let parts: Vec<&str> = trimmed.split_whitespace().collect();
                    match parts[0] {
                        ".exit" | ".quit" => break,
                        ".tables" | ".labels" => {
                            match db.list_labels() {
                                Ok(labels) => println!("Labels: {:?}", labels),
                                Err(e) => eprintln!("Error: {}", e),
                            }
                            match db.list_relationship_types() {
                                Ok(rels) => println!("RelationshipTypes: {:?}", rels),
                                Err(e) => eprintln!("Error: {}", e),
                            }
                        },
                        ".schema" => {
                            match db.schema() {
                                Ok(s) => println!("{}", s),
                                Err(e) => eprintln!("Error: {}", e),
                            }
                        },
                        ".mode" => {
                            if parts.len() > 1 {
                                match parts[1].parse::<OutputMode>() {
                                    Ok(m) => { mode = m; println!("mode set to {:?}", mode); },
                                    Err(e) => eprintln!("{}", e),
                                }
                            } else {
                                println!("current mode: {:?}", mode);
                            }
                        },
                        ".timer" => {
                            if parts.len() > 1 {
                                match parts[1] {
                                    "on" => { timer = true; println!("timer on"); },
                                    "off" => { timer = false; println!("timer off"); },
                                    _ => eprintln!("usage: .timer on|off"),
                                }
                            } else {
                                println!("timer: {}", if timer {"on"} else {"off"});
                            }
                        },
                        ".help" => {
                            println!("Dot commands: .tables .labels .schema .mode table|json|csv .timer on|off .import <file> .export <file> .exit .help");
                        },
                        ".import" => {
                            if parts.len() < 2 { eprintln!("usage: .import <file>"); } else {
                                let path = parts[1..].join(" ");
                                match std::fs::read_to_string(&path) {
                                    Ok(content) => {
                                        let mut ok = 0; let mut fail = 0;
                                        for stmt in content.split(';') {
                                            let s = stmt.trim();
                                            if s.is_empty() { continue; }
                                            match db.execute(s, HashMap::new()) {
                                                Ok(r) => { println!("OK {} affected", r.affected); ok+=1; },
                                                Err(e) => { eprintln!("Error: {}", e); fail+=1; },
                                            }
                                        }
                                        println!("import done: {} ok, {} failed", ok, fail);
                                    },
                                    Err(e) => eprintln!("import error: {}", e),
                                }
                            }
                        },
                        ".export" => {
                            if parts.len() < 2 { eprintln!("usage: .export <file>"); } else {
                                let path = parts[1..].join(" ");
                                let mut out = Vec::new();
                                // Export schema
                                if let Ok(s) = db.schema() { out.push(s); }
                                // Export nodes sample: MATCH (n) RETURN n
                                match db.execute("MATCH (n) RETURN n", HashMap::new()) {
                                    Ok(r) => {
                                        for row in r.rows {
                                            for (k,n) in row.nodes { out.push(format!("Node {} {:?} {:?}", k, n.labels, n.properties)); }
                                        }
                                    },
                                    Err(e) => eprintln!("export query error: {}", e),
                                }
                                match std::fs::write(&path, out.join("\n")) {
                                    Ok(_) => println!("exported to {:?}", path),
                                    Err(e) => eprintln!("export error: {}", e),
                                }
                            }
                        },
                        _ => eprintln!("unknown dot command: {}", parts[0]),
                    }
                    continue;
                }

                // accumulate multi-line: until semicolon or empty line execution on single line?
                // If line ends with ';', execute buffer+line
                buffer.push_str(&line);
                buffer.push('\n');
                if trimmed.ends_with(';') || trimmed.is_empty() {
                    let query = buffer.trim().to_string();
                    if !query.is_empty() {
                        rl.add_history_entry(query.clone())?;
                        let start = Instant::now();
                        match handle_query(&db, &query, &mut mode, timer) {
                            Ok(_) => {},
                            Err(e) => eprintln!("Error: {}", e),
                        }
                        if timer {
                            println!("({}ms)", start.elapsed().as_millis());
                        }
                    }
                    buffer.clear();
                } else {
                    // heuristic: if query contains RETURN or CREATE and not ending with ';', wait for more?
                    // But for simplicity execute if buffer contains RETURN/CREATE and line not ending semicolon? We'll try execute on empty? Actually we wait for ';'
                    // To allow single-line without ';', if buffer has newline and query looks complete without semicolon, try execution after empty?
                    // We'll auto-execute if buffer has at least two lines and last line empty? Instead provide fallback: if user presses enter twice?
                    // For now require ';' or dot commands.
                    // To improve UX, if query ends without ';' but is single clause and user presses enter, we can attempt to parse? For now we wait.
                    // Let's also execute if buffer ends with newline and query contains RETURN but no semicolon and we are in single-line mode: execute after 1 second idle? Not.
                    // Simplify: if buffer contains at least one of keywords and user enters empty line, execute.
                    if trimmed.is_empty() && !buffer.trim().is_empty() {
                        let q = buffer.trim().to_string();
                        rl.add_history_entry(q.clone())?;
                        let start = Instant::now();
                        match handle_query(&db, &q, &mut mode, timer) {
                            Ok(_) => {},
                            Err(e) => eprintln!("Error: {}", e),
                        }
                        if timer {
                            println!("({}ms)", start.elapsed().as_millis());
                        }
                        buffer.clear();
                    }
                }
            },
            Err(ReadlineError::Interrupted) => { println!("Interrupted"); break; },
            Err(ReadlineError::Eof) => break,
            Err(err) => { eprintln!("Error: {:?}", err); break; }
        }
    }
    if let Some(p) = history_path {
        let _ = rl.save_history(&p);
    }
    println!("Bye.");
    Ok(())
}

fn dirs_next() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".glite_history"))
}

mod dirs {
    use std::path::PathBuf;
    pub fn home_dir() -> Option<PathBuf> {
        std::env::var("USERPROFILE").ok().map(PathBuf::from).or_else(|| std::env::var("HOME").ok().map(PathBuf::from))
    }
}

fn handle_query(db: &GraphyLite, query: &str, mode: &mut OutputMode, timer: bool) -> anyhow::Result<()> {
    // Strip trailing semicolon for execution but keep internal
    let q = query.trim().trim_end_matches(';').trim();
    if q.is_empty() { return Ok(()); }
    let params: HashMap<String, Value> = HashMap::new();
    let start = Instant::now();
    let result = db.execute(q, params)?;
    let elapsed = start.elapsed();

    if result.rows.is_empty() {
        println!("OK, {} rows affected.", result.affected);
        if timer { println!("{} rows in {}ms", result.rows.len(), elapsed.as_millis()); }
        return Ok(());
    }

    match mode {
        OutputMode::Table => {
            // Build table from rows
            // Collect all column names
            let mut cols = std::collections::BTreeSet::new();
            for row in &result.rows {
                for k in row.values.keys() { cols.insert(k.clone()); }
                for k in row.nodes.keys() { cols.insert(k.clone()); }
                for k in row.edges.keys() { cols.insert(k.clone()); }
                for k in row.paths.keys() { cols.insert(k.clone()); }
            }
            let cols: Vec<String> = cols.into_iter().collect();
            let mut table = Table::new();
            table.load_preset(comfy_table::presets::UTF8_FULL);
            table.set_content_arrangement(ContentArrangement::Dynamic);
            table.set_header(cols.clone());
            for row in &result.rows {
                let mut vals = vec![];
                for c in &cols {
                    if let Some(v) = row.values.get(c) {
                        vals.push(format!("{}", v));
                    } else if let Some(n) = row.nodes.get(c) {
                        vals.push(format!("Node({} labels={:?} props={:?})", n.id, n.labels, n.properties));
                    } else if let Some(e) = row.edges.get(c) {
                        vals.push(format!("Edge({} {} -[{}]-> {} props={:?})", e.id, e.src, e.edge_type, e.dst, e.properties));
                    } else if let Some(p) = row.paths.get(c) {
                        vals.push(format!("{}", p));
                    } else {
                        vals.push("null".to_string());
                    }
                }
                table.add_row(vals);
            }
            println!("{table}");
            println!("{} rows returned in {}ms", result.rows.len(), elapsed.as_millis());
        },
        OutputMode::Json => {
            let mut arr = vec![];
            for row in &result.rows {
                let mut obj = serde_json::Map::new();
                for (k,v) in &row.values {
                    obj.insert(k.clone(), value_to_json(v));
                }
                for (k,n) in &row.nodes {
                    obj.insert(k.clone(), serde_json::json!({"id": n.id, "labels": n.labels, "properties": properties_to_json(&n.properties)}));
                }
                for (k,e) in &row.edges {
                    obj.insert(k.clone(), serde_json::json!({"id": e.id, "src": e.src, "dst": e.dst, "type": e.edge_type, "properties": properties_to_json(&e.properties)}));
                }
                for (k,p) in &row.paths {
                    obj.insert(k.clone(), serde_json::json!({"length": p.length, "nodes": p.nodes.iter().map(|n| serde_json::json!({"id": n.id, "labels": n.labels})).collect::<Vec<_>>()}));
                }
                arr.push(serde_json::Value::Object(obj));
            }
            println!("{}", serde_json::to_string_pretty(&arr)?);
            println!("{} rows in {}ms", result.rows.len(), elapsed.as_millis());
        },
        OutputMode::Csv => {
            let mut cols = std::collections::BTreeSet::new();
            for row in &result.rows {
                for k in row.values.keys() { cols.insert(k.clone()); }
                for k in row.nodes.keys() { cols.insert(k.clone()); }
                for k in row.edges.keys() { cols.insert(k.clone()); }
                for k in row.paths.keys() { cols.insert(k.clone()); }
            }
            let cols: Vec<String> = cols.into_iter().collect();
            println!("{}", cols.join(","));
            for row in &result.rows {
                let mut vals = vec![];
                for c in &cols {
                    if let Some(v) = row.values.get(c) { vals.push(format!("{}", v)); }
                    else if let Some(n) = row.nodes.get(c) { vals.push(format!("Node({})", n.id)); }
                    else if let Some(e) = row.edges.get(c) { vals.push(format!("Edge({})", e.id)); }
                    else if let Some(p) = row.paths.get(c) { vals.push(format!("{}", p.length)); }
                    else { vals.push("".to_string()); }
                }
                println!("{}", vals.join(","));
            }
            println!("{} rows in {}ms", result.rows.len(), elapsed.as_millis());
        }
    }
    Ok(())
}

fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::Integer(i) => serde_json::json!(*i),
        Value::Float(f) => serde_json::json!(*f),
        Value::Boolean(b) => serde_json::json!(*b),
        Value::Null => serde_json::Value::Null,
    }
}
fn properties_to_json(props: &HashMap<String, Value>) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    for (k,v) in props { m.insert(k.clone(), value_to_json(v)); }
    serde_json::Value::Object(m)
}
