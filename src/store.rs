use crate::config::Settings;
pub use crate::ranking::SearchMetadata;
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use unicode_normalization::UnicodeNormalization;

pub const DIMENSIONS: usize = 768;
pub const MAX_RESULTS: usize = 5000;
pub fn folded(s: &str) -> String {
    s.nfkc().flat_map(char::to_lowercase).collect()
}
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileRecord {
    #[serde(default)]
    pub search: crate::ranking::SearchMetadata,
    #[serde(default)]
    pub source_id: i64,
    pub uri: String,
    pub path: PathBuf,
    pub name: String,
    pub mime: String,
    pub stamp: String,
    pub size: u64,
    pub folder: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchHit {
    #[serde(skip)]
    pub(crate) duplicate: bool,
    pub uri: String,
    pub path: PathBuf,
    pub name: String,
    pub mime: String,
    pub snippet: String,
    pub page: Option<u32>,
    pub seconds: Option<f64>,
    pub score: f64,
}

/// A query embedding and the pipeline that produced it must travel together.
pub struct SearchQuery<'a> {
    pub text: &'a str,
    pub embedding: Option<SearchEmbedding<'a>>,
}

pub struct SearchEmbedding<'a> {
    pub vector: &'a [f32],
    pub pipeline: &'a str,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Counts {
    pub files: u64,
    pub indexed: u64,
    pub pending: u64,
    pub failed: u64,
    pub skipped: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Activity {
    pub time: i64,
    pub path: String,
    pub state: String,
    pub detail: String,
}

pub struct StoredChunk {
    pub text: String,
    pub vector: Option<Vec<f32>>,
    pub page: Option<u32>,
    pub seconds: Option<f64>,
}

pub struct Store {
    conn: Connection,
}

