use super::{Document, Metadata};
use mockall::predicate::*;
use mockall::*;
use rusqlite::{Connection, Row};
use sea_query::{Expr, ExprTrait, Iden, Order, Query, SqliteQueryBuilder};
use sea_query_rusqlite::{RusqliteBinder, rusqlite};
use std::sync::Mutex;
use thiserror::Error;
use uuid::Uuid;

enum DocumentIden {
    Table,
    Id,
    Name,
    Checksum,
    DetectedType,
    Size,
    CreatedAt,
    Transcript,
    FileContent,
    FilePreview,
}

impl Iden for DocumentIden {
    fn unquoted(&self) -> &'static str {
        match self {
            Self::Table => "document",
            Self::Id => "id",
            Self::Name => "name",
            Self::Checksum => "checksum",
            Self::DetectedType => "detected_type",
            Self::Size => "size",
            Self::CreatedAt => "created_at",
            Self::Transcript => "transcript",
            Self::FileContent => "file_content",
            Self::FilePreview => "file_preview",
        }
    }
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("failed to generate a query: {0}")]
    QueryError(#[from] sea_query::error::Error),

    #[error("failed to execute a query: {0}")]
    SqlError(#[from] rusqlite::Error),
}

pub struct GetAllCmd<'a> {
    pub after_id: Option<&'a Uuid>,
    pub limit: u64,
}

#[automock]
pub trait Storage: Send + Sync {
    fn save(&self, document: &Document) -> Result<(), Error>;
    fn get_all<'a>(&self, cmd: &'a GetAllCmd<'a>) -> Result<Vec<Metadata>, Error>;
    fn get_preview(&self, doc_id: &Uuid) -> Result<Box<[u8]>, Error>;
    fn get_content(&self, doc_id: &Uuid) -> Result<Box<[u8]>, Error>;
}

pub struct SqliteStorage {
    conn: Mutex<Connection>,
}

impl SqliteStorage {
    pub fn new(conn: Mutex<Connection>) -> Self {
        Self { conn }
    }
}

impl Storage for SqliteStorage {
    fn save(&self, document: &Document) -> Result<(), Error> {
        let (sql, values) = Query::insert()
            .into_table(DocumentIden::Table)
            .columns([
                DocumentIden::Id,
                DocumentIden::Name,
                DocumentIden::Checksum,
                DocumentIden::DetectedType,
                DocumentIden::Size,
                DocumentIden::CreatedAt,
                DocumentIden::Transcript,
                DocumentIden::FileContent,
                DocumentIden::FilePreview,
            ])
            .values([
                document.metadata.id.as_bytes().as_ref().into(),
                document.metadata.name.to_owned().into(),
                document.metadata.checksum.to_owned().into(),
                document.metadata.detected_type.to_owned().into(),
                document.metadata.size.into(),
                document.metadata.created_at.into(),
                document.metadata.transcript.to_owned().into(),
                document.file_content.as_ref().into(),
                document.file_preview.as_ref().into(),
            ])?
            .build_rusqlite(SqliteQueryBuilder);

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare_cached(sql.as_str())?;
        let _ = stmt.execute(&*values.as_params())?;

        Ok(())
    }

    fn get_all<'a>(&self, cmd: &'a GetAllCmd<'a>) -> Result<Vec<Metadata>, Error> {
        let mut query = Query::select();

        query
            .columns([
                DocumentIden::Id,
                DocumentIden::Name,
                DocumentIden::Checksum,
                DocumentIden::DetectedType,
                DocumentIden::Transcript,
                DocumentIden::Size,
                DocumentIden::CreatedAt,
            ])
            .from(DocumentIden::Table)
            .order_by(DocumentIden::Id, Order::Asc)
            .limit(cmd.limit as u64);

        if let Some(after_id) = cmd.after_id {
            query.and_where(Expr::col(DocumentIden::Id).gt(after_id.into_bytes().as_slice()));
        }

        let (sql, values) = query.build_rusqlite(SqliteQueryBuilder);
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare_cached(sql.as_str())?;
        let mut rows = stmt.query(&*values.as_params())?;

        let mut result = Vec::with_capacity(cmd.limit as usize);

        while let Some(row) = rows.next()? {
            result.push(Metadata::from(row));
        }

        Ok(result)
    }

    fn get_preview(&self, doc_id: &Uuid) -> Result<Box<[u8]>, Error> {
        let mut query = Query::select();

        query
            .columns([DocumentIden::FilePreview])
            .from(DocumentIden::Table)
            .and_where(Expr::col(DocumentIden::Id).eq(doc_id.as_bytes().as_ref()));

        let (sql, values) = query.build_rusqlite(SqliteQueryBuilder);
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare_cached(sql.as_str())?;

        let res = stmt.query_one(&*values.as_params(), |f| {
            Ok(f.get(DocumentIden::FilePreview.unquoted())?)
        })?;

        Ok(res)
    }

    fn get_content(&self, doc_id: &Uuid) -> Result<Box<[u8]>, Error> {
        let mut query = Query::select();

        query
            .columns([DocumentIden::FileContent])
            .from(DocumentIden::Table)
            .and_where(Expr::col(DocumentIden::Id).eq(doc_id.as_bytes().as_ref()));

        let (sql, values) = query.build_rusqlite(SqliteQueryBuilder);
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare_cached(sql.as_str())?;

        let res = stmt.query_one(&*values.as_params(), |f| {
            Ok(f.get(DocumentIden::FileContent.unquoted())?)
        })?;

        Ok(res)
    }
}

impl From<&Row<'_>> for Metadata {
    fn from(row: &Row) -> Self {
        Self {
            id: row.get_unwrap(DocumentIden::Id.unquoted()),
            name: row.get_unwrap(DocumentIden::Name.unquoted()),
            checksum: row.get_unwrap(DocumentIden::Checksum.unquoted()),
            detected_type: row.get_unwrap(DocumentIden::DetectedType.unquoted()),
            size: row.get_unwrap(DocumentIden::Size.unquoted()),
            created_at: row.get_unwrap(DocumentIden::CreatedAt.unquoted()),
            transcript: row.get(DocumentIden::Transcript.unquoted()).ok(),
        }
    }
}
