use crate::error::{GliteError, Result};
use crate::types::Value;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum Expr {
    Literal(Value),
    Param(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Direction {
    Outgoing,   // -->
    Incoming,   // <--
    Undirected, // --
}

#[derive(Debug, Clone)]
pub struct PatternNode {
    pub variable: Option<String>,
    pub labels: Vec<String>,
    pub properties: HashMap<String, Expr>,
}

#[derive(Debug, Clone)]
pub struct PatternEdge {
    pub variable: Option<String>,
    pub edge_type: Option<String>,
    pub properties: HashMap<String, Expr>,
    pub direction: Direction,
    pub is_variable_length: bool, // for [*] syntax
    pub var_len_min: Option<usize>,
    pub var_len_max: Option<usize>,
    pub weight_property: Option<String>, // for Dijkstra weight
}

#[derive(Debug, Clone)]
pub struct PatternPath {
    pub nodes: Vec<PatternNode>,
    pub edges: Vec<PatternEdge>,
}

#[derive(Debug, Clone)]
pub struct MatchClause {
    pub paths: Vec<PatternPath>,
    pub optional: bool,
}

#[derive(Debug, Clone)]
pub struct WhereClause {
    pub conditions: Vec<Condition>,
}

#[derive(Debug, Clone)]
pub struct Condition {
    pub left_var: String,
    pub left_prop: String,
    pub op: Operator,
    pub right: ExprOrProp,
}

#[derive(Debug, Clone)]
pub enum ExprOrProp {
    Expr(Expr),
    Prop { var: String, prop: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Operator {
    Eq,
    NotEq,
    Gt,
    Lt,
    Gte,
    Lte,
}

#[derive(Debug, Clone)]
pub struct CreateClause {
    pub paths: Vec<PatternPath>,
}

#[derive(Debug, Clone)]
pub struct SetClause {
    pub assignments: Vec<Assignment>,
}
#[derive(Debug, Clone)]
pub struct Assignment {
    pub var: String,
    pub prop: String,
    pub value: Expr,
}

#[derive(Debug, Clone)]
pub struct DeleteClause {
    pub variables: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum ReturnExpr {
    Variable(String),
    Property { var: String, prop: String },
    ShortestPath(ShortestPathSpec),
    Count(String), // count(n)
}

#[derive(Debug, Clone)]
pub struct ReturnItem {
    pub expr: ReturnExpr,
    pub alias: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ReturnClause {
    pub items: Vec<ReturnItem>,
    pub distinct: bool,
}

#[derive(Debug, Clone)]
pub struct ShortestPathSpec {
    pub path_var: Option<String>, // e.g., p=shortestPath(...)
    pub start_var: String,
    pub end_var: String,
    pub edge_type: Option<String>,
    pub direction: Direction,
    pub weighted: bool,
    pub weight_prop: Option<String>,
    pub algorithm: PathAlgorithm,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PathAlgorithm {
    Bfs,
    Dijkstra,
    Bidirectional,
}

#[derive(Debug, Clone)]
pub struct ParsedQuery {
    pub creates: Vec<CreateClause>,
    pub matches: Vec<MatchClause>,
    pub where_clause: Option<WhereClause>,
    pub sets: Vec<SetClause>,
    pub deletes: Vec<DeleteClause>,
    pub returns: Option<ReturnClause>,
    pub shortest_path: Option<ShortestPathSpec>,
}

// ---------- Parser implementation ----------

pub fn parse_query(input: &str, params: &HashMap<String, Value>) -> Result<ParsedQuery> {
    // Validate with decypher for diagnostics (R10)
    let decypher_err = match decypher::parse(input) {
        Ok(_) => None,
        Err(e) => Some(format!("{:?}", e)),
    };

    let mut parser = Parser::new(input);
    match parser.parse(params) {
        Ok(pq) => Ok(pq),
        Err(GliteError::Parse(msg)) => {
            if let Some(d) = decypher_err {
                Err(GliteError::Parse(format!("{}; decypher: {}", msg, d)))
            } else {
                Err(GliteError::Parse(msg))
            }
        },
        Err(e) => Err(e),
    }
}

struct Parser<'a> {
    input: &'a str,
    pos: usize,
    chars: Vec<char>,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0, chars: input.chars().collect() }
    }

    fn remaining(&self) -> String {
        self.chars[self.pos..].iter().collect()
    }

    fn peek(&self) -> Option<char> { self.chars.get(self.pos).copied() }
    #[allow(dead_code)]
    fn peek_next(&self, n: usize) -> Option<char> { self.chars.get(self.pos+n).copied() }
    fn consume(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied();
        if c.is_some() { self.pos+=1; }
        c
    }
    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() { self.consume(); } else { break; }
        }
    }
    fn expect_char(&mut self, expected: char) -> Result<()> {
        self.skip_ws();
        if self.peek() == Some(expected) { self.consume(); Ok(()) }
        else { Err(GliteError::Parse(format!("expected '{}' at pos {}", expected, self.pos))) }
    }
    fn consume_str_case_insensitive(&mut self, s: &str) -> bool {
        self.skip_ws();
        let rem: String = self.chars[self.pos..].iter().collect();
        if rem.len() < s.len() { return false; }
        if rem[..s.len()].eq_ignore_ascii_case(s) {
            // ensure word boundary for keywords
            let after = rem.chars().nth(s.len());
            if let Some(ch) = after {
                if ch.is_alphanumeric() || ch=='_' {
                    // if s is keyword, require non-alnum next; but for symbols like "->" it's fine
                    if s.chars().all(|c| c.is_alphabetic()) {
                        return false;
                    }
                }
            }
            self.pos += s.len();
            true
        } else { false }
    }
    fn peek_keyword(&self, kw: &str) -> bool {
        let mut p = self.pos;
        while p < self.chars.len() && self.chars[p].is_whitespace() { p+=1; }
        let rem: String = self.chars[p..].iter().collect();
        if rem.len() < kw.len() { return false; }
        rem[..kw.len()].eq_ignore_ascii_case(kw)
    }

    fn parse(&mut self, _params: &HashMap<String, Value>) -> Result<ParsedQuery> {
        let mut pq = ParsedQuery {
            creates: vec![], matches: vec![], where_clause: None, sets: vec![], deletes: vec![], returns: None, shortest_path: None,
        };
        // Handle shortestPath assignment form: MATCH p=shortestPath(...)
        // We'll preprocess detection before main loop.

        loop {
            self.skip_ws();
            if self.pos >= self.chars.len() { break; }
            if self.consume_str_case_insensitive("CREATE") {
                let cc = self.parse_create()?;
                pq.creates.push(cc);
                // optional comma handled inside
                if self.peek() == Some(',') { /* continued? */ }
                // allow optional RETURN after CREATE in same statement? We'll loop again to parse RETURN
            } else if self.consume_str_case_insensitive("MATCH") {
                // Check for p=shortestPath pattern
                // Look ahead for shortestPath
                let saved = self.pos;
                self.skip_ws();
                // check for var = shortestPath
                let maybe_sp = self.try_parse_shortest_path_assignment();
                if let Some(sp) = maybe_sp {
                    pq.shortest_path = Some(sp);
                    // Need to also handle that this MATCH contains nodes for start/end resolution
                    // For now, we create a dummy match clause to bind start/end vars?
                    // Instead we skip creating match clause, shortest_path covers it.
                    // But also need to ensure we capture MATCH vars? We'll parse remaining shortestPath path to extract vars?
                    // For now push empty match
                    pq.matches.push(MatchClause{ paths: vec![], optional: false });
                } else {
                    self.pos = saved;
                    // need to skip initial MATCH keyword already consumed, so re-parse? we consumed MATCH already, so parse paths without re-consuming
                    let mc = self.parse_match_paths()?;
                    pq.matches.push(mc);
                }
            } else if self.peek_keyword("WHERE") {
                self.consume_str_case_insensitive("WHERE");
                let wc = self.parse_where()?;
                pq.where_clause = Some(wc);
            } else if self.consume_str_case_insensitive("SET") {
                let sc = self.parse_set()?;
                pq.sets.push(sc);
            } else if self.consume_str_case_insensitive("DELETE") {
                let dc = self.parse_delete()?;
                pq.deletes.push(dc);
            } else if self.consume_str_case_insensitive("RETURN") {
                let rc = self.parse_return()?;
                // Detect shortestPath inside RETURN items
                for item in &rc.items {
                    if let ReturnExpr::ShortestPath(sp) = &item.expr {
                        pq.shortest_path = Some(sp.clone());
                    }
                }
                pq.returns = Some(rc);
            } else if self.consume_str_case_insensitive("WITH") {
                // treat WITH as RETURN-like for chaining; ignore for MVP
                let rc = self.parse_return()?;
                pq.returns = Some(rc);
            } else {
                // Try to handle comma separated creates without keyword?
                // Or stray semicolon
                if self.peek() == Some(';') { self.consume(); continue; }
                return Err(GliteError::Parse(format!("unexpected token at pos {}: '{}'", self.pos, self.remaining().chars().take(20).collect::<String>())));
            }
            self.skip_ws();
            // consume optional semicolon
            if self.peek() == Some(';') { self.consume(); }
        }
        Ok(pq)
    }

    fn parse_create(&mut self) -> Result<CreateClause> {
        let paths = self.parse_pattern_list()?;
        Ok(CreateClause{ paths })
    }

    fn parse_match_paths(&mut self) -> Result<MatchClause> {
        let paths = self.parse_pattern_list()?;
        Ok(MatchClause{ paths, optional: false })
    }

    fn parse_pattern_list(&mut self) -> Result<Vec<PatternPath>> {
        let mut paths = vec![];
        loop {
            self.skip_ws();
            if self.peek().is_none() { break; }
            // stop if next keyword
            if self.peek_keyword("WHERE") || self.peek_keyword("RETURN") || self.peek_keyword("SET") || self.peek_keyword("DELETE") || self.peek_keyword("MATCH") || self.peek_keyword("CREATE") || self.peek_keyword("WITH") { break; }
            let path = self.parse_pattern_path()?;
            paths.push(path);
            self.skip_ws();
            if self.peek() == Some(',') {
                self.consume();
                continue;
            } else { break; }
        }
        Ok(paths)
    }

    fn parse_pattern_path(&mut self) -> Result<PatternPath> {
        let mut nodes = vec![];
        let mut edges = vec![];
        // Expect node
        let node = self.parse_node_pattern()?;
        nodes.push(node);
        loop {
            self.skip_ws();
            // Check for edge pattern: -[ ... ]-> , <-[ ... ]- , -[ ... ]- , --> etc.
            let saved = self.pos;
            // possible edge start: '-', '<', etc.
            if self.peek() != Some('-') && self.peek() != Some('<') { break; }
            // Try to parse edge
            if let Some(edge) = self.try_parse_edge_pattern()? {
                edges.push(edge);
                self.skip_ws();
                let node2 = self.parse_node_pattern()?;
                nodes.push(node2);
            } else {
                self.pos = saved;
                break;
            }
        }
        Ok(PatternPath{ nodes, edges })
    }

    fn try_parse_edge_pattern(&mut self) -> Result<Option<PatternEdge>> {
        self.skip_ws();
        let start = self.pos;
        let direction;
        // Check leading '<'
        let mut has_left_arrow = false;
        if self.peek() == Some('<') {
            has_left_arrow = true;
            self.consume();
        }
        if self.peek() != Some('-') {
            self.pos = start;
            return Ok(None);
        }
        self.consume(); // '-'
        // Optional '['
        let mut variable = None;
        let mut edge_type = None;
        let mut properties = HashMap::new();
        let mut is_variable_length = false;
        let mut var_len_min: Option<usize> = None;
        let mut var_len_max: Option<usize> = None;
        if self.peek() == Some('[') {
            self.consume();
            self.skip_ws();
            // variable or type?
            // Could be ":TYPE" or "r:TYPE" or "r" or "*"
            // Parse until ']'
            let mut inner = String::new();
            let mut depth = 1;
            let content_start = self.pos;
            while let Some(c) = self.peek() {
                if c == '[' { depth+=1; }
                if c == ']' { depth-=1; if depth==0 { break; } }
                inner.push(c);
                self.consume();
            }
            // Now inner contains inside brackets
            // Parse inner separately
            let inner_str = self.chars[content_start..self.pos].iter().collect::<String>();
            // Mark variable length and parse bounds like *1..5, *..5, *2.., *
            let mut cleaned_inner = inner_str.clone();
            if inner_str.contains('*') {
                is_variable_length = true;
                // extract bounds after '*'
                if let Some(star_idx) = inner_str.find('*') {
                    let after = inner_str[star_idx+1..].trim_start();
                    // Find end of bounds (digits and dots)
                    let mut bound_len = 0usize;
                    for c in after.chars() {
                        if c.is_ascii_digit() || c=='.' { bound_len += c.len_utf8(); } else { break; }
                    }
                    let bound_str = after[..bound_len].trim();
                    if !bound_str.is_empty() {
                        // parse like "1..5", "1..", "..5", "3"
                        if bound_str.contains("..") {
                            let parts: Vec<&str> = bound_str.split("..").collect();
                            let min_s = parts[0].trim();
                            let max_s = if parts.len()>1 { parts[1].trim() } else { "" };
                            if !min_s.is_empty() {
                                if let Ok(v) = min_s.parse::<usize>() { var_len_min = Some(v); }
                            }
                            if !max_s.is_empty() {
                                if let Ok(v) = max_s.parse::<usize>() { var_len_max = Some(v); }
                            }
                            if var_len_min.is_none() && var_len_max.is_some() { var_len_min = Some(1); }
                            if var_len_min.is_none() && var_len_max.is_none() { var_len_min = Some(1); var_len_max = Some(10); }
                        } else {
                            if let Ok(v) = bound_str.parse::<usize>() {
                                var_len_min = Some(v);
                                var_len_max = Some(v);
                            }
                        }
                    } else {
                        // bare "*" -> 1..10 default
                        var_len_min = Some(1);
                        var_len_max = Some(10);
                    }
                    if var_len_min.is_none() { var_len_min = Some(1); }
                    if var_len_max.is_none() { var_len_max = Some(10); }
                    // Build cleaned inner without the *bounds
                    let before = &inner_str[..star_idx];
                    let after_remainder = &after[bound_len..];
                    cleaned_inner = format!("{}{}", before, after_remainder);
                }
            }
            // Simple parsing for inner
            self.parse_edge_inner(&cleaned_inner, &mut variable, &mut edge_type, &mut properties)?;
            // expect ']'
            if self.peek() == Some(']') { self.consume(); } else { return Err(GliteError::Parse("expected ']'".into())) }
        }
        // Expect '-'
        if self.peek() != Some('-') {
            self.pos = start;
            return Ok(None);
        }
        self.consume();
        let mut has_right_arrow = false;
        if self.peek() == Some('>') {
            has_right_arrow = true;
            self.consume();
        }
        direction = match (has_left_arrow, has_right_arrow) {
            (true, false) => Direction::Incoming, // <-...-
            (false, true) => Direction::Outgoing, // -...->
            (true, true) => Direction::Undirected, // though <--> unusual
            (false, false) => Direction::Undirected,
        };
        Ok(Some(PatternEdge{ variable, edge_type, properties, direction, is_variable_length, var_len_min, var_len_max, weight_property: None }))
    }

    fn parse_edge_inner(&mut self, inner: &str, variable: &mut Option<String>, edge_type: &mut Option<String>, props: &mut HashMap<String, Expr>) -> Result<()> {
        let s = inner.trim();
        if s.is_empty() { return Ok(()); }
        // Remove '*' for variable length marker
        let s_clean = s.replace("*", "").trim().to_string();
        if s_clean.is_empty() { return Ok(()); }
        // Check for properties { ... }
        let (head, prop_str) = if let Some(idx) = s_clean.find('{') {
            (&s_clean[..idx], Some(&s_clean[idx..]))
        } else { (s_clean.as_str(), None) };

        let head = head.trim();
        if !head.is_empty() {
            if head.starts_with(":") {
                *edge_type = Some(head[1..].trim().to_string());
            } else if head.contains(":") {
                let parts: Vec<&str> = head.splitn(2, ':').collect();
                *variable = Some(parts[0].trim().to_string());
                let t = parts[1].trim();
                // t may contain trailing weight? ignore
                if !t.is_empty() { *edge_type = Some(t.to_string()); }
            } else {
                // just variable
                let h = head.trim();
                if !h.is_empty() {
                    *variable = Some(h.to_string());
                }
            }
        }
        if let Some(pstr) = prop_str {
            let map = parse_properties_map(pstr)?;
            *props = map;
        }
        Ok(())
    }

    fn parse_node_pattern(&mut self) -> Result<PatternNode> {
        self.skip_ws();
        self.expect_char('(')?;
        // Capture until matching ')'
        let _start = self.pos;
        let mut depth = 1;
        let mut content = String::new();
        while let Some(c) = self.peek() {
            if c == '(' { depth+=1; }
            else if c == ')' { depth-=1; if depth==0 { break; } }
            content.push(c);
            self.consume();
        }
        // content is inside parentheses
        let node = parse_node_content(&content)?;
        self.expect_char(')')?;
        Ok(node)
    }

    fn parse_where(&mut self) -> Result<WhereClause> {
        let mut conditions = vec![];
        loop {
            self.skip_ws();
            // parse condition: var.prop op value-or-prop
            // stop if next keyword
            if self.peek_keyword("RETURN") || self.peek_keyword("SET") || self.peek_keyword("DELETE") || self.peek_keyword("MATCH") || self.peek_keyword("CREATE") || self.peek_keyword("ORDER") || self.peek_keyword("LIMIT") || self.peek().is_none() { break; }
            // handle AND
            if self.consume_str_case_insensitive("AND") { continue; }
            // also handle OR? treat as AND for MVP
            if self.consume_str_case_insensitive("OR") { continue; }
            // Need to parse left side: var.prop or var
            let left = self.parse_var_prop()?;
            self.skip_ws();
            let op = self.parse_operator()?;
            self.skip_ws();
            // right side: could be var.prop or literal/param
            let right = self.parse_right_expr()?;
            conditions.push(Condition{ left_var: left.0, left_prop: left.1, op, right });
            self.skip_ws();
            if self.peek_keyword("AND") || self.peek_keyword("OR") {
                continue;
            }
            // Check if next is keyword else continue parsing another condition? We break if next is keyword else comma?
            if self.peek() == Some(',') { self.consume(); continue; }
            // If next starts with var.prop without AND, allow? For simplicity break after one if no AND
            // But we attempt to see if remaining looks like condition
            let save = self.pos;
            self.skip_ws();
            if self.pos < self.chars.len() && self.peek().map(|c| c.is_alphabetic() || c=='_').unwrap_or(false) {
                // peek ahead if contains dot or =
                // attempt to parse next condition if not a keyword
                if !self.peek_keyword("RETURN") && !self.peek_keyword("SET") && !self.peek_keyword("DELETE") {
                    self.pos = save;
                    // continue loop expects AND? but we allow implicit AND
                    // we need to check if next chars contain comparator; if not, break
                    let rem: String = self.chars[self.pos..].iter().collect();
                    if rem.contains('=') || rem.contains('>') || rem.contains('<') {
                        continue;
                    } else {
                        break;
                    }
                } else {
                    self.pos = save;
                    break;
                }
            } else {
                break;
            }
        }
        Ok(WhereClause{ conditions })
    }

    fn parse_var_prop(&mut self) -> Result<(String, String)> {
        self.skip_ws();
        let var = self.parse_identifier()?;
        self.skip_ws();
        if self.peek() == Some('.') {
            self.consume();
            let prop = self.parse_identifier()?;
            Ok((var, prop))
        } else {
            // No prop, maybe shouldn't happen
            Ok((var, "".to_string()))
        }
    }

    fn parse_identifier(&mut self) -> Result<String> {
        self.skip_ws();
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c=='_' { s.push(c); self.consume(); }
            else { break; }
        }
        if s.is_empty() { return Err(GliteError::Parse("expected identifier".into())) }
        Ok(s)
    }

    fn parse_operator(&mut self) -> Result<Operator> {
        self.skip_ws();
        if self.remaining().starts_with("<>") || self.remaining().starts_with("!=") {
            self.pos+=2; Ok(Operator::NotEq)
        } else if self.remaining().starts_with(">=") { self.pos+=2; Ok(Operator::Gte) }
        else if self.remaining().starts_with("<=") { self.pos+=2; Ok(Operator::Lte) }
        else if self.peek() == Some('=') { self.consume(); Ok(Operator::Eq) }
        else if self.peek() == Some('>') { self.consume(); Ok(Operator::Gt) }
        else if self.peek() == Some('<') { self.consume(); Ok(Operator::Lt) }
        else { Err(GliteError::Parse("expected operator".into())) }
    }

    fn parse_right_expr(&mut self) -> Result<ExprOrProp> {
        self.skip_ws();
        let c = self.peek().ok_or(GliteError::Parse("expected expr".into()))?;
        if c == '$' {
            self.consume();
            let name = self.parse_identifier()?;
            Ok(ExprOrProp::Expr(Expr::Param(name)))
        } else if c == '\'' || c == '"' {
            let val = self.parse_string_literal()?;
            Ok(ExprOrProp::Expr(Expr::Literal(Value::String(val))))
        } else if c.is_ascii_digit() || c=='-' {
            let num = self.parse_number_literal()?;
            Ok(ExprOrProp::Expr(Expr::Literal(num)))
        } else if c.is_alphabetic() || c=='_' {
            // Could be true/false/null or var.prop or param?
            let ident = self.parse_identifier()?;
            let lower = ident.to_lowercase();
            if lower=="true" { return Ok(ExprOrProp::Expr(Expr::Literal(Value::Boolean(true)))) }
            if lower=="false" { return Ok(ExprOrProp::Expr(Expr::Literal(Value::Boolean(false)))) }
            if lower=="null" { return Ok(ExprOrProp::Expr(Expr::Literal(Value::Null))) }
            // Check if next is '.'
            self.skip_ws();
            if self.peek() == Some('.') {
                self.consume();
                let prop = self.parse_identifier()?;
                Ok(ExprOrProp::Prop{ var: ident, prop })
            } else {
                // treat as string literal? For now return as string
                Ok(ExprOrProp::Expr(Expr::Literal(Value::String(ident))))
            }
        } else {
            Err(GliteError::Parse(format!("unexpected expr start: {}", c)))
        }
    }

    fn parse_string_literal(&mut self) -> Result<String> {
        let quote = self.consume().unwrap();
        let mut s = String::new();
        while let Some(c) = self.consume() {
            if c == quote { break; }
            if c=='\\' {
                if let Some(n) = self.consume() { s.push(n); }
            } else { s.push(c); }
        }
        Ok(s)
    }

    fn parse_number_literal(&mut self) -> Result<Value> {
        let mut s = String::new();
        if self.peek() == Some('-') { s.push('-'); self.consume(); }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c=='.' { s.push(c); self.consume(); } else { break; }
        }
        if s.contains('.') {
            let f: f64 = s.parse().map_err(|_| GliteError::Parse(format!("invalid float {}", s)))?;
            Ok(Value::Float(f))
        } else {
            let i: i64 = s.parse().map_err(|_| GliteError::Parse(format!("invalid int {}", s)))?;
            Ok(Value::Integer(i))
        }
    }

    fn parse_set(&mut self) -> Result<SetClause> {
        let mut assignments = vec![];
        loop {
            self.skip_ws();
            if self.peek_keyword("RETURN") || self.peek_keyword("DELETE") || self.peek_keyword("MATCH") || self.peek_keyword("CREATE") || self.peek_keyword("WHERE") || self.peek().is_none() { break; }
            let var = self.parse_identifier()?;
            self.expect_char('.')?;
            let prop = self.parse_identifier()?;
            self.skip_ws();
            self.expect_char('=')?;
            self.skip_ws();
            let expr = self.parse_expr_literal()?;
            assignments.push(Assignment{ var, prop, value: expr });
            self.skip_ws();
            if self.peek() == Some(',') { self.consume(); continue; } else { break; }
        }
        Ok(SetClause{ assignments })
    }

    fn parse_expr_literal(&mut self) -> Result<Expr> {
        self.skip_ws();
        let c = self.peek().ok_or(GliteError::Parse("expected expr".into()))?;
        if c=='$' {
            self.consume();
            let name = self.parse_identifier()?;
            Ok(Expr::Param(name))
        } else if c=='\'' || c=='"' {
            let s = self.parse_string_literal()?;
            Ok(Expr::Literal(Value::String(s)))
        } else if c.is_ascii_digit() || c=='-' {
            let v = self.parse_number_literal()?; Ok(Expr::Literal(v))
        } else if c.is_alphabetic() {
            let ident = self.parse_identifier()?;
            let lower = ident.to_lowercase();
            if lower=="true" { Ok(Expr::Literal(Value::Boolean(true))) }
            else if lower=="false" { Ok(Expr::Literal(Value::Boolean(false))) }
            else if lower=="null" { Ok(Expr::Literal(Value::Null)) }
            else { Ok(Expr::Literal(Value::String(ident))) }
        } else {
            Err(GliteError::Parse(format!("unexpected expr char {}", c)))
        }
    }

    fn parse_delete(&mut self) -> Result<DeleteClause> {
        let mut vars = vec![];
        loop {
            self.skip_ws();
            if self.peek_keyword("RETURN") || self.peek_keyword("SET") || self.peek_keyword("MATCH") || self.peek_keyword("WHERE") || self.peek().is_none() { break; }
            let ident = self.parse_identifier()?;
            vars.push(ident);
            self.skip_ws();
            if self.peek() == Some(',') { self.consume(); continue; } else { break; }
        }
        Ok(DeleteClause{ variables: vars })
    }

    fn parse_return(&mut self) -> Result<ReturnClause> {
        let mut distinct = false;
        if self.consume_str_case_insensitive("DISTINCT") { distinct = true; }
        let mut items = vec![];
        loop {
            self.skip_ws();
            if self.peek().is_none() || self.peek()==Some(';') { break; }
            // Detect shortestPath in return
            if self.peek_keyword("shortestPath") || self.peek_keyword("shortestpath") {
                let sp = self.parse_shortest_path_expr(None)?;
                let alias = if self.consume_str_case_insensitive("AS") { Some(self.parse_identifier()?) } else { None };
                items.push(ReturnItem{ expr: ReturnExpr::ShortestPath(sp), alias });
            } else {
                // Check for p=shortestPath style? Already handled
                // Parse expression (var or var.prop or count)
                let save = self.pos;
                // peek for "count("
                if self.remaining().to_lowercase().starts_with("count(") {
                    self.pos += 5; // "count"
                    self.expect_char('(')?;
                    let var = self.parse_identifier().unwrap_or_default();
                    self.expect_char(')')?;
                    let alias = if self.consume_str_case_insensitive("AS") { Some(self.parse_identifier()?) } else { None };
                    items.push(ReturnItem{ expr: ReturnExpr::Count(var), alias });
                } else {
                    // try var.prop or var
                    let ident = match self.parse_identifier() {
                        Ok(v) => v,
                        Err(_) => break,
                    };
                    self.skip_ws();
                    if self.peek() == Some('.') {
                        self.consume();
                        let prop = self.parse_identifier()?;
                        let alias = if self.consume_str_case_insensitive("AS") { Some(self.parse_identifier()?) } else { None };
                        items.push(ReturnItem{ expr: ReturnExpr::Property{ var: ident, prop }, alias });
                    } else if self.peek() == Some('=') {
                        // p = shortestPath ...
                        self.consume();
                        self.skip_ws();
                        if self.peek_keyword("shortestPath") {
                            let sp = self.parse_shortest_path_expr(Some(ident))?;
                            let alias2 = if self.consume_str_case_insensitive("AS") { Some(self.parse_identifier()?) } else { None };
                            items.push(ReturnItem{ expr: ReturnExpr::ShortestPath(sp), alias: alias2 });
                        } else {
                            // alias assignment?
                            let var2 = self.parse_identifier()?;
                            items.push(ReturnItem{ expr: ReturnExpr::Variable(var2), alias: Some(ident) });
                        }
                    } else {
                        let alias = if self.consume_str_case_insensitive("AS") { Some(self.parse_identifier()?) } else { None };
                        // Could also be "p" that is path variable from earlier shortestPath MATCH
                        // treat as variable
                        items.push(ReturnItem{ expr: ReturnExpr::Variable(ident), alias });
                    }
                    // If after parsing, we detect remaining "shortestPath" without comma? already handled
                    let _ = save;
                }
            }
            self.skip_ws();
            if self.peek() == Some(',') { self.consume(); continue; } else { break; }
        }
        Ok(ReturnClause{ items, distinct })
    }

    fn parse_shortest_path_expr(&mut self, preset_var: Option<String>) -> Result<ShortestPathSpec> {
        // expect shortestPath '(' ... ')' with optional weight/algorithm args: shortestPath((a)-[:TYPE*]->(b), 'weight', 'bidirectional')
        self.consume_str_case_insensitive("shortestPath");
        self.expect_char('(')?;
        self.skip_ws();
        // Inside: (a)-[:KNOWS*]->(b) or (a)-[*]-(b)
        let path = self.parse_pattern_path()?;
        self.skip_ws();
        // Optional comma-separated extra args: weight prop and/or algorithm
        let mut weight_prop: Option<String> = None;
        let mut algorithm = PathAlgorithm::Bfs;
        // Check for additional args after pattern: ',' weight ',' algo
        if self.peek() == Some(',') {
            self.consume();
            self.skip_ws();
            // Parse weight prop: could be string literal 'weight' or identifier or $param
            if self.peek() == Some('\'') || self.peek() == Some('"') {
                let w = self.parse_string_literal()?;
                if !w.is_empty() {
                    weight_prop = Some(w.clone());
                    algorithm = PathAlgorithm::Dijkstra;
                }
            } else if self.peek() == Some('$') {
                self.consume();
                let p = self.parse_identifier()?;
                weight_prop = Some(p.clone());
                algorithm = PathAlgorithm::Dijkstra;
            } else if self.peek().map(|c| c.is_alphabetic() || c=='_').unwrap_or(false) {
                // could be weight identifier or algorithm name
                let ident = self.parse_identifier()?;
                let lower = ident.to_lowercase();
                if lower == "bidirectional" || lower == "bfs" || lower == "dijkstra" {
                    algorithm = match lower.as_str() {
                        "bidirectional" => PathAlgorithm::Bidirectional,
                        "dijkstra" => PathAlgorithm::Dijkstra,
                        _ => PathAlgorithm::Bfs,
                    };
                    // if dijkstra without weight, default weight prop "weight"
                    if algorithm == PathAlgorithm::Dijkstra && weight_prop.is_none() {
                        weight_prop = Some("weight".to_string());
                    }
                } else {
                    weight_prop = Some(ident.clone());
                    algorithm = PathAlgorithm::Dijkstra;
                }
            }
            self.skip_ws();
            // Check for second optional arg (algorithm) if first was weight
            if self.peek() == Some(',') {
                self.consume();
                self.skip_ws();
                if self.peek().map(|c| c.is_alphabetic() || c=='_').unwrap_or(false) {
                    let ident2 = self.parse_identifier()?;
                    let lower2 = ident2.to_lowercase();
                    algorithm = match lower2.as_str() {
                        "bidirectional" => PathAlgorithm::Bidirectional,
                        "dijkstra" => PathAlgorithm::Dijkstra,
                        "bfs" => PathAlgorithm::Bfs,
                        _ => algorithm,
                    };
                    if algorithm == PathAlgorithm::Dijkstra && weight_prop.is_none() {
                        weight_prop = Some("weight".to_string());
                    }
                } else if self.peek() == Some('\'') || self.peek() == Some('"') {
                    let a = self.parse_string_literal()?;
                    let lower = a.to_lowercase();
                    algorithm = match lower.as_str() {
                        "bidirectional" => PathAlgorithm::Bidirectional,
                        "dijkstra" => PathAlgorithm::Dijkstra,
                        _ => PathAlgorithm::Bfs,
                    };
                }
            }
        } else {
            // No extra args: try to infer weight from edge properties (e.g., [:TYPE {weight: 1}])
            // Already captured in path.edges[0].weight_property ? check edge props
            if !path.edges.is_empty() {
                let ep = &path.edges[0];
                // If edge has property named weight or cost etc, use it?
                // Alternatively, if inner edge specified weight_property via bracket syntax we haven't captured.
                // For now check if edge variable has weight_property set (currently not set)
                if ep.weight_property.is_some() {
                    weight_prop = ep.weight_property.clone();
                    algorithm = PathAlgorithm::Dijkstra;
                }
                // Also detect algorithm hint in raw input near shortestPath (e.g., "algo=bidirectional")
                // Simple heuristic: if original input contains "bidirectional" case-insensitive, use it
                if self.input.to_lowercase().contains("bidirectional") {
                    algorithm = PathAlgorithm::Bidirectional;
                }
            }
        }
        self.skip_ws();
        self.expect_char(')')?;
        // Extract start/end vars, edge type/direction
        if path.nodes.len() < 2 { return Err(GliteError::Parse("shortestPath requires at least two nodes".into())) }
        let start_var = path.nodes[0].variable.clone().unwrap_or_else(|| "a".to_string());
        let end_var = path.nodes.last().unwrap().variable.clone().unwrap_or_else(|| "b".to_string());
        let edge_type = if !path.edges.is_empty() { path.edges[0].edge_type.clone() } else { None };
        let direction = if !path.edges.is_empty() { path.edges[0].direction.clone() } else { Direction::Undirected };
        let weighted = weight_prop.is_some();
        // If weight specified but algorithm still Bfs, promote to Dijkstra
        if weighted && algorithm == PathAlgorithm::Bfs {
            algorithm = PathAlgorithm::Dijkstra;
        }
        Ok(ShortestPathSpec{ path_var: preset_var, start_var, end_var, edge_type, direction, weighted, weight_prop, algorithm })
    }

    fn try_parse_shortest_path_assignment(&mut self) -> Option<ShortestPathSpec> {
        let saved = self.pos;
        // try parse: ident = shortestPath ( pattern )
        // peek identifier
        let ident = match self.parse_identifier() {
            Ok(v) => v,
            Err(_) => { self.pos = saved; return None; }
        };
        self.skip_ws();
        if self.peek() != Some('=') { self.pos = saved; return None; }
        self.consume();
        self.skip_ws();
        if !self.consume_str_case_insensitive("shortestPath") { self.pos = saved; return None; }
        self.skip_ws();
        if self.peek() != Some('(') { self.pos = saved; return None; }
        // Now parse shortestPath
        match self.parse_shortest_path_expr(Some(ident.clone())) {
            Ok(sp) => Some(sp),
            Err(_) => { self.pos = saved; None }
        }
    }
}