// Keep one best chunk per file and an ordered, bounded set of its scores.
// Updating or evicting a candidate is logarithmic, including for large result views.
#[derive(Clone, Debug)]
struct Rank {
    score: f64,
    uri: String,
}
impl PartialEq for Rank {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}
impl Eq for Rank {}
impl PartialOrd for Rank {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Rank {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // The worst score sorts first; a later URI loses equal-score ties.
        self.score
            .total_cmp(&other.score)
            .then_with(|| other.uri.cmp(&self.uri))
    }
}
struct SemanticCandidates {
    hits: HashMap<String, SearchHit>,
    order: BTreeSet<Rank>,
    limit: usize,
}
impl SemanticCandidates {
    fn new(limit: usize) -> Self {
        Self {
            hits: HashMap::new(),
            order: BTreeSet::new(),
            limit,
        }
    }
    fn accepts(&self, uri: &str, score: f64) -> bool {
        if let Some(old) = self.hits.get(uri) {
            return score > old.score;
        }
        if self.hits.len() < self.limit {
            return true;
        }
        self.order.first().is_some_and(|worst| {
            score > worst.score || (score == worst.score && uri < worst.uri.as_str())
        })
    }
    fn insert(&mut self, hit: SearchHit) {
        if !self.accepts(&hit.uri, hit.score) {
            return;
        }
        if let Some(old) = self.hits.remove(&hit.uri) {
            self.order.remove(&Rank {
                score: old.score,
                uri: old.uri,
            });
        } else if self.hits.len() == self.limit {
            let worst = self.order.pop_first().unwrap();
            self.hits.remove(&worst.uri);
        }
        self.order.insert(Rank {
            score: hit.score,
            uri: hit.uri.clone(),
        });
        self.hits.insert(hit.uri.clone(), hit);
    }
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA cache_size=-8192; PRAGMA synchronous=NORMAL;")?;
        let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > 4 {
            bail!("This index was created by a newer GSeek; please upgrade the application");
        }
        if version == 0 {
            conn.execute_batch("BEGIN IMMEDIATE;
            CREATE TABLE files(uri TEXT PRIMARY KEY, path TEXT NOT NULL, name TEXT NOT NULL, mime TEXT NOT NULL,
                folded TEXT NOT NULL, stamp TEXT NOT NULL, size INTEGER NOT NULL, folder INTEGER NOT NULL,
                seen INTEGER NOT NULL, state TEXT NOT NULL DEFAULT 'pending', content TEXT, error TEXT NOT NULL DEFAULT '',source_id INTEGER NOT NULL DEFAULT 0);
            CREATE INDEX file_queue ON files(state, size);
            CREATE INDEX file_source ON files(source_id);
            CREATE TABLE contents(id TEXT PRIMARY KEY, pipeline TEXT NOT NULL, created INTEGER NOT NULL, complete INTEGER NOT NULL DEFAULT 1);
            CREATE TABLE chunks(id INTEGER PRIMARY KEY, content TEXT NOT NULL REFERENCES contents(id) ON DELETE CASCADE,
                text TEXT NOT NULL, vector BLOB, page INTEGER, seconds REAL, part INTEGER NOT NULL);
            CREATE INDEX content_chunks ON chunks(content);
            CREATE UNIQUE INDEX chunk_part ON chunks(content,part);
            CREATE VIRTUAL TABLE words USING fts5(text, content='chunks', content_rowid='id', tokenize='unicode61 remove_diacritics 2');
            CREATE TRIGGER chunks_insert AFTER INSERT ON chunks BEGIN INSERT INTO words(rowid,text) VALUES(new.id,new.text); END;
            CREATE TRIGGER chunks_delete AFTER DELETE ON chunks BEGIN INSERT INTO words(words,rowid,text) VALUES('delete',old.id,old.text); END;
            CREATE TABLE activity(time INTEGER NOT NULL, path TEXT NOT NULL, state TEXT NOT NULL, detail TEXT NOT NULL);
            CREATE TABLE metadata(key TEXT PRIMARY KEY, value TEXT NOT NULL);
            PRAGMA user_version=3; COMMIT;")?;
        } else if version == 1 {
            conn.execute_batch("BEGIN IMMEDIATE;
                ALTER TABLE contents ADD COLUMN complete INTEGER NOT NULL DEFAULT 1;
                ALTER TABLE chunks ADD COLUMN part INTEGER;
                WITH ranks AS (SELECT id, row_number() OVER (PARTITION BY content ORDER BY id)-1 AS n FROM chunks)
                UPDATE chunks SET part=(SELECT n FROM ranks WHERE ranks.id=chunks.id);
                CREATE UNIQUE INDEX chunk_part ON chunks(content,part);
                ALTER TABLE files ADD COLUMN source_id INTEGER NOT NULL DEFAULT 0;
                CREATE INDEX file_source ON files(source_id);
                PRAGMA user_version=3; COMMIT;")?;
        }
        if version == 2 {
            conn.execute_batch("BEGIN IMMEDIATE; ALTER TABLE files ADD COLUMN source_id INTEGER NOT NULL DEFAULT 0; CREATE INDEX file_source ON files(source_id); PRAGMA user_version=3; COMMIT;")?;
        }
        if version < 4 {
            let transaction = rusqlite::Transaction::new_unchecked(
                &conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let current: u32 =
                transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            if current > 4 {
                bail!("This index was created by a newer GSeek; please upgrade the application");
            }
            if current < 4 {
                transaction.execute_batch(
                    "
                ALTER TABLE files ADD COLUMN modified INTEGER NOT NULL DEFAULT 0;
                ALTER TABLE files ADD COLUMN accessed INTEGER NOT NULL DEFAULT 0;
                PRAGMA user_version=4;",
                )?;
            }
            transaction.commit()?;
        }
        Ok(Self { conn })
    }

    pub fn start_scan(&self) -> Result<i64> {
        // A monotonic scan ID avoids collisions between scans within one second.
        self.conn.execute("INSERT INTO metadata VALUES('scan','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1", [])?;
        Ok(self.conn.query_row(
            "SELECT CAST(value AS INTEGER) FROM metadata WHERE key='scan'",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn activate_pipeline(&self, pipeline: &str) -> Result<()> {
        let old: Option<String> = self
            .conn
            .query_row("SELECT value FROM metadata WHERE key='pipeline'", [], |r| {
                r.get(0)
            })
            .optional()?;
        if old.as_deref() != Some(pipeline) {
            self.conn.execute(
                "UPDATE files SET state='pending',error='' WHERE folder=0",
                [],
            )?;
            self.conn.execute("INSERT INTO metadata VALUES('pipeline',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[pipeline])?;
        }
        Ok(())
    }

    pub fn discover(&self, file: &FileRecord, generation: i64) -> Result<()> {
        // Check the committed policy under the same write transaction as insertion.
        // A scan that started before an exclusion cannot put its files back.
        let tx = if self.conn.is_autocommit() {
            Some(rusqlite::Transaction::new_unchecked(
                &self.conn,
                rusqlite::TransactionBehavior::Immediate,
            )?)
        } else {
            None
        };
        let scope: Option<String> = self
            .conn
            .query_row("SELECT value FROM metadata WHERE key='scope'", [], |r| {
                r.get(0)
            })
            .optional()?;
        if let Some(scope) = scope
            && !serde_json::from_str::<Settings>(&scope)?.allows(&file.path)
        {
            return Ok(());
        }
        self.conn.execute("INSERT INTO files(uri,path,name,mime,folded,stamp,size,folder,seen,state,source_id)
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
            ON CONFLICT(uri) DO UPDATE SET path=excluded.path,name=excluded.name,mime=excluded.mime,folded=excluded.folded,
            size=excluded.size,folder=excluded.folder,seen=CASE WHEN ?9<0 THEN files.seen ELSE excluded.seen END,source_id=CASE WHEN excluded.source_id!=0 THEN excluded.source_id ELSE files.source_id END,
            state=CASE WHEN files.stamp!=excluded.stamp THEN excluded.state ELSE files.state END,accessed=CASE WHEN files.stamp=excluded.stamp THEN files.accessed ELSE 0 END,stamp=excluded.stamp",
            params![file.uri, file.path.to_string_lossy(), file.name, file.mime, folded(&file.path.to_string_lossy()),
                file.stamp, i64::try_from(file.size).unwrap_or(i64::MAX), file.folder, generation, if file.folder { "indexed" } else { "pending" },file.source_id])?;
        self.conn.execute(
            "UPDATE files SET modified=?2,accessed=CASE WHEN ?3>0 THEN ?3 ELSE accessed END WHERE uri=?1",
            params![file.uri, file.search.modified, file.search.accessed],
        )?;
        if let Some(tx) = tx {
            tx.commit()?;
        }
        Ok(())
    }

    pub fn finish_scan(&self, generation: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM files WHERE seen!=?1", [generation])?;
        // Keep content-addressed vectors briefly, so renames and moves don't require inference again.
        self.conn.execute("DELETE FROM contents WHERE created<?1 AND id NOT IN (SELECT content FROM files WHERE content IS NOT NULL)", [now()-7*86400])?;
        Ok(())
    }
    pub fn refresh(&self, file: &FileRecord) -> Result<()> {
        self.discover(file, -1)
    }
    pub fn remove_file(&self, uri: &str) -> Result<()> {
        self.conn.execute("DELETE FROM files WHERE uri=?1", [uri])?;
        Ok(())
    }

    pub fn apply_changes(&self, ids: &[i64], records: &[FileRecord]) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let seen: std::collections::HashSet<_> = records.iter().map(|f| f.uri.as_str()).collect();
        for id in ids {
            let mut stmt = tx.prepare("SELECT uri FROM files WHERE source_id=?1")?;
            let old = stmt
                .query_map([id], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for uri in old {
                if !seen.contains(uri.as_str()) {
                    tx.execute("DELETE FROM files WHERE uri=?1", [uri])?;
                }
            }
        }
        let generation = self
            .conn
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM metadata WHERE key='scan'",
                [],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        for file in records {
            self.discover(file, generation)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn prune_scope(&self, settings: &Settings) -> Result<()> {
        let settings = settings.normalized()?;
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        self.conn.execute("INSERT INTO metadata VALUES('scope',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [serde_json::to_string(&settings)?])?;
        let mut stmt = self.conn.prepare("SELECT uri,path FROM files")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (uri, path) = row?;
            if !settings.allows(Path::new(&path)) {
                self.conn.execute("DELETE FROM files WHERE uri=?1", [uri])?;
            }
        }
        drop(stmt);
        // Exclusions are deletion requests, including vectors and resumable parts.
        // Content still referenced by an included duplicate is retained.
        self.conn.execute("DELETE FROM contents WHERE id NOT IN (SELECT content FROM files WHERE content IS NOT NULL)", [])?;
        let mut stmt = self.conn.prepare("SELECT DISTINCT path FROM activity")?;
        let paths = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        for path in paths {
            let file = url::Url::parse(&path)
                .ok()
                .and_then(|u| u.to_file_path().ok())
                .unwrap_or_else(|| PathBuf::from(&path));
            if file.is_absolute() && !settings.allows(&file) {
                self.conn
                    .execute("DELETE FROM activity WHERE path=?1", [path])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn next_pending(&self) -> Result<Option<FileRecord>> {
        Ok(self.conn.query_row("SELECT uri,path,name,mime,stamp,size,folder FROM files WHERE state='pending' ORDER BY size,uri LIMIT 1", [], Self::file_row).optional()?)
    }

    fn file_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileRecord> {
        Ok(FileRecord {
            search: Default::default(),
            source_id: 0,
            uri: row.get(0)?,
            path: PathBuf::from(row.get::<_, String>(1)?),
            name: row.get(2)?,
            mime: row.get(3)?,
            stamp: row.get(4)?,
            size: row.get::<_, i64>(5)?.max(0) as u64,
            folder: row.get(6)?,
        })
    }

    pub fn file(&self, uri: &str) -> Result<Option<FileRecord>> {
        Ok(self
            .conn
            .query_row(
                "SELECT uri,path,name,mime,stamp,size,folder FROM files WHERE uri=?1",
                [uri],
                Self::file_row,
            )
            .optional()?)
    }

    pub fn cached(&self, content: &str, pipeline: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM contents WHERE id=?1 AND pipeline=?2 AND complete=1",
                params![content, pipeline],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false))
    }

    pub fn save_content(
        &mut self,
        content: &str,
        pipeline: &str,
        chunks: &[StoredChunk],
    ) -> Result<()> {
        for chunk in chunks {
            if let Some(v) = &chunk.vector {
                validate_vector(v)?;
            }
        }
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM contents WHERE id=?1", [content])?;
        tx.execute(
            "INSERT INTO contents(id,pipeline,created,complete) VALUES(?1,?2,?3,1)",
            params![content, pipeline, now()],
        )?;
        for (part, chunk) in chunks.iter().enumerate() {
            let bytes = chunk
                .vector
                .as_ref()
                .map(|v| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>());
            tx.execute(
                "INSERT INTO chunks(content,text,vector,page,seconds,part) VALUES(?1,?2,?3,?4,?5,?6)",
                params![content, chunk.text, bytes, chunk.page, chunk.seconds,part as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn content_progress(&self, content: &str) -> Result<usize> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM chunks WHERE content=?1",
            [content],
            |r| r.get::<_, i64>(0),
        )? as usize)
    }
    pub fn checkpoint(
        &self,
        content: &str,
        pipeline: &str,
        part: usize,
        chunk: &StoredChunk,
    ) -> Result<()> {
        if let Some(v) = &chunk.vector {
            validate_vector(v)?;
        }
        self.conn.execute(
            "INSERT OR IGNORE INTO contents(id,pipeline,created,complete) VALUES(?1,?2,?3,0)",
            params![content, pipeline, now()],
        )?;
        let bytes = chunk
            .vector
            .as_ref()
            .map(|v| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>());
        self.conn.execute("INSERT OR IGNORE INTO chunks(content,text,vector,page,seconds,part) VALUES(?1,?2,?3,?4,?5,?6)",params![content,chunk.text,bytes,chunk.page,chunk.seconds,part as i64])?;
        Ok(())
    }
    pub fn checkpoint_for(
        &self,
        file: &FileRecord,
        content: &str,
        pipeline: &str,
        part: usize,
        chunk: &StoredChunk,
    ) -> Result<bool> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let present = self
            .conn
            .query_row(
                "SELECT 1 FROM files WHERE uri=?1 AND stamp=?2",
                params![file.uri, file.stamp],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !present {
            return Ok(false);
        }
        self.checkpoint(content, pipeline, part, chunk)?;
        self.conn.execute(
            "UPDATE files SET content=?2 WHERE uri=?1",
            params![file.uri, content],
        )?;
        tx.commit()?;
        Ok(true)
    }
    pub fn finish_content(&self, content: &str) -> Result<()> {
        self.conn
            .execute("UPDATE contents SET complete=1 WHERE id=?1", [content])?;
        Ok(())
    }
    pub fn discard_incomplete(&self, content: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM contents WHERE id=?1 AND complete=0", [content])?;
        Ok(())
    }

    pub fn complete(&self, uri: &str, content: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE files SET content=?2,state='indexed',error='' WHERE uri=?1",
            params![uri, content],
        )?;
        if changed > 0 {
            self.log(uri, "indexed", "Ready to search")?;
        }
        Ok(())
    }

    pub fn mark(&self, uri: &str, state: &str, detail: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE files SET state=?2,error=?3 WHERE uri=?1",
            params![uri, state, detail],
        )?;
        if changed > 0 {
            self.log(uri, state, detail)?;
        }
        Ok(())
    }

    pub fn retry(&self, location: Option<&Path>, failed_only: bool) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let mut stmt = self
            .conn
            .prepare("SELECT uri,path,state,content FROM files WHERE folder=0")?;
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })? {
            let (uri, path, state, content) = row?;
            if location.is_none_or(|p| Path::new(&path).starts_with(p))
                && (!failed_only || state == "failed")
            {
                if !failed_only && let Some(content) = content {
                    self.conn
                        .execute("DELETE FROM contents WHERE id=?1", [content])?;
                }
                self.conn.execute(
                    "UPDATE files SET state='pending',error='' WHERE uri=?1",
                    [uri],
                )?;
            }
        }
        drop(stmt);
        tx.commit()?;
        Ok(())
    }

    pub fn reconsider_skipped(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET state='pending',error='' WHERE state='skipped' AND folder=0",
            [],
        )?;
        Ok(())
    }

    pub fn counts(&self) -> Result<Counts> {
        let mut c = Counts::default();
        let mut stmt = self
            .conn
            .prepare("SELECT state,count(*) FROM files GROUP BY state")?;
        for row in stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
        })? {
            let (state, n) = row?;
            c.files += n;
            match state.as_str() {
                "indexed" => c.indexed = n,
                "pending" => c.pending = n,
                "failed" => c.failed = n,
                "skipped" => c.skipped = n,
                _ => (),
            }
        }
        Ok(c)
    }

    pub fn log(&self, path: &str, state: &str, detail: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO activity VALUES(?1,?2,?3,?4)",
            params![now(), path, state, detail],
        )?;
        self.conn.execute(
            "DELETE FROM activity WHERE rowid <= (SELECT max(rowid)-2000 FROM activity)",
            [],
        )?;
        Ok(())
    }

    pub fn activity(&self) -> Result<Vec<Activity>> {
        let mut stmt = self
            .conn
            .prepare("SELECT time,path,state,detail FROM activity ORDER BY rowid DESC LIMIT 200")?;
        Ok(stmt
            .query_map([], |r| {
                Ok(Activity {
                    time: r.get(0)?,
                    path: r.get(1)?,
                    state: r.get(2)?,
                    detail: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn search(
        &self,
        query: SearchQuery<'_>,
        settings: &Settings,
        filenames: &[FileRecord],
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let terms: Vec<String> = folded(query.text.trim())
            .split_whitespace()
            .take(32)
            .map(str::to_string)
            .collect();
        if terms.is_empty() {
            return Ok(vec![]);
        }
        if let Some(embedding) = &query.embedding {
            validate_vector(embedding.vector)?;
        }
        let limit = limit.clamp(1, MAX_RESULTS);
        let mut hits = HashMap::<String, SearchHit>::new();
        // Collect every source before scoring or truncating. A filename from
        // LocalSearch may corroborate a content candidate below its own top N.
        for file in filenames {
            hits.insert(
                file.uri.clone(),
                SearchHit {
                    duplicate: false,
                    uri: file.uri.clone(),
                    path: file.path.clone(),
                    name: file.name.clone(),
                    mime: file.mime.clone(),
                    snippet: String::new(),
                    page: None,
                    seconds: None,
                    score: crate::ranking::filename(query.text, &file.name, &file.path),
                },
            );
        }
        // Bound candidates, but preserve AND matching for filenames/paths.
        let mut sql = "SELECT uri,path,name,mime FROM files WHERE ".to_owned();
        for i in 0..terms.len() {
            if i > 0 {
                sql.push_str(" AND ");
            }
            sql.push_str(&format!("instr(folded,?{})>0", i + 1));
        }
        sql.push_str(&format!(" ORDER BY length(name),uri LIMIT {MAX_RESULTS}"));
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(&terms))?;
        while let Some(r) = rows.next()? {
            let mut h = hit_row(r, "", None, None, 0.0)?;
            h.score = crate::ranking::filename(query.text, &h.name, &h.path);
            keep_hit(&mut hits, h, settings);
        }
        let fts = terms
            .iter()
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND ");
        let mut stmt = self.conn.prepare(&format!(
            "WITH matches AS MATERIALIZED (
                SELECT rowid,bm25(words) AS strength FROM words WHERE words MATCH ?1)
            SELECT f.uri,f.path,f.name,f.mime,c.text,c.page,c.seconds,MIN(m.strength)
            FROM matches m JOIN chunks c ON c.id=m.rowid JOIN files f ON f.content=c.content
            WHERE f.state='indexed' GROUP BY f.uri ORDER BY MIN(m.strength),f.uri LIMIT {MAX_RESULTS}"
        ))?;
        let mut best_word_strength = 0.0_f64;
        for row in stmt.query_map([fts], |r| {
            Ok((
                hit_row(r, &r.get::<_, String>(4)?, r.get(5)?, r.get(6)?, 0.0)?,
                r.get::<_, f64>(7)?,
            ))
        })? {
            let (mut h, raw) = row?;
            let strength = (-raw).max(0.0);
            best_word_strength = best_word_strength.max(strength);
            let relative = if best_word_strength > 0.0 {
                strength / best_word_strength
            } else {
                0.0
            };
            let name = crate::ranking::filename(query.text, &h.name, &h.path);
            h.score =
                name.max(0.35 + 0.20 * relative) + if name > 0.0 { 0.12 * relative } else { 0.0 };
            keep_hit(&mut hits, h, settings);
        }
        let mut lexical_semantics = HashMap::<String, SearchHit>::new();
        let mut best_semantic = 0.0_f64;
        let mut semantic = SemanticCandidates::new(MAX_RESULTS);
        if let Some(embedding) = &query.embedding {
            let v = embedding.vector;
            // Stream vectors from SQLite's bounded page cache; never materialize the vector database in memory.
            let mut stmt = self.conn.prepare("SELECT f.uri,f.path,f.name,f.mime,c.text,c.page,c.seconds,c.vector FROM chunks c
                JOIN files f ON f.content=c.content JOIN contents p ON p.id=c.content WHERE c.vector IS NOT NULL AND p.pipeline=?1")?;
            let mut rows = stmt.query([embedding.pipeline])?;
            while let Some(r) = rows.next()? {
                let bytes: Vec<u8> = r.get(7)?;
                if bytes.len() != DIMENSIONS * 4 {
                    continue;
                }
                let score: f64 = bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(v)
                    .map(|(b, q)| f32::from_le_bytes(*b) as f64 * *q as f64)
                    .sum();
                if score < crate::ranking::SEMANTIC_FLOOR || !score.is_finite() {
                    continue;
                }
                let path: String = r.get(1)?;
                if !settings.allows(Path::new(&path)) {
                    continue;
                }
                let uri: String = r.get(0)?;
                if hits.contains_key(&uri) {
                    if !search_path_allowed(Path::new(&path), settings) {
                        continue;
                    }
                    if lexical_semantics
                        .get(&uri)
                        .is_none_or(|old| score > old.score)
                    {
                        lexical_semantics.insert(
                            uri,
                            hit_row(r, &r.get::<_, String>(4)?, r.get(5)?, r.get(6)?, score)?,
                        );
                    }
                    best_semantic = best_semantic.max(score);
                    continue;
                }
                if !semantic.accepts(&uri, score) {
                    continue;
                }
                let hit = hit_row(r, &r.get::<_, String>(4)?, r.get(5)?, r.get(6)?, score)?;
                if !semantic.hits.contains_key(&uri) && !search_path_allowed(&hit.path, settings) {
                    continue;
                }
                best_semantic = best_semantic.max(score);
                semantic.insert(hit);
            }
        }
        // EmbeddingGemma's unrelated document pairs can still have positive cosine scores.
        // Preserve lexical hits, but omit weak semantic neighbours rather than filling the view.
        let cutoff = (best_semantic - 0.12).max(0.60);
        for (uri, hit) in &mut hits {
            let lexical = hit.score;
            let matched = lexical_semantics.remove(uri).filter(|h| h.score >= cutoff);
            let cosine = matched.as_ref().map(|h| h.score);
            if let Some(semantic_hit) = matched {
                // Opening a corroborated result should jump to its best semantic
                // passage, even if its filename also matched the query.
                *hit = semantic_hit;
            }
            hit.score = crate::ranking::combined(lexical, cosine);
        }
        let mut hits: Vec<_> = hits.into_values().collect();
        for mut hit in semantic.hits.into_values().filter(|h| h.score >= cutoff) {
            hit.score = crate::ranking::combined(0.0, Some(hit.score));
            hits.push(hit);
        }
        // Filename-only locations (e.g. Downloads) follow GNOME's broader scope.
        hits.retain(|h| {
            settings.allows_filename(&h.path)
                && std::fs::canonicalize(&h.path).is_ok_and(|p| settings.allows_filename(&p))
        });
        let metadata: HashMap<_, _> = filenames
            .iter()
            .map(|file| (file.uri.as_str(), file))
            .collect();
        let mut details = self
            .conn
            .prepare("SELECT modified,accessed,content,stamp,state FROM files WHERE uri=?1")?;
        let mut identities = HashMap::new();
        let at = now();
        for hit in &mut hits {
            let stored = details
                .query_row([&hit.uri], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                })
                .optional()?;
            let mut evidence = crate::ranking::SearchMetadata::default();
            if let Some((modified, accessed, content, stamp, state)) = stored {
                evidence.modified = modified;
                evidence.accessed = accessed;
                if state == "indexed"
                    && let Some(content) = content
                {
                    let current = metadata
                        .get(hit.uri.as_str())
                        .map(|f| f.stamp.clone())
                        .or_else(|| {
                            std::fs::metadata(&hit.path)
                                .ok()
                                .map(|m| crate::discovery::file_stamp(&m))
                        });
                    if current.as_deref() == Some(&stamp) {
                        identities.insert(hit.uri.clone(), content);
                    }
                }
            }
            if let Some(file) = metadata.get(hit.uri.as_str()) {
                evidence = file.search.clone();
            }
            hit.score += crate::ranking::context(query.text, &hit.name, &evidence, at);
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.uri.cmp(&b.uri)));
        let mut seen = std::collections::HashSet::new();
        for hit in &mut hits {
            if let Some(identity) = identities.get(&hit.uri) {
                hit.duplicate = !seen.insert(identity);
            }
        }
        // The full window keeps every copy, after distinct results. Overview
        // filters the flagged copies without inventing a grouped result UI.
        hits.sort_by_key(|hit| hit.duplicate);
        hits.truncate(limit);
        Ok(hits)
    }
}

fn hit_row(
    r: &rusqlite::Row<'_>,
    snippet: &str,
    page: Option<u32>,
    seconds: Option<f64>,
    score: f64,
) -> rusqlite::Result<SearchHit> {
    Ok(SearchHit {
        duplicate: false,
        uri: r.get(0)?,
        path: PathBuf::from(r.get::<_, String>(1)?),
        name: r.get(2)?,
        mime: r.get(3)?,
        snippet: snippet.chars().take(240).collect(),
        page,
        seconds,
        score,
    })
}

fn keep_hit(hits: &mut HashMap<String, SearchHit>, h: SearchHit, settings: &Settings) {
    if hits.get(&h.uri).is_some_and(|old| {
        old.score > h.score
            || (old.score == h.score && (!old.snippet.is_empty() || h.snippet.is_empty()))
    }) || !search_path_allowed(&h.path, settings)
    {
        return;
    }
    hits.insert(h.uri.clone(), h);
}
fn search_path_allowed(path: &Path, settings: &Settings) -> bool {
    // A symlink must not turn an approved folder into access outside its scope.
    settings.allows(path) && std::fs::canonicalize(path).is_ok_and(|p| settings.allows(&p))
}

pub fn validate_vector(v: &[f32]) -> Result<()> {
    if v.len() != DIMENSIONS || v.iter().any(|x| !x.is_finite()) {
        bail!("Expected a finite {DIMENSIONS}-dimensional embedding");
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if !(0.99..=1.01).contains(&norm) {
        bail!("Embedding is not L2 normalized");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn query<'a>(text: &'a str, vector: &'a [f32], pipeline: &'a str) -> SearchQuery<'a> {
        SearchQuery {
            text,
            embedding: (!vector.is_empty()).then_some(SearchEmbedding { vector, pipeline }),
        }
    }
    fn query_vector() -> Vec<f32> {
        let mut v = vec![0.0; DIMENSIONS];
        v[0] = 1.0;
        v
    }
    fn fixture() -> Result<(tempfile::TempDir, Store, Settings, FileRecord)> {
        let d = tempfile::Builder::new().prefix("gseek-test-").tempdir()?;
        let path = d.path().join("Lease.txt");
        std::fs::write(&path, "cats welcome")?;
        let uri = url::Url::from_file_path(&path).unwrap().to_string();
        let s = Settings {
            folders: vec![d.path().to_owned()],
            ..Default::default()
        };
        let f = FileRecord {
            search: Default::default(),
            source_id: 1,
            uri,
            path,
            name: "Lease.txt".into(),
            mime: "text/plain".into(),
            stamp: "1".into(),
            size: 12,
            folder: false,
        };
        Ok((d, Store::open(Path::new(":memory:"))?, s, f))
    }

    #[test]
    fn known_file_benchmark_uses_gnome_recency_and_rank_without_folder_bias() -> Result<()> {
        let (dir, mut store, settings, template) = fixture()?;
        let at = now();
        let cases = [
            (
                "profile",
                "Downloads/Profile.pdf",
                "Documents/Profile.pdf",
                0,
                180,
                0.0,
            ),
            (
                "budget",
                "Downloads/Budget.pdf",
                "Documents/Budget.pdf",
                180,
                0,
                0.0,
            ),
            (
                "minutes",
                "Documents/Minutes.pdf",
                "Downloads/Minutes.pdf",
                180,
                0,
                0.0,
            ),
            (
                "contract",
                "Downloads/Contract.pdf",
                "Documents/Contract.pdf",
                180,
                180,
                10.0,
            ),
            (
                "proposal",
                "Downloads/Proposal.pdf",
                "Documents/Proposal.pdf",
                15,
                180,
                0.0,
            ),
        ];
        let mut success1 = 0;
        let mut success3 = 0;
        let mut gnome_success1 = 0;
        let started = std::time::Instant::now();
        for (text, old, wanted, old_days, new_days, rank) in cases {
            let mut old =
                ranked_file(dir.path(), &mut store, &template, old, vec![chunk(0.90, 1)])?;
            let mut wanted = ranked_file(
                dir.path(),
                &mut store,
                &template,
                wanted,
                vec![chunk(0.82, 1)],
            )?;
            if text == "profile" {
                let mut page = chunk(0.82, 1);
                page.text = "profile evidence".into();
                store.save_content("Documents/Profile.pdf", "p", &[page])?;
            }
            old.search.modified = at - old_days * 86400;
            wanted.search.modified = at - new_days * 86400;
            wanted.search.fts_rank = rank;
            if text == "proposal" {
                wanted.search.accessed = at;
            }
            let files = [old.clone(), wanted.clone()];
            let hits = store.search(query(text, &query_vector(), "p"), &settings, &files, 20)?;
            let position = hits.iter().position(|h| h.uri == wanted.uri).unwrap();
            success1 += usize::from(position == 0);
            success3 += usize::from(position < 3);
            // NautilusSearchHit global scoring, with NULL query location.
            let baseline = |file: &FileRecord| {
                (10.0 * (crate::ranking::name_match(text, &file.name) + file.search.fts_rank))
                    .min(500.0)
                    + crate::ranking::recency(&file.search, at)
            };
            gnome_success1 += usize::from(baseline(&wanted) > baseline(&old));
            assert_eq!(hits[0].uri, wanted.uri, "{text}: {hits:?}");
        }
        eprintln!(
            "Known-file corpus: Success@1={success1}/5 Success@3={success3}/5 GNOME-global-reference@1={gnome_success1}/5 elapsed={:?}",
            started.elapsed()
        );
        assert_eq!(success1, 5);
        Ok(())
    }

    #[test]
    fn proven_identical_copies_are_demoted_but_changed_and_unhashed_files_survive() -> Result<()> {
        let (dir, mut store, settings, template) = fixture()?;
        let mut first = ranked_file(
            dir.path(),
            &mut store,
            &template,
            "Documents/Guide.pdf",
            vec![chunk(0.9, 1)],
        )?;
        first.stamp = crate::discovery::file_stamp(&std::fs::metadata(&first.path)?);
        store.discover(&first, 1)?;
        store.complete(&first.uri, "Documents/Guide.pdf")?;
        let mut copy = ranked_file(
            dir.path(),
            &mut store,
            &template,
            "Copies/Guide.pdf",
            vec![],
        )?;
        copy.stamp = crate::discovery::file_stamp(&std::fs::metadata(&copy.path)?);
        store.discover(&copy, 1)?;
        store.complete(&copy.uri, "Documents/Guide.pdf")?;
        first.search.modified = now();
        copy.search.modified = now() - 180 * 86400;
        let unknown = ranked_file(
            dir.path(),
            &mut store,
            &template,
            "Downloads/Guide.pdf",
            vec![],
        )?;
        let mut files = vec![copy.clone(), first.clone(), unknown];
        let hits = store.search(query("guide", &query_vector(), "p"), &settings, &files, 20)?;
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].uri, first.uri);
        assert_eq!(hits.iter().filter(|h| h.duplicate).count(), 1);
        assert_eq!(hits.last().unwrap().uri, copy.uri);
        std::fs::write(&copy.path, "a distinct revision")?;
        files[0].stamp = crate::discovery::file_stamp(&std::fs::metadata(&copy.path)?);
        let hits = store.search(query("guide", &[], ""), &settings, &files, 20)?;
        assert!(hits.iter().all(|h| !h.duplicate));
        Ok(())
    }

    #[test]
    fn bm25_preserves_equal_strength_across_files_and_does_not_count_chunks_as_files() -> Result<()>
    {
        let (dir, mut store, settings, template) = fixture()?;
        let make = || StoredChunk {
            text: "archaeology satellite".into(),
            vector: None,
            page: Some(1),
            seconds: None,
        };
        ranked_file(
            dir.path(),
            &mut store,
            &template,
            "A.txt",
            (0..510).map(|_| make()).collect(),
        )?;
        ranked_file(dir.path(), &mut store, &template, "B.txt", vec![make()])?;
        let hits = store.search(query("archaeology", &[], ""), &settings, &[], 20)?;
        assert_eq!(hits.len(), 2);
        assert!((hits[0].score - hits[1].score).abs() < 1e-10);
        Ok(())
    }

    fn chunk(cosine: f32, page: u32) -> StoredChunk {
        let mut vector = vec![0.0; DIMENSIONS];
        vector[0] = cosine;
        vector[1] = (1.0 - cosine * cosine).sqrt();
        StoredChunk {
            text: format!("Passage {page}"),
            vector: Some(vector),
            page: Some(page),
            seconds: None,
        }
    }

    fn ranked_file(
        dir: &Path,
        store: &mut Store,
        template: &FileRecord,
        name: &str,
        chunks: Vec<StoredChunk>,
    ) -> Result<FileRecord> {
        let mut file = template.clone();
        file.name = Path::new(name)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .into();
        file.path = dir.join(name);
        std::fs::create_dir_all(file.path.parent().unwrap())?;
        std::fs::write(&file.path, "Ranking fixture")?;
        file.uri = url::Url::from_file_path(&file.path).unwrap().into();
        file.source_id = 0;
        if !chunks.is_empty() {
            store.discover(&file, 1)?;
            store.save_content(name, "p", &chunks)?;
            store.complete(&file.uri, name)?;
        }
        Ok(file)
    }

    #[test]
    fn hybrid_ranking_prefers_intent_over_the_source_of_a_match() -> Result<()> {
        let (dir, mut store, settings, template) = fixture()?;
        let mut filenames = vec![];
        for name in ["syntax.txt", "Tax archives.zip", "tax.txt"] {
            filenames.push(ranked_file(
                dir.path(),
                &mut store,
                &template,
                name,
                vec![],
            )?);
        }
        ranked_file(
            dir.path(),
            &mut store,
            &template,
            "Fiscal.pdf",
            vec![chunk(0.93, 2)],
        )?;
        ranked_file(
            dir.path(),
            &mut store,
            &template,
            "tax/Unrelated.txt",
            vec![chunk(0.40, 1)],
        )?;
        let mut word_match = chunk(0.45, 1);
        word_match.text = "tax guidance".into();
        ranked_file(
            dir.path(),
            &mut store,
            &template,
            "Keyword.txt",
            vec![word_match],
        )?;
        let vector = query_vector();
        let hits = store.search(query("tax", &vector, "p"), &settings, &filenames, 20)?;
        assert_eq!(
            hits.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
            [
                "tax.txt",
                "Fiscal.pdf",
                "Tax archives.zip",
                "Keyword.txt",
                "syntax.txt",
                "Unrelated.txt"
            ]
        );
        // Results must not depend on arrival order from LocalSearch or HashMap iteration.
        filenames.reverse();
        for limit in [1, 3, 20] {
            let limited = store.search(query("tax", &vector, "p"), &settings, &filenames, limit)?;
            assert_eq!(
                limited.iter().map(|h| &h.uri).collect::<Vec<_>>(),
                hits.iter().take(limit).map(|h| &h.uri).collect::<Vec<_>>()
            );
        }
        Ok(())
    }

    #[test]
    fn corroborated_matches_retain_similarity_and_the_best_passage_before_limiting() -> Result<()> {
        let (dir, mut store, settings, template) = fixture()?;
        ranked_file(
            dir.path(),
            &mut store,
            &template,
            "Unknown.pdf",
            vec![chunk(0.85, 1)],
        )?;
        let both = ranked_file(
            dir.path(),
            &mut store,
            &template,
            "Tax notes.txt",
            vec![chunk(0.72, 1), chunk(0.84, 7)],
        )?;
        let vector = query_vector();
        let files = [both.clone(), both.clone()];
        for limit in [1, 20] {
            let hits = store.search(query("tax", &vector, "p"), &settings, &files, limit)?;
            assert_eq!(hits[0].uri, both.uri);
            assert_eq!(hits[0].page, Some(7));
            assert_eq!(hits[0].snippet, "Passage 7");
            assert_eq!(hits.iter().filter(|h| h.uri == both.uri).count(), 1);
        }
        // A weak semantic neighbour must not gain corroboration from an unrelated embedding.
        let wrong_model = store.search(
            query("tax", &vector, "other-pipeline"),
            &settings,
            &files,
            20,
        )?;
        assert_eq!(wrong_model.len(), 1);
        assert!(wrong_model[0].snippet.is_empty());
        Ok(())
    }

    #[test]
    fn weak_embeddings_do_not_boost_lexical_matches_or_replace_their_passages() -> Result<()> {
        let (dir, mut store, settings, template) = fixture()?;
        ranked_file(
            dir.path(),
            &mut store,
            &template,
            "Relevant.pdf",
            vec![chunk(0.95, 1)],
        )?;
        let mut weak = chunk(0.61, 2);
        weak.text = "tax text".into();
        let file = ranked_file(
            dir.path(),
            &mut store,
            &template,
            "Tax guide.txt",
            vec![weak],
        )?;
        let vector = query_vector();
        let hits = store.search(query("tax", &vector, "p"), &settings, &[], 20)?;
        assert_eq!(hits[0].name, "Relevant.pdf");
        let lexical = hits.iter().find(|hit| hit.uri == file.uri).unwrap();
        let lexical_only = store.search(query("tax", &vector, "other"), &settings, &[], 20)?;
        assert_eq!(lexical.score, lexical_only[0].score);
        assert_eq!(lexical.snippet, "tax text");
        Ok(())
    }

    #[test]
    fn filename_only_locations_and_exclusions_survive_joint_ranking() -> Result<()> {
        let (dir, mut store, mut settings, template) = fixture()?;
        settings.folders = vec![dir.path().join("Documents")];
        let mut files = vec![];
        for name in [
            "Downloads/Tax.zip",
            "Downloads/.Tax.txt",
            "Downloads/Tax.pem",
            "Private/Tax.txt",
        ] {
            files.push(ranked_file(
                dir.path(),
                &mut store,
                &template,
                name,
                vec![],
            )?);
        }
        settings.excluded_folders.push(dir.path().join("Private"));
        let vector = query_vector();
        let hits = store.search(query("tax", &vector, "p"), &settings, &files, 20)?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "Tax.zip");
        assert_eq!(store.counts()?.files, 0);
        Ok(())
    }

    #[test]
    fn unicode_titles_and_whole_words_outrank_partial_names() -> Result<()> {
        let (dir, mut store, settings, template) = fixture()?;
        let mut files = vec![];
        for name in [
            "Annual_Report.pdf",
            "Annual Report Archive.zip",
            "Annually Reporting.txt",
            "الفاتورة.pdf",
        ] {
            files.push(ranked_file(
                dir.path(),
                &mut store,
                &template,
                name,
                vec![],
            )?);
        }
        ranked_file(
            dir.path(),
            &mut store,
            &template,
            "Other.pdf",
            vec![chunk(0.99, 1)],
        )?;
        let vector = query_vector();
        let hits = store.search(
            query("Ａｎｎｕａｌ Report", &vector, "p"),
            &settings,
            &files[..3],
            20,
        )?;
        assert_eq!(hits[0].name, "Annual_Report.pdf");
        assert!(
            hits.iter()
                .position(|h| h.name == "Annual Report Archive.zip")
                < hits.iter().position(|h| h.name == "Annually Reporting.txt")
        );
        let hits = store.search(query("الفاتورة", &vector, "p"), &settings, &files[3..], 20)?;
        assert_eq!(hits[0].name, "الفاتورة.pdf");
        Ok(())
    }
    #[test]
    fn exclusions_remove_queued_files_checkpoints_vectors_and_stale_scan_writes() -> Result<()> {
        let (_dir, store, mut settings, file) = fixture()?;
        let mut pending = file.clone();
        pending.uri.push_str("-queued");
        pending.path.set_file_name("Queued.txt");
        store.discover(&file, 1)?;
        store.discover(&pending, 1)?;
        let chunk = StoredChunk {
            text: "private text".into(),
            vector: Some(query_vector()),
            page: None,
            seconds: None,
        };
        assert!(store.checkpoint_for(&file, "private-hash", "model", 0, &chunk)?);
        store.complete(&file.uri, "private-hash")?;
        assert!(store.checkpoint_for(&pending, "partial-hash", "model", 0, &chunk)?);
        settings.excluded_folders = settings.folders.clone();
        store.prune_scope(&settings)?;
        assert_eq!(store.counts()?.files, 0);
        assert_eq!(store.content_progress("private-hash")?, 0);
        assert_eq!(store.content_progress("partial-hash")?, 0);
        assert!(store.activity()?.is_empty());
        // A different connection still holding an old scan/file snapshot cannot restore it.
        store.discover(&file, 1)?;
        assert!(!store.checkpoint_for(&file, "private-hash", "model", 0, &chunk)?);
        assert_eq!(store.counts()?.pending, 0);
        assert_eq!(store.content_progress("private-hash")?, 0);
        Ok(())
    }
    #[test]
    fn excluding_a_linked_folder_removes_its_indexed_target_and_queue() -> Result<()> {
        let (dir, store, mut settings, mut indexed) = fixture()?;
        let archive = dir.path().join("Archive");
        std::fs::create_dir(&archive)?;
        let link = dir.path().join("Archive Link");
        std::os::unix::fs::symlink(&archive, &link)?;
        indexed.path = archive.join("Indexed.txt");
        indexed.uri = url::Url::from_file_path(&indexed.path).unwrap().to_string();
        std::fs::write(&indexed.path, "private archive")?;
        let mut queued = indexed.clone();
        queued.path = archive.join("Queued.txt");
        queued.uri = url::Url::from_file_path(&queued.path).unwrap().to_string();
        std::fs::write(&queued.path, "queued private archive")?;
        store.discover(&indexed, 1)?;
        store.discover(&queued, 1)?;
        store.checkpoint_for(
            &indexed,
            "archive-hash",
            "model",
            0,
            &StoredChunk {
                text: "private archive".into(),
                vector: Some(query_vector()),
                page: None,
                seconds: None,
            },
        )?;
        store.finish_content("archive-hash")?;
        store.complete(&indexed.uri, "archive-hash")?;
        settings.excluded_folders.push(link);
        store.prune_scope(&settings)?;
        assert_eq!(store.counts()?.files, 0);
        assert_eq!(store.content_progress("archive-hash")?, 0);
        assert!(store.activity()?.is_empty());
        // A scan holding the original host path must not reinsert the target.
        store.discover(&queued, 1)?;
        assert_eq!(store.counts()?.files, 0);
        Ok(())
    }
    #[test]
    fn exclusions_preserve_vectors_for_an_included_duplicate() -> Result<()> {
        let (_dir, store, mut settings, file) = fixture()?;
        let mut copy = file.clone();
        copy.uri.push_str("-copy");
        copy.path.set_file_name("Copy.txt");
        store.discover(&file, 1)?;
        store.discover(&copy, 1)?;
        let chunk = StoredChunk {
            text: "shared text".into(),
            vector: Some(query_vector()),
            page: None,
            seconds: None,
        };
        store.checkpoint_for(&file, "shared-hash", "model", 0, &chunk)?;
        store.finish_content("shared-hash")?;
        store.complete(&file.uri, "shared-hash")?;
        store.complete(&copy.uri, "shared-hash")?;
        settings.excluded_folders.push(file.path.clone());
        store.prune_scope(&settings)?;
        assert_eq!(store.counts()?.files, 1);
        assert!(store.cached("shared-hash", "model")?);
        assert_eq!(store.content_progress("shared-hash")?, 1);
        Ok(())
    }
    #[test]
    fn exact_words_join_valid_embeddings_and_survive_reopen() -> Result<()> {
        let (d, _, s, f) = fixture()?;
        let db = d.path().join("index.sqlite");
        {
            let mut store = Store::open(&db)?;
            store.discover(&f, 1)?;
            store.save_content(
                "hash",
                "model1",
                &[StoredChunk {
                    text: "Cats are welcome. رقم الفاتورة INV-2026".into(),
                    vector: None,
                    page: Some(2),
                    seconds: None,
                }],
            )?;
            store.complete(&f.uri, "hash")?;
        }
        let store = Store::open(&db)?;
        assert_eq!(
            store
                .search(query("lease", &query_vector(), ""), &s, &[], 20)?
                .len(),
            1
        );
        assert_eq!(
            store.search(query("cats welcome", &query_vector(), ""), &s, &[], 20)?[0].page,
            Some(2)
        );
        assert_eq!(
            store
                .search(query("الفاتورة", &query_vector(), ""), &s, &[], 20)?
                .len(),
            1
        );
        assert!(
            store
                .search(query("cats unknown", &query_vector(), ""), &s, &[], 20)?
                .is_empty()
        );
        Ok(())
    }
    #[test]
    fn completed_scans_prune_missing_files_but_failed_scans_do_not() -> Result<()> {
        let (_d, store, _s, f) = fixture()?;
        let first = store.start_scan()?;
        store.discover(&f, first)?;
        let second = store.start_scan()?;
        assert!(second > first);
        assert_eq!(store.counts()?.files, 1);
        store.finish_scan(second)?;
        assert_eq!(store.counts()?.files, 0);
        Ok(())
    }
    #[test]
    fn unchanged_files_keep_completed_work_and_changes_requeue() -> Result<()> {
        let (_d, mut store, _s, mut f) = fixture()?;
        store.discover(&f, 1)?;
        store.save_content("h", "p", &[])?;
        store.complete(&f.uri, "h")?;
        store.discover(&f, 2)?;
        assert_eq!(store.counts()?.pending, 0);
        f.stamp = "2".into();
        store.discover(&f, 3)?;
        assert_eq!(store.counts()?.pending, 1);
        Ok(())
    }
    #[test]
    fn semantic_results_are_deduplicated_and_exact_names_win() -> Result<()> {
        let (_d, mut store, s, f) = fixture()?;
        store.discover(&f, 1)?;
        let mut v = vec![0.0; DIMENSIONS];
        v[0] = 1.0;
        store.save_content(
            "h",
            "p",
            &[
                StoredChunk {
                    text: "pets".into(),
                    vector: Some(v.clone()),
                    page: Some(1),
                    seconds: None,
                },
                StoredChunk {
                    text: "animals".into(),
                    vector: Some(v.clone()),
                    page: Some(2),
                    seconds: None,
                },
            ],
        )?;
        store.complete(&f.uri, "h")?;
        assert_eq!(store.search(query("pets", &v, "p"), &s, &[], 20)?.len(), 1);
        assert!(store.search(query("lease.txt", &v, "p"), &s, &[], 20)?[0].score > 1.0);
        assert!(
            store
                .search(query("concept", &v, "different-model"), &s, &[], 20)?
                .is_empty()
        );
        Ok(())
    }
    #[test]
    fn changing_the_pipeline_requeues_content_without_deleting_metadata() -> Result<()> {
        let (_d, mut store, _s, f) = fixture()?;
        store.activate_pipeline("old")?;
        store.discover(&f, 1)?;
        store.save_content("h", "old", &[])?;
        store.complete(&f.uri, "h")?;
        store.activate_pipeline("old")?;
        assert_eq!(store.counts()?.pending, 0);
        store.activate_pipeline("new")?;
        assert_eq!(store.counts()?.pending, 1);
        assert!(store.file(&f.uri)?.is_some());
        Ok(())
    }
    #[test]
    fn large_semantic_result_sets_keep_all_requested_files_and_their_best_chunk() -> Result<()> {
        let (dir, mut store, settings, template) = fixture()?;
        let vector = query_vector();
        let mut weaker = vector.clone();
        weaker[0] = 0.95;
        weaker[1] = (1.0_f32 - 0.95_f32.powi(2)).sqrt();
        store.save_content(
            "shared",
            "p",
            &[
                StoredChunk {
                    text: "first chunk".into(),
                    vector: Some(weaker),
                    page: Some(1),
                    seconds: None,
                },
                StoredChunk {
                    text: "best chunk".into(),
                    vector: Some(vector.clone()),
                    page: Some(2),
                    seconds: None,
                },
            ],
        )?;
        for i in (0..1000).rev() {
            let mut file = template.clone();
            file.name = format!("Entry {i:04}.txt");
            file.path = dir.path().join(&file.name);
            std::fs::write(&file.path, "content")?;
            file.uri = url::Url::from_file_path(&file.path).unwrap().into();
            file.source_id = i + 2;
            store.discover(&file, 1)?;
            store.complete(&file.uri, "shared")?;
        }
        let hits = store.search(query("concept", &vector, "p"), &settings, &[], 900)?;
        assert_eq!(hits.len(), 900);
        assert_eq!(hits.first().unwrap().name, "Entry 0000.txt");
        assert_eq!(hits.last().unwrap().name, "Entry 0899.txt");
        assert!(
            hits.iter()
                .all(|hit| hit.page == Some(2) && hit.score > 0.999)
        );
        Ok(())
    }
    #[test]
    fn checkpoints_survive_restart_and_only_completed_content_is_reused() -> Result<()> {
        let (directory, _, _, file) = fixture()?;
        let path = directory.path().join("checkpoints.sqlite");
        let chunk = StoredChunk {
            text: "First page".into(),
            vector: Some(query_vector()),
            page: Some(1),
            seconds: None,
        };
        {
            let store = Store::open(&path)?;
            store.discover(&file, 1)?;
            store.checkpoint("pipeline:hash", "pipeline", 0, &chunk)?;
            store.checkpoint("pipeline:hash", "pipeline", 0, &chunk)?;
            assert_eq!(store.content_progress("pipeline:hash")?, 1);
            assert!(!store.cached("pipeline:hash", "pipeline")?);
        }
        let store = Store::open(&path)?;
        assert_eq!(store.content_progress("pipeline:hash")?, 1);
        assert_eq!(store.counts()?.pending, 1);
        store.checkpoint("pipeline:hash", "pipeline", 1, &chunk)?;
        store.finish_content("pipeline:hash")?;
        store.complete(&file.uri, "pipeline:hash")?;
        store.discard_incomplete("pipeline:hash")?;
        assert!(store.cached("pipeline:hash", "pipeline")?);
        store.checkpoint("pipeline:changed", "pipeline", 0, &chunk)?;
        store.discard_incomplete("pipeline:changed")?;
        assert_eq!(store.content_progress("pipeline:changed")?, 0);
        Ok(())
    }
    #[test]
    fn changed_content_preferences_requeue_skipped_files_without_repeating_completed_work()
    -> Result<()> {
        let (_directory, store, _, file) = fixture()?;
        store.discover(&file, 1)?;
        store.complete(&file.uri, "existing")?;
        let mut skipped = file.clone();
        skipped.uri.push_str("-skipped");
        store.discover(&skipped, 1)?;
        store.mark(&skipped.uri, "skipped", "Image indexing is disabled")?;
        let mut failed = file.clone();
        failed.uri.push_str("-failed");
        store.discover(&failed, 1)?;
        store.mark(&failed.uri, "failed", "Cannot read file")?;
        store.reconsider_skipped()?;
        let counts = store.counts()?;
        assert_eq!(
            (
                counts.indexed,
                counts.pending,
                counts.failed,
                counts.skipped
            ),
            (1, 1, 1, 0)
        );
        Ok(())
    }
    #[test]
    fn resource_updates_handle_rename_delete_and_preserve_scan_membership() -> Result<()> {
        let (_directory, store, _, mut file) = fixture()?;
        let generation = store.start_scan()?;
        store.discover(&file, generation)?;
        let old_uri = file.uri.clone();
        file.uri.push_str("-renamed");
        file.name = "Renamed.txt".into();
        store.apply_changes(&[file.source_id], &[file.clone()])?;
        assert!(store.file(&old_uri)?.is_none());
        store.refresh(&file)?;
        store.finish_scan(generation)?;
        assert!(store.file(&file.uri)?.is_some());
        store.apply_changes(&[file.source_id], &[])?;
        assert_eq!(store.counts()?.files, 0);
        Ok(())
    }
    #[test]
    fn migrations_preserve_existing_content_and_make_it_resumable() -> Result<()> {
        for version in [1, 2, 3] {
            let (directory, _, _, file) = fixture()?;
            let path = directory.path().join("older.sqlite");
            {
                let mut store = Store::open(&path)?;
                store.discover(&file, 1)?;
                store.save_content(
                    "hash",
                    "pipeline",
                    &[StoredChunk {
                        text: "Existing page".into(),
                        vector: Some(query_vector()),
                        page: Some(3),
                        seconds: None,
                    }],
                )?;
                store.complete(&file.uri, "hash")?;
                store.conn.execute_batch("ALTER TABLE files DROP COLUMN modified; ALTER TABLE files DROP COLUMN accessed;")?;
                if version < 3 {
                    store.conn.execute_batch(
                        "DROP INDEX file_source; ALTER TABLE files DROP COLUMN source_id;",
                    )?;
                }
                if version == 1 {
                    store.conn.execute_batch("DROP INDEX chunk_part; ALTER TABLE chunks DROP COLUMN part; ALTER TABLE contents DROP COLUMN complete;")?;
                }
                store.conn.pragma_update(None, "user_version", version)?;
            }
            let store = Store::open(&path)?;
            assert!(store.cached("hash", "pipeline")?);
            assert_eq!(store.content_progress("hash")?, 1);
            assert_eq!(store.counts()?.indexed, 1);
            let part: i64 = store
                .conn
                .query_row("SELECT part FROM chunks", [], |row| row.get(0))?;
            assert_eq!(part, 0);
        }
        Ok(())
    }

    #[test]
    fn simultaneous_version_three_opens_migrate_once() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("index.sqlite");
        let store = Store::open(&path)?;
        store.conn.execute_batch("ALTER TABLE files DROP COLUMN modified; ALTER TABLE files DROP COLUMN accessed; PRAGMA user_version=3;")?;
        drop(store);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let barrier = barrier.clone();
                let path = path.clone();
                std::thread::spawn(move || -> Result<()> {
                    barrier.wait();
                    let store = Store::open(&path)?;
                    let version: u32 = store
                        .conn
                        .query_row("PRAGMA user_version", [], |r| r.get(0))?;
                    assert_eq!(version, 4);
                    Ok(())
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap()?;
        }
        Ok(())
    }

    #[test]
    fn filesystem_reads_cannot_create_or_replace_access_recency() -> Result<()> {
        let (_dir, store, settings, file) = fixture()?;
        let mut indexed = file.clone();
        indexed.search.accessed = now() - 180 * 86400;
        indexed.stamp = crate::discovery::file_stamp(&std::fs::metadata(&file.path)?);
        store.discover(&indexed, 1)?;
        std::fs::read(&file.path)?;
        let stat = crate::discovery::record_from_uri(&file.uri, &settings)?.unwrap();
        assert_eq!(stat.search.accessed, 0);
        store.discover(&stat, 1)?;
        let accessed: i64 = store.conn.query_row(
            "SELECT accessed FROM files WHERE uri=?1",
            [&file.uri],
            |r| r.get(0),
        )?;
        assert_eq!(accessed, indexed.search.accessed);
        Ok(())
    }
    #[test]
    fn invalid_vectors_and_newer_schemas_are_rejected() -> Result<()> {
        assert!(validate_vector(&[0.0; DIMENSIONS]).is_err());
        let c = Connection::open_in_memory()?;
        c.execute_batch("PRAGMA user_version=99")?;
        let d = tempfile::Builder::new().prefix("gseek-test-").tempdir()?;
        let path = d.path().join("newer.db");
        let c = Connection::open(&path)?;
        c.execute_batch("PRAGMA user_version=99")?;
        assert!(Store::open(&path).is_err());
        Ok(())
    }
    #[test]
    fn explicit_reindex_discards_completed_vectors_but_keeps_file_metadata() -> Result<()> {
        let (_dir, mut store, settings, file) = fixture()?;
        store.discover(&file, 1)?;
        store.save_content("hash", "pipeline", &[])?;
        store.complete(&file.uri, "hash")?;
        store.retry(Some(&settings.folders[0]), false)?;
        assert!(!store.cached("hash", "pipeline")?);
        assert!(store.file(&file.uri)?.is_some());
        assert_eq!(store.counts()?.pending, 1);
        Ok(())
    }
    #[test]
    fn delta_updates_wait_for_an_active_index_writer() -> Result<()> {
        let (dir, _, _, file) = fixture()?;
        let path = dir.path().join("concurrent.sqlite");
        let writer = Store::open(&path)?;
        writer.discover(&file, 1)?;
        let updater = Store::open(&path)?;
        writer.conn.execute_batch("BEGIN IMMEDIATE")?;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let ready = barrier.clone();
        let thread = std::thread::spawn(move || {
            ready.wait();
            updater.apply_changes(&[file.source_id], &[])
        });
        barrier.wait();
        std::thread::sleep(std::time::Duration::from_millis(100));
        writer.conn.execute_batch("COMMIT")?;
        thread.join().unwrap()?;
        assert_eq!(writer.counts()?.files, 0);
        Ok(())
    }
}
