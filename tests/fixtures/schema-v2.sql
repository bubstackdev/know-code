
CREATE TABLE IF NOT EXISTS generations(id INTEGER PRIMARY KEY);
CREATE TABLE IF NOT EXISTS state(singleton INTEGER PRIMARY KEY CHECK(singleton=1), current_generation INTEGER);
INSERT OR IGNORE INTO state VALUES(1,NULL);
CREATE TABLE IF NOT EXISTS contents(hash TEXT PRIMARY KEY, utf8 BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS compilations(
 generation INTEGER REFERENCES generations ON DELETE CASCADE, id TEXT, language TEXT,
 project_path TEXT, target TEXT, configuration TEXT, inferred INTEGER, compiler_version TEXT, options_json TEXT,
 dependencies_json TEXT NOT NULL DEFAULT '[]', input_paths_json TEXT NOT NULL DEFAULT '[]',
 PRIMARY KEY(generation,id));
CREATE TABLE IF NOT EXISTS artifacts(
 generation INTEGER REFERENCES generations ON DELETE CASCADE, id TEXT, path TEXT,
 content_hash TEXT REFERENCES contents, generated INTEGER, PRIMARY KEY(generation,id));
CREATE INDEX IF NOT EXISTS artifact_path ON artifacts(path,generation);
CREATE TABLE IF NOT EXISTS generation_units(
 generation INTEGER REFERENCES generations ON DELETE CASCADE, compilation_id TEXT, owner_generation INTEGER,
 PRIMARY KEY(generation,compilation_id),
 FOREIGN KEY(owner_generation,compilation_id) REFERENCES compilations(generation,id));
CREATE TABLE IF NOT EXISTS unit_artifacts(
 owner_generation INTEGER, compilation_id TEXT, artifact_id TEXT,
 PRIMARY KEY(owner_generation,compilation_id,artifact_id),
 FOREIGN KEY(owner_generation,compilation_id) REFERENCES compilations(generation,id) ON DELETE CASCADE,
 FOREIGN KEY(owner_generation,artifact_id) REFERENCES artifacts(generation,id) ON DELETE CASCADE);
CREATE TABLE IF NOT EXISTS symbols(
 rowid INTEGER PRIMARY KEY, generation INTEGER, compilation_id TEXT, id TEXT, name TEXT, kind TEXT, signature TEXT, external INTEGER,
 UNIQUE(generation,compilation_id,id),
 FOREIGN KEY(generation,compilation_id) REFERENCES compilations ON DELETE CASCADE);
CREATE INDEX IF NOT EXISTS symbol_name ON symbols(generation,name);
CREATE TABLE IF NOT EXISTS occurrences(
 generation INTEGER, compilation_id TEXT, symbol_id TEXT, artifact_id TEXT, start_byte INTEGER,
 end_byte INTEGER CHECK(end_byte>=start_byte AND start_byte>=0), role TEXT, enclosing_symbol_id TEXT,
 FOREIGN KEY(generation,compilation_id,symbol_id) REFERENCES symbols(generation,compilation_id,id) ON DELETE CASCADE,
 FOREIGN KEY(generation,artifact_id) REFERENCES artifacts ON DELETE CASCADE,
 UNIQUE(generation,compilation_id,symbol_id,artifact_id,start_byte,end_byte,role));
CREATE INDEX IF NOT EXISTS occurrence_symbol ON occurrences(generation,symbol_id,role);
CREATE TABLE IF NOT EXISTS relationships(
 generation INTEGER, compilation_id TEXT, source_id TEXT, target_id TEXT, kind TEXT,
 FOREIGN KEY(generation,compilation_id,source_id) REFERENCES symbols(generation,compilation_id,id) ON DELETE CASCADE,
 FOREIGN KEY(generation,compilation_id,target_id) REFERENCES symbols(generation,compilation_id,id) ON DELETE CASCADE,
 UNIQUE(generation,compilation_id,source_id,target_id,kind));
CREATE TABLE IF NOT EXISTS reports(
 generation INTEGER, compilation_id TEXT, status TEXT, diagnostics TEXT,
 PRIMARY KEY(generation,compilation_id),
 FOREIGN KEY(generation,compilation_id) REFERENCES compilations ON DELETE CASCADE);
CREATE VIRTUAL TABLE IF NOT EXISTS symbol_search USING fts5(name,signature,content='symbols',content_rowid='rowid');
CREATE TRIGGER IF NOT EXISTS symbols_insert AFTER INSERT ON symbols BEGIN
 INSERT INTO symbol_search(rowid,name,signature) VALUES(new.rowid,new.name,new.signature); END;
CREATE TRIGGER IF NOT EXISTS symbols_delete AFTER DELETE ON symbols BEGIN
 INSERT INTO symbol_search(symbol_search,rowid,name,signature) VALUES('delete',old.rowid,old.name,old.signature); END;
PRAGMA user_version=2;
