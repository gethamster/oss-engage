use engage_core::BoardError;
use serde_json::Value;
use sqlx::{Any, AnyConnection, AnyPool, Row, Transaction, any::AnyPoolOptions};

pub(crate) fn database_error(error: sqlx::Error) -> BoardError {
    tracing_error(&error);
    BoardError::new(500, "storage_error", "database operation failed")
}

fn tracing_error(error: &sqlx::Error) {
    tracing::error!(kind = ?std::mem::discriminant(error), "board database operation failed");
}

pub(crate) async fn connect(url: &str) -> Result<AnyPool, BoardError> {
    sqlx::any::install_default_drivers();
    let url = if url == ":memory:" || url == "memory" {
        "sqlite::memory:"
    } else {
        url
    };
    let sqlite = url.starts_with("sqlite:");
    let pool = AnyPoolOptions::new()
        .max_connections(if url.contains(":memory:") { 1 } else { 8 })
        .after_connect(move |conn: &mut AnyConnection, _| {
            Box::pin(async move {
                if sqlite {
                    sqlx::query("PRAGMA busy_timeout=30000")
                        .execute(&mut *conn)
                        .await?;
                    sqlx::query("PRAGMA foreign_keys=ON")
                        .execute(&mut *conn)
                        .await?;
                }
                Ok(())
            })
        })
        .connect(url)
        .await
        .map_err(database_error)?;
    let mut connection = pool.acquire().await.map_err(database_error)?;
    let postgres = connection.backend_name() == "PostgreSQL";
    // Cold-starting replicas must not race PostgreSQL's catalog creation.
    const MIGRATION_LOCK: i64 = 0x454e47414745;
    if postgres {
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(MIGRATION_LOCK)
            .execute(&mut *connection)
            .await
            .map_err(database_error)?;
    }
    if sqlite && !url.contains(":memory:") {
        sqlx::query("PRAGMA journal_mode=WAL")
            .execute(&mut *connection)
            .await
            .map_err(database_error)?;
    }
    for statement in [
        "CREATE TABLE IF NOT EXISTS engage_clock (id INTEGER PRIMARY KEY, position BIGINT NOT NULL)",
        "INSERT INTO engage_clock(id,position) VALUES(1,0) ON CONFLICT(id) DO NOTHING",
        "CREATE TABLE IF NOT EXISTS engage_records (kind TEXT NOT NULL, id TEXT NOT NULL, channel TEXT NOT NULL, parent TEXT NOT NULL, position BIGINT NOT NULL, body TEXT NOT NULL, PRIMARY KEY(kind,id))",
        "CREATE INDEX IF NOT EXISTS engage_records_channel ON engage_records(kind,channel,position)",
        "CREATE INDEX IF NOT EXISTS engage_records_parent ON engage_records(kind,parent,position)",
        "CREATE TABLE IF NOT EXISTS engage_events (position BIGINT PRIMARY KEY, body TEXT NOT NULL, before_body TEXT)",
    ] {
        sqlx::query(statement)
            .execute(&mut *connection)
            .await
            .map_err(database_error)?;
    }
    if postgres {
        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(MIGRATION_LOCK)
            .execute(&mut *connection)
            .await
            .map_err(database_error)?;
    }
    Ok(pool)
}

pub(crate) struct Tx<'a> {
    pub tx: Transaction<'a, Any>,
    pub position: i64,
    pub at: chrono::DateTime<chrono::Utc>,
    pub middleware: Vec<std::sync::Arc<dyn crate::middleware::Middleware>>,
    pub source: Option<engage_core::WriteSource>,
}

