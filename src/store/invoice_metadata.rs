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
    pub checkout_id: String,
    pub description: Option<String>,
    pub invoice: Option<String>,
    pub amount_sat: Option<i64>,
    pub created_at: i64,
    pub expires_at: i64,
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
				checkout_id TEXT NOT NULL,
				description TEXT,
				invoice TEXT,
				amount_sat INTEGER,
				created_at INTEGER NOT NULL,
				expires_at INTEGER NOT NULL DEFAULT 0,
				notified_expired INTEGER NOT NULL DEFAULT 0,
				paid INTEGER NOT NULL DEFAULT 0
			);",
        )
        .map_err(|e| io::Error::other(format!("Failed to create metadata table: {}", e)))?;

        // Migration: add paid column for existing databases.
        let _ = conn.execute_batch(
            "ALTER TABLE mdk_invoice_metadata ADD COLUMN paid INTEGER NOT NULL DEFAULT 0;",
        );

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn insert(&self, metadata: &InvoiceMetadata) -> io::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
			"INSERT INTO mdk_invoice_metadata (payment_hash, external_id, webhook_url, checkout_id, description, invoice, amount_sat, created_at, expires_at)
			 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
			(
				&metadata.payment_hash,
				&metadata.external_id,
				&metadata.webhook_url,
				&metadata.checkout_id,
				&metadata.description,
				&metadata.invoice,
				metadata.amount_sat,
				metadata.created_at,
				metadata.expires_at,
			),
		)
		.map_err(|e| io::Error::other(format!("Failed to insert invoice metadata: {}", e)))?;
        Ok(())
    }

    pub fn get_by_payment_hash(&self, payment_hash: &str) -> io::Result<Option<InvoiceMetadata>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT payment_hash, external_id, webhook_url, checkout_id, description, invoice, amount_sat, created_at, expires_at
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
                    description: row.get(4)?,
                    invoice: row.get(5)?,
                    amount_sat: row.get(6)?,
                    created_at: row.get(7)?,
                    expires_at: row.get(8)?,
                })
            })
            .optional()
            .map_err(|e| io::Error::other(format!("Failed to query invoice metadata: {}", e)))?;

        Ok(result)
    }

    pub fn get_expired_pending(&self, now: i64) -> io::Result<Vec<InvoiceMetadata>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT payment_hash, external_id, webhook_url, checkout_id, description, invoice, amount_sat, created_at, expires_at
				 FROM mdk_invoice_metadata
				 WHERE expires_at > 0 AND expires_at <= ?1 AND notified_expired = 0 AND webhook_url IS NOT NULL",
            )
            .map_err(|e| io::Error::other(format!("Failed to prepare expired query: {}", e)))?;

        let rows = stmt
            .query_map([now], |row| {
                Ok(InvoiceMetadata {
                    payment_hash: row.get(0)?,
                    external_id: row.get(1)?,
                    webhook_url: row.get(2)?,
                    checkout_id: row.get(3)?,
                    description: row.get(4)?,
                    invoice: row.get(5)?,
                    amount_sat: row.get(6)?,
                    created_at: row.get(7)?,
                    expires_at: row.get(8)?,
                })
            })
            .map_err(|e| io::Error::other(format!("Failed to query expired invoices: {}", e)))?;

        rows.map(|row| {
            row.map_err(|e| io::Error::other(format!("Failed to read expired row: {e}")))
        })
        .collect()
    }

    pub fn mark_expired_notified(&self, payment_hash: &str) -> io::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE mdk_invoice_metadata SET notified_expired = 1 WHERE payment_hash = ?1",
            [payment_hash],
        )
        .map_err(|e| io::Error::other(format!("Failed to mark expired notified: {}", e)))?;
        Ok(())
    }

    pub fn mark_paid(&self, payment_hash: &str) -> io::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE mdk_invoice_metadata SET paid = 1 WHERE payment_hash = ?1",
            [payment_hash],
        )
        .map_err(|e| io::Error::other(format!("Failed to mark payment paid: {}", e)))?;
        Ok(())
    }

    /// List invoices with pagination.
    ///
    /// `from`/`to` always filter on `created_at`.
    /// `all=false` (default) restricts to paid invoices only.
    pub fn list(
        &self,
        from: i64,
        to: i64,
        limit: i64,
        offset: i64,
        all: bool,
        external_id: Option<&str>,
    ) -> io::Result<Vec<InvoiceMetadata>> {
        let conn = self.conn.lock().unwrap();

        let sql = if all {
            "SELECT payment_hash, external_id, webhook_url, checkout_id, description, invoice, amount_sat, created_at, expires_at
             FROM mdk_invoice_metadata
             WHERE created_at >= ?1 AND created_at <= ?2
               AND (?3 IS NULL OR external_id = ?3)
             ORDER BY created_at DESC, rowid DESC
             LIMIT ?4 OFFSET ?5"
        } else {
            "SELECT payment_hash, external_id, webhook_url, checkout_id, description, invoice, amount_sat, created_at, expires_at
             FROM mdk_invoice_metadata
             WHERE paid = 1
               AND created_at >= ?1 AND created_at <= ?2
               AND (?3 IS NULL OR external_id = ?3)
             ORDER BY created_at DESC, rowid DESC
             LIMIT ?4 OFFSET ?5"
        };

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| io::Error::other(format!("Failed to prepare list query: {e}")))?;

        let rows = stmt
            .query_map(
                rusqlite::params![from, to, external_id, limit, offset],
                |row| {
                    Ok(InvoiceMetadata {
                        payment_hash: row.get(0)?,
                        external_id: row.get(1)?,
                        webhook_url: row.get(2)?,
                        checkout_id: row.get(3)?,
                        description: row.get(4)?,
                        invoice: row.get(5)?,
                        amount_sat: row.get(6)?,
                        created_at: row.get(7)?,
                        expires_at: row.get(8)?,
                    })
                },
            )
            .map_err(|e| io::Error::other(format!("Failed to query invoice list: {e}")))?;

        rows.map(|row| row.map_err(|e| io::Error::other(format!("Failed to read row: {e}"))))
            .collect()
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
