use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

pub struct InvoiceMetadataStore {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone)]
pub struct InvoiceMetadata {
    pub payment_hash: String,
    pub external_id: Option<String>,
    pub webhook_url: Option<String>,
    pub checkout_id: Option<String>,
    pub created_at: i64,
}

impl InvoiceMetadataStore {
    pub fn new(db_path: &Path) -> io::Result<Self> {
        let conn = Connection::open(db_path).map_err(|e| {
            io::Error::other(format!(
                "Failed to open metadata DB {}: {}",
                db_path.display(),
                e
            ))
        })?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS mdk_invoice_metadata (
				payment_hash TEXT PRIMARY KEY,
				external_id TEXT,
				webhook_url TEXT,
				checkout_id TEXT,
				created_at INTEGER NOT NULL
			);",
        )
        .map_err(|e| io::Error::other(format!("Failed to create metadata table: {}", e)))?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn insert(&self, metadata: &InvoiceMetadata) -> io::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
			"INSERT INTO mdk_invoice_metadata (payment_hash, external_id, webhook_url, checkout_id, created_at)
			 VALUES (?1, ?2, ?3, ?4, ?5)",
			(
				&metadata.payment_hash,
				&metadata.external_id,
				&metadata.webhook_url,
				&metadata.checkout_id,
				metadata.created_at,
			),
		)
		.map_err(|e| io::Error::other(format!("Failed to insert invoice metadata: {}", e)))?;
        Ok(())
    }

    pub fn get_by_payment_hash(&self, payment_hash: &str) -> io::Result<Option<InvoiceMetadata>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT payment_hash, external_id, webhook_url, checkout_id, created_at
				 FROM mdk_invoice_metadata WHERE payment_hash = ?1",
            )
            .map_err(|e| io::Error::other(format!("Failed to prepare query: {}", e)))?;

        let result = stmt
            .query_row([payment_hash], |row| {
                Ok(InvoiceMetadata {
                    payment_hash: row.get(0)?,
                    external_id: row.get(1)?,
                    webhook_url: row.get(2)?,
                    checkout_id: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })
            .optional()
            .map_err(|e| io::Error::other(format!("Failed to query invoice metadata: {}", e)))?;

        Ok(result)
    }

    pub fn now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time must be > 1970")
            .as_secs() as i64
    }
}

trait OptionalExt<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalExt<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