impl<'a> Tx<'a> {
    pub async fn begin(pool: &'a AnyPool) -> Result<Self, BoardError> {
        let mut tx = pool.begin().await.map_err(database_error)?;
        // This row lock is taken before reading state, including on SQLite. Positions are
        // allocated under this same lock and published only with the enclosing commit.
        sqlx::query("UPDATE engage_clock SET position=position WHERE id=1")
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        let position: i64 = sqlx::query_scalar("SELECT position FROM engage_clock WHERE id=1")
            .fetch_one(&mut *tx)
            .await
            .map_err(database_error)?;
        let query = if tx.backend_name() == "PostgreSQL" {
            "SELECT to_char(clock_timestamp() AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"')"
        } else {
            "SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')"
        };
        let instant: String = sqlx::query_scalar(query)
            .fetch_one(&mut *tx)
            .await
            .map_err(database_error)?;
        let at = chrono::DateTime::parse_from_rfc3339(&instant)
            .map_err(|_| BoardError::new(500, "storage_error", "database clock is invalid"))?
            .with_timezone(&chrono::Utc);
        Ok(Self {
            tx,
            position,
            at,
            middleware: vec![],
            source: None,
        })
    }
    pub fn conn(&mut self) -> &mut AnyConnection {
        &mut self.tx
    }
    pub async fn get(&mut self, kind: &str, id: &str) -> Result<Option<Value>, BoardError> {
        let value: Option<String> =
            sqlx::query_scalar("SELECT body FROM engage_records WHERE kind=$1 AND id=$2")
                .bind(kind)
                .bind(id)
                .fetch_optional(self.conn())
                .await
                .map_err(database_error)?;
        value
            .map(|s| {
                serde_json::from_str(&s)
                    .map_err(|_| BoardError::new(500, "storage_error", "invalid stored record"))
            })
            .transpose()
    }
    pub async fn list(&mut self, kind: &str) -> Result<Vec<Value>, BoardError> {
        self.select(kind, None, None).await
    }
    pub async fn select(
        &mut self,
        kind: &str,
        channel: Option<&str>,
        parent: Option<&str>,
    ) -> Result<Vec<Value>, BoardError> {
        let rows = if let Some(channel) = channel {
            sqlx::query(
                "SELECT body FROM engage_records WHERE kind=$1 AND channel=$2 ORDER BY position,id",
            )
            .bind(kind)
            .bind(channel)
            .fetch_all(self.conn())
            .await
        } else if let Some(parent) = parent {
            sqlx::query(
                "SELECT body FROM engage_records WHERE kind=$1 AND parent=$2 ORDER BY position,id",
            )
            .bind(kind)
            .bind(parent)
            .fetch_all(self.conn())
            .await
        } else {
            sqlx::query("SELECT body FROM engage_records WHERE kind=$1 ORDER BY position,id")
                .bind(kind)
                .fetch_all(self.conn())
                .await
        }
        .map_err(database_error)?;
        rows.into_iter()
            .map(|row| {
                let s: String = row.get("body");
                serde_json::from_str(&s)
                    .map_err(|_| BoardError::new(500, "storage_error", "invalid stored record"))
            })
            .collect()
    }
    pub async fn put(&mut self, kind: &str, id: &str, value: &Value) -> Result<(), BoardError> {
        sqlx::query("INSERT INTO engage_records(kind,id,channel,parent,position,body) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(kind,id) DO UPDATE SET channel=excluded.channel,parent=excluded.parent,position=excluded.position,body=excluded.body")
            .bind(kind).bind(id).bind(value["channel"].as_str().or_else(||value["path"].as_str()).unwrap_or(""))
            .bind(value["thread"].as_str().unwrap_or(""))
            .bind(value["position"].as_i64().or_else(|| if matches!(kind,"message"|"task"){id.parse::<i64>().ok()}else{value["version"].as_i64()}).unwrap_or(self.position))
            .bind(value.to_string()).execute(self.conn()).await.map_err(database_error)?;
        Ok(())
    }
    pub async fn remove(&mut self, kind: &str, id: &str) -> Result<(), BoardError> {
        sqlx::query("DELETE FROM engage_records WHERE kind=$1 AND id=$2")
            .bind(kind)
            .bind(id)
            .execute(self.conn())
            .await
            .map_err(database_error)?;
        Ok(())
    }
    pub async fn event(
        &mut self,
        event: &mut Value,
        before: Option<&Value>,
    ) -> Result<(), BoardError> {
        self.position += 1;
        event["cursor"] = self.position.to_string().into();
        sqlx::query("INSERT INTO engage_events(position,body,before_body) VALUES($1,$2,$3)")
            .bind(self.position)
            .bind(event.to_string())
            .bind(before.map(Value::to_string))
            .execute(self.conn())
            .await
            .map_err(database_error)?;
        sqlx::query("UPDATE engage_clock SET position=$1 WHERE id=1")
            .bind(self.position)
            .execute(self.conn())
            .await
            .map_err(database_error)?;
        Ok(())
    }
    pub async fn events(&mut self, after: i64) -> Result<Vec<(Value, Option<Value>)>, BoardError> {
        let rows = sqlx::query(
            "SELECT body,before_body FROM engage_events WHERE position>$1 ORDER BY position",
        )
        .bind(after)
        .fetch_all(self.conn())
        .await
        .map_err(database_error)?;
        rows.into_iter()
            .map(|row| {
                let s: String = row.get("body");
                let b: Option<String> = row.get("before_body");
                Ok((
                    serde_json::from_str(&s).map_err(|_| {
                        BoardError::new(500, "storage_error", "invalid stored event")
                    })?,
                    b.map(|s| serde_json::from_str(&s))
                        .transpose()
                        .map_err(|_| {
                            BoardError::new(500, "storage_error", "invalid stored event")
                        })?,
                ))
            })
            .collect()
    }
    pub async fn commit(self) -> Result<(), BoardError> {
        self.tx.commit().await.map_err(database_error)
    }
}
