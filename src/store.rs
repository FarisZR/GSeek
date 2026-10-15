use crate::config::Settings;
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::{Path, PathBuf}, time::{SystemTime, UNIX_EPOCH}};
use unicode_normalization::UnicodeNormalization;

pub const DIMENSIONS: usize = 768;
pub fn folded(s: &str) -> String { s.nfkc().flat_map(char::to_lowercase).collect() }
pub fn now() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64 }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileRecord {
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
    pub uri: String,
    pub path: PathBuf,
    pub name: String,
    pub mime: String,
    pub snippet: String,
    pub page: Option<u32>,
    pub seconds: Option<f64>,
    pub score: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Counts { pub files: u64, pub indexed: u64, pub pending: u64, pub failed: u64, pub skipped: u64 }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Activity { pub time: i64, pub path: String, pub state: String, pub detail: String }

pub struct StoredChunk {
    pub text: String,
    pub vector: Option<Vec<f32>>,
    pub page: Option<u32>,
    pub seconds: Option<f64>,
}

pub struct Store { conn: Connection }
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA cache_size=-8192; PRAGMA synchronous=NORMAL;")?;
        let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > 1 { bail!("This index was created by a newer GSeek; please upgrade the application"); }
        if version == 0 {
            conn.execute_batch("BEGIN IMMEDIATE;
            CREATE TABLE files(uri TEXT PRIMARY KEY, path TEXT NOT NULL, name TEXT NOT NULL, mime TEXT NOT NULL,
                folded TEXT NOT NULL, stamp TEXT NOT NULL, size INTEGER NOT NULL, folder INTEGER NOT NULL,
                seen INTEGER NOT NULL, state TEXT NOT NULL DEFAULT 'pending', content TEXT, error TEXT NOT NULL DEFAULT '');
            CREATE INDEX file_queue ON files(state, size);
            CREATE TABLE contents(id TEXT PRIMARY KEY, pipeline TEXT NOT NULL, created INTEGER NOT NULL);
            CREATE TABLE chunks(id INTEGER PRIMARY KEY, content TEXT NOT NULL REFERENCES contents(id) ON DELETE CASCADE,
                text TEXT NOT NULL, vector BLOB, page INTEGER, seconds REAL);
            CREATE INDEX content_chunks ON chunks(content);
            CREATE VIRTUAL TABLE words USING fts5(text, content='chunks', content_rowid='id', tokenize='unicode61 remove_diacritics 2');
            CREATE TRIGGER chunks_insert AFTER INSERT ON chunks BEGIN INSERT INTO words(rowid,text) VALUES(new.id,new.text); END;
            CREATE TRIGGER chunks_delete AFTER DELETE ON chunks BEGIN INSERT INTO words(words,rowid,text) VALUES('delete',old.id,old.text); END;
            CREATE TABLE activity(time INTEGER NOT NULL, path TEXT NOT NULL, state TEXT NOT NULL, detail TEXT NOT NULL);
            CREATE TABLE metadata(key TEXT PRIMARY KEY, value TEXT NOT NULL);
            PRAGMA user_version=1; COMMIT;")?;
        }
        Ok(Self { conn })
    }

    pub fn start_scan(&self) -> Result<i64> {
        // A monotonic scan ID avoids collisions between scans within one second.
        self.conn.execute("INSERT INTO metadata VALUES('scan','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1", [])?;
        Ok(self.conn.query_row("SELECT CAST(value AS INTEGER) FROM metadata WHERE key='scan'", [], |r| r.get(0))?)
    }

    pub fn discover(&self, file: &FileRecord, generation: i64) -> Result<()> {
        self.conn.execute("INSERT INTO files(uri,path,name,mime,folded,stamp,size,folder,seen,state)
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
            ON CONFLICT(uri) DO UPDATE SET path=excluded.path,name=excluded.name,mime=excluded.mime,folded=excluded.folded,
            size=excluded.size,folder=excluded.folder,seen=excluded.seen,
            state=CASE WHEN files.stamp!=excluded.stamp THEN excluded.state ELSE files.state END,stamp=excluded.stamp",
            params![file.uri, file.path.to_string_lossy(), file.name, file.mime, folded(&file.path.to_string_lossy()),
                file.stamp, file.size, file.folder, generation, if file.folder { "indexed" } else { "pending" }])?;
        Ok(())
    }

    pub fn finish_scan(&self, generation: i64) -> Result<()> {
        self.conn.execute("DELETE FROM files WHERE seen!=?1", [generation])?;
        // Keep content-addressed vectors briefly, so renames and moves don't require inference again.
        self.conn.execute("DELETE FROM contents WHERE created<?1 AND id NOT IN (SELECT content FROM files WHERE content IS NOT NULL)", [now()-7*86400])?;
        Ok(())
    }

    pub fn prune_scope(&self, settings: &Settings) -> Result<()> {
        let mut stmt = self.conn.prepare("SELECT uri,path FROM files")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?)))?;
        for row in rows {
            let (uri, path) = row?;
            if !settings.allows(Path::new(&path)) { self.conn.execute("DELETE FROM files WHERE uri=?1", [uri])?; }
        }
        Ok(())
    }

    pub fn next_pending(&self) -> Result<Option<FileRecord>> {
        Ok(self.conn.query_row("SELECT uri,path,name,mime,stamp,size,folder FROM files WHERE state='pending' ORDER BY size,uri LIMIT 1", [], Self::file_row).optional()?)
    }

    fn file_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileRecord> {
        Ok(FileRecord { uri:row.get(0)?, path:PathBuf::from(row.get::<_,String>(1)?), name:row.get(2)?, mime:row.get(3)?,
            stamp:row.get(4)?, size:row.get(5)?, folder:row.get(6)? })
    }

    pub fn file(&self, uri: &str) -> Result<Option<FileRecord>> {
        Ok(self.conn.query_row("SELECT uri,path,name,mime,stamp,size,folder FROM files WHERE uri=?1", [uri], Self::file_row).optional()?)
    }

    pub fn cached(&self, content: &str, pipeline: &str) -> Result<bool> {
        Ok(self.conn.query_row("SELECT 1 FROM contents WHERE id=?1 AND pipeline=?2", params![content,pipeline], |_| Ok(true)).optional()?.unwrap_or(false))
    }

    pub fn save_content(&mut self, content: &str, pipeline: &str, chunks: &[StoredChunk]) -> Result<()> {
        for chunk in chunks {
            if let Some(v) = &chunk.vector { validate_vector(v)?; }
        }
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM contents WHERE id=?1", [content])?;
        tx.execute("INSERT INTO contents VALUES(?1,?2,?3)", params![content,pipeline,now()])?;
        for chunk in chunks {
            let bytes = chunk.vector.as_ref().map(|v| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>());
            tx.execute("INSERT INTO chunks(content,text,vector,page,seconds) VALUES(?1,?2,?3,?4,?5)", params![content,chunk.text,bytes,chunk.page,chunk.seconds])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn complete(&self, uri: &str, content: &str) -> Result<()> {
        self.conn.execute("UPDATE files SET content=?2,state='indexed',error='' WHERE uri=?1", params![uri,content])?;
        self.log(uri, "indexed", "Ready to search")
    }

    pub fn mark(&self, uri: &str, state: &str, detail: &str) -> Result<()> {
        self.conn.execute("UPDATE files SET state=?2,error=?3 WHERE uri=?1", params![uri,state,detail])?;
        self.log(uri, state, detail)
    }

    pub fn retry(&self, location: Option<&Path>, failed_only: bool) -> Result<()> {
        let mut stmt = self.conn.prepare("SELECT uri,path,state FROM files WHERE folder=0")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))? {
            let (uri,path,state) = row?;
            if location.is_none_or(|p| Path::new(&path).starts_with(p)) && (!failed_only || state=="failed") {
                self.conn.execute("UPDATE files SET state='pending',error='' WHERE uri=?1", [uri])?;
            }
        }
        Ok(())
    }

    pub fn counts(&self) -> Result<Counts> {
        let mut c = Counts::default();
        let mut stmt = self.conn.prepare("SELECT state,count(*) FROM files GROUP BY state")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,u64>(1)?)))? {
            let (state,n) = row?; c.files+=n;
            match state.as_str() { "indexed"=>c.indexed=n, "pending"=>c.pending=n, "failed"=>c.failed=n, "skipped"=>c.skipped=n, _=>() }
        }
        Ok(c)
    }

    pub fn log(&self, path: &str, state: &str, detail: &str) -> Result<()> {
        self.conn.execute("INSERT INTO activity VALUES(?1,?2,?3,?4)", params![now(),path,state,detail])?;
        self.conn.execute("DELETE FROM activity WHERE rowid <= (SELECT max(rowid)-2000 FROM activity)", [])?;
        Ok(())
    }

    pub fn activity(&self) -> Result<Vec<Activity>> {
        let mut stmt = self.conn.prepare("SELECT time,path,state,detail FROM activity ORDER BY rowid DESC LIMIT 200")?;
        Ok(stmt.query_map([], |r| Ok(Activity { time:r.get(0)?,path:r.get(1)?,state:r.get(2)?,detail:r.get(3)? }))?.collect::<rusqlite::Result<_>>()?)
    }

    pub fn search(&self, query: &str, vector: Option<&[f32]>, pipeline: &str, settings: &Settings, limit: usize) -> Result<Vec<SearchHit>> {
        let terms: Vec<String> = folded(query.trim()).split_whitespace().take(32).map(str::to_string).collect();
        if terms.is_empty() { return Ok(vec![]); }
        if let Some(v) = vector { validate_vector(v)?; }
        let limit = limit.clamp(1,200);
        let mut hits = HashMap::<String, SearchHit>::new();
        // Bound candidates, but preserve AND matching for filenames/paths.
        let mut sql = "SELECT uri,path,name,mime FROM files WHERE ".to_owned();
        for i in 0..terms.len() { if i>0 { sql.push_str(" AND "); } sql.push_str(&format!("instr(folded,?{})>0",i+1)); }
        sql.push_str(" ORDER BY length(name),uri LIMIT 500");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(&terms))?;
        while let Some(r) = rows.next()? {
            let mut h = hit_row(r, "", None, None, 3.0)?;
            if folded(&h.name)==terms.join(" ") { h.score+=2.0; }
            keep_hit(&mut hits,h,settings);
        }
        let fts = terms.iter().map(|t| format!("\"{}\"", t.replace('"', "\"\""))).collect::<Vec<_>>().join(" AND ");
        let mut stmt = self.conn.prepare("SELECT f.uri,f.path,f.name,f.mime,c.text,c.page,c.seconds FROM words
            JOIN chunks c ON c.id=words.rowid JOIN files f ON f.content=c.content
            WHERE words MATCH ?1 ORDER BY bm25(words),f.uri LIMIT 500")?;
        for (rank,row) in stmt.query_map([fts], |r| hit_row(r, &r.get::<_,String>(4)?,r.get(5)?,r.get(6)?, 0.0))?.enumerate() {
            let mut h = row?; h.score=2.0+1.0/(rank as f64+1.0); keep_hit(&mut hits,h,settings);
        }
        if let Some(v) = vector {
            // Stream vectors from SQLite's bounded page cache; never materialize the vector database in memory.
            let mut stmt = self.conn.prepare("SELECT f.uri,f.path,f.name,f.mime,c.text,c.page,c.seconds,c.vector FROM chunks c
                JOIN files f ON f.content=c.content JOIN contents p ON p.id=c.content WHERE c.vector IS NOT NULL AND p.pipeline=?1")?;
            let mut rows = stmt.query([pipeline])?;
            while let Some(r) = rows.next()? {
                let bytes: Vec<u8> = r.get(7)?;
                if bytes.len()!=DIMENSIONS*4 { continue; }
                let score: f64 = bytes.chunks_exact(4).zip(v).map(|(b,q)| f32::from_le_bytes(b.try_into().unwrap()) as f64 * *q as f64).sum();
                if score < 0.30 || !score.is_finite() { continue; }
                if hits.len()>=limit*4 && !hits.contains_key(&r.get::<_,String>(0)?) {
                    if let Some((_,lowest))=hits.iter().min_by(|a,b| a.1.score.total_cmp(&b.1.score)) {
                        if score<=lowest.score { continue; }
                    }
                }
                keep_hit(&mut hits,hit_row(r,&r.get::<_,String>(4)?,r.get(5)?,r.get(6)?,score)?,settings);
                if hits.len()>limit*4 {
                    let lowest = hits.iter().min_by(|a,b| a.1.score.total_cmp(&b.1.score)).map(|(k,_)| k.clone()).unwrap();
                    hits.remove(&lowest);
                }
            }
        }
        let mut hits: Vec<_> = hits.into_values().collect();
        hits.sort_by(|a,b| b.score.total_cmp(&a.score).then(a.uri.cmp(&b.uri)));
        hits.truncate(limit);
        Ok(hits)
    }
}