fn parse_node_content(content: &str) -> Result<PatternNode> {
    let s = content.trim();
    let mut variable = None;
    let mut labels = vec![];
    let mut properties = HashMap::new();

    if s.is_empty() { return Ok(PatternNode{ variable, labels, properties }) }

    // Find properties part {...}
    let prop_idx = s.find('{');
    let (head, prop_part) = if let Some(idx) = prop_idx {
        let p = &s[idx..];
        (&s[..idx], Some(p))
    } else { (s, None) };

    let head = head.trim();
    if !head.is_empty() {
        // head may be "var:Label:Label2"
        // split by ':'
        let parts: Vec<&str> = head.split(':').collect();
        let first = parts[0].trim();
        if !first.is_empty() {
            variable = Some(first.to_string());
        }
        for p in parts.iter().skip(1) {
            let label = p.trim();
            if !label.is_empty() {
                labels.push(label.to_string());
            }
        }
        // Handle case where head starts with ':' -> variable None, label present
        if head.starts_with(':') {
            variable = None;
            // first part is empty, we already added labels from skip(1), but need to include first label if head like ":Person"
            // Our split gives ["", "Person"], skip(1) gives "Person" ok
        }
    }

    if let Some(pstr) = prop_part {
        let map = parse_properties_map(pstr)?;
        properties = map;
    }

    Ok(PatternNode{ variable, labels, properties })
}

