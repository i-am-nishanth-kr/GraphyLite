"""
GraphyLite Python example — requires `maturin develop` or `pip install -e .`
"""
from pygraphylite import PyGraphyLite

# In-memory DB for testing — use PyGraphyLite("graph.glite") for file
db = PyGraphyLite.open_in_memory()

# CREATE
db.execute("CREATE (a:Person {name: $name})", {"name": "Alice"})
db.execute("CREATE (b:Person {name: $name})", {"name": "Bob"})
print("labels:", db.list_labels())
print("schema:", db.schema())

# MATCH + CREATE edge (R1)
db.execute("MATCH (a:Person {name: $a}), (b:Person {name: $b}) CREATE (a)-[:KNOWS]->(b)",
           {"a": "Alice", "b": "Bob"})

# QUERY
rows = db.query("MATCH (n:Person) RETURN n")
print(f"Persons: {len(rows)}")
for r in rows:
    print(r["n"])  # {'id': 1, 'labels': ['Person'], 'properties': {'name': 'Alice'}}

# WHERE
rows = db.query("MATCH (n:Person) WHERE n.name = $name RETURN n", {"name": "Alice"})
print("WHERE Alice:", rows)

# SET / DELETE
db.execute("MATCH (n:Person {name: $name}) SET n.age = $age", {"name": "Alice", "age": 30})
print(db.query("MATCH (n:Person {name: 'Alice'}) RETURN n.age"))

# COUNT
print(db.query("MATCH (n:Person) RETURN count(n)"))

# Variable-length + shortestPath (R3/R4)
db.execute("CREATE (c:Person {name: 'Carol'})")
db.execute("MATCH (b:Person {name: 'Bob'}), (c:Person {name: 'Carol'}) CREATE (b)-[:KNOWS]->(c)")
print("shortestPath BFS:", db.query("MATCH (a:Person {name: 'Alice'}), (c:Person {name: 'Carol'}) RETURN shortestPath((a)-[:KNOWS*]->(c))"))

# Weighted Dijkstra
db2 = PyGraphyLite.open_in_memory()
db2.execute("CREATE (a:Node {id: 1})")
db2.execute("CREATE (b:Node {id: 2})")
db2.execute("CREATE (c:Node {id: 3})")
db2.execute("MATCH (a:Node {id: 1}), (b:Node {id: 2}) CREATE (a)-[:ROAD {weight: 5}]->(b)")
db2.execute("MATCH (b:Node {id: 2}), (c:Node {id: 3}) CREATE (b)-[:ROAD {weight: 2}]->(c)")
db2.execute("MATCH (a:Node {id: 1}), (c:Node {id: 3}) CREATE (a)-[:ROAD {weight: 10}]->(c)")
print("dijkstra:", db2.query("MATCH (a:Node {id: 1}), (c:Node {id: 3}) RETURN shortestPath((a)-[:ROAD*]->(c), 'weight')"))

# Pathfinding APIs (direct)
# Need node ids
rows = db2.query("MATCH (n:Node {id: 1}) RETURN n")
a_id = rows[0]["n"]["id"]
rows = db2.query("MATCH (n:Node {id: 3}) RETURN n")
c_id = rows[0]["n"]["id"]
print("bfs API:", db2.shortest_path_bfs(a_id, c_id, "ROAD", "outgoing"))
print("dijkstra API:", db2.shortest_path_dijkstra(a_id, c_id, "ROAD", "weight", "outgoing"))
print("bidirectional:", db2.shortest_path_bidirectional(a_id, c_id, "ROAD"))

# Prefix scan R5: 50 nodes
for i in range(5):
    db.execute(f"CREATE (x:LabelA {{id: {i}}})")
print("LabelA count:", len(db.query("MATCH (n:LabelA) RETURN n")))