fn hit_row(r: &rusqlite::Row<'_>, snippet: &str, page: Option<u32>, seconds: Option<f64>, score: f64) -> rusqlite::Result<SearchHit> {
    Ok(SearchHit { uri:r.get(0)?,path:PathBuf::from(r.get::<_,String>(1)?),name:r.get(2)?,mime:r.get(3)?,snippet:snippet.chars().take(240).collect(),page,seconds,score })
}
fn keep_hit(hits: &mut HashMap<String,SearchHit>, h: SearchHit, settings: &Settings) {
    if !settings.allows(&h.path) || !h.path.exists() { return; }
    // A symlink must not turn an approved folder into access outside the user's scope.
    if std::fs::canonicalize(&h.path).is_ok_and(|p| !settings.allows(&p)) { return; }
    match hits.get(&h.uri) { Some(old) if old.score>=h.score=>(), _=>{hits.insert(h.uri.clone(),h);} }
}

pub fn validate_vector(v: &[f32]) -> Result<()> {
    if v.len()!=DIMENSIONS || v.iter().any(|x| !x.is_finite()) { bail!("Expected a finite {DIMENSIONS}-dimensional embedding"); }
    let norm = v.iter().map(|x| x*x).sum::<f32>().sqrt();
    if !(0.99..=1.01).contains(&norm) { bail!("Embedding is not L2 normalized"); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Result<(tempfile::TempDir,Store,Settings,FileRecord)> {
        let d = tempfile::tempdir()?; let path=d.path().join("Lease.txt"); std::fs::write(&path,"cats welcome")?;
        let uri=url::Url::from_file_path(&path).unwrap().to_string();
        let s=Settings { folders:vec![d.path().to_owned()],..Default::default() };
        let f=FileRecord { uri,path,name:"Lease.txt".into(),mime:"text/plain".into(),stamp:"1".into(),size:12,folder:false };
        Ok((d,Store::open(Path::new(":memory:"))?,s,f))
    }
    #[test]
    fn keyword_search_works_without_runtime_and_survives_reopen() -> Result<()> {
        let (d,_,s,f)=fixture()?; let db=d.path().join("index.sqlite");
        { let mut store=Store::open(&db)?; store.discover(&f,1)?;
          store.save_content("hash","model1",&[StoredChunk { text:"Cats are welcome. رقم الفاتورة INV-2026".into(),vector:None,page:Some(2),seconds:None }])?; store.complete(&f.uri,"hash")?; }
        let store=Store::open(&db)?;
        assert_eq!(store.search("lease",None,"",&s,20)?.len(),1);
        assert_eq!(store.search("cats welcome",None,"",&s,20)?[0].page,Some(2));
        assert_eq!(store.search("الفاتورة",None,"",&s,20)?.len(),1);
        assert!(store.search("cats unknown",None,"",&s,20)?.is_empty());
        Ok(())
    }
    #[test]
    fn completed_scans_prune_missing_files_but_failed_scans_do_not() -> Result<()> {
        let (_d,store,_s,f)=fixture()?; let first=store.start_scan()?; store.discover(&f,first)?;
        let second=store.start_scan()?; assert!(second>first); assert_eq!(store.counts()?.files,1);
        store.finish_scan(second)?; assert_eq!(store.counts()?.files,0); Ok(())
    }
    #[test]
    fn unchanged_files_keep_completed_work_and_changes_requeue() -> Result<()> {
        let (_d,mut store,_s,mut f)=fixture()?; store.discover(&f,1)?; store.save_content("h","p",&[])?; store.complete(&f.uri,"h")?;
        store.discover(&f,2)?; assert_eq!(store.counts()?.pending,0);
        f.stamp="2".into(); store.discover(&f,3)?; assert_eq!(store.counts()?.pending,1); Ok(())
    }
    #[test]
    fn semantic_results_are_deduplicated_and_exact_names_win() -> Result<()> {
        let (_d,mut store,s,f)=fixture()?; store.discover(&f,1)?;
        let mut v=vec![0.0;DIMENSIONS]; v[0]=1.0;
        store.save_content("h","p",&[StoredChunk { text:"pets".into(),vector:Some(v.clone()),page:Some(1),seconds:None },StoredChunk { text:"animals".into(),vector:Some(v.clone()),page:Some(2),seconds:None }])?;
        store.complete(&f.uri,"h")?;
        assert_eq!(store.search("pets",Some(&v),"p",&s,20)?.len(),1);
        assert!(store.search("lease.txt",Some(&v),"p",&s,20)?[0].score>4.0);
        assert!(store.search("concept",Some(&v),"different-model",&s,20)?.is_empty()); Ok(())
    }
    #[test]
    fn invalid_vectors_and_newer_schemas_are_rejected() -> Result<()> {
        assert!(validate_vector(&[0.0;DIMENSIONS]).is_err());
        let c=Connection::open_in_memory()?; c.execute_batch("PRAGMA user_version=99")?;
        let d=tempfile::tempdir()?; let path=d.path().join("newer.db"); let c=Connection::open(&path)?; c.execute_batch("PRAGMA user_version=99")?;
        assert!(Store::open(&path).is_err()); Ok(())
    }
}