fn parse_properties_map(s: &str) -> Result<HashMap<String, Expr>> {
    let mut map = HashMap::new();
    let trimmed = s.trim();
    let inner = if trimmed.starts_with('{') && trimmed.ends_with('}') {
        &trimmed[1..trimmed.len()-1]
    } else {
        return Err(GliteError::Parse(format!("invalid properties map: {}", s)));
    };
    if inner.trim().is_empty() { return Ok(map); }
    // Split by commas not inside quotes
    let mut parts = vec![];
    let mut cur = String::new();
    let mut in_str = false;
    let mut str_char = '\'';
    for c in inner.chars() {
        if (c=='\'' || c=='"') && !in_str { in_str=true; str_char=c; cur.push(c); }
        else if c==str_char && in_str { in_str=false; cur.push(c); }
        else if c==',' && !in_str { parts.push(cur.clone()); cur.clear(); }
        else { cur.push(c); }
    }
    if !cur.is_empty() { parts.push(cur); }

    for part in parts {
        let kv: Vec<&str> = part.splitn(2, ':').collect();
        if kv.len()!=2 { continue; }
        let k = kv[0].trim().trim_matches('"').trim_matches('\'').to_string();
        let v_raw = kv[1].trim();
        let expr = if v_raw.starts_with('$') {
            Expr::Param(v_raw[1..].trim().to_string())
        } else if (v_raw.starts_with('\'') && v_raw.ends_with('\'')) || (v_raw.starts_with('"') && v_raw.ends_with('"')) {
            let inner_val = &v_raw[1..v_raw.len()-1];
            Expr::Literal(Value::String(inner_val.to_string()))
        } else if v_raw.eq_ignore_ascii_case("true") {
            Expr::Literal(Value::Boolean(true))
        } else if v_raw.eq_ignore_ascii_case("false") {
            Expr::Literal(Value::Boolean(false))
        } else if v_raw.eq_ignore_ascii_case("null") {
            Expr::Literal(Value::Null)
        } else if let Ok(i) = v_raw.parse::<i64>() {
            Expr::Literal(Value::Integer(i))
        } else if let Ok(f) = v_raw.parse::<f64>() {
            Expr::Literal(Value::Float(f))
        } else {
            // treat as string param? Could be identifier
            Expr::Literal(Value::String(v_raw.to_string()))
        };
        map.insert(k, expr);
    }
    Ok(map)
}

pub fn eval_expr(expr: &Expr, params: &HashMap<String, Value>) -> Result<Value> {
    match expr {
        Expr::Literal(v) => Ok(v.clone()),
        Expr::Param(name) => {
            params.get(name).cloned().ok_or_else(|| GliteError::Parse(format!("missing param ${}", name)))
        }
    }
}
