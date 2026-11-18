//! Single-threaded platform interfaces. Workers futures need not be Send.
#![allow(async_fn_in_trait)]

use crate::{CoreError, DbError, NetError};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Row {
    columns: BTreeMap<String, Value>,
}

impl FromIterator<(String, Value)> for Row {
    fn from_iter<I: IntoIterator<Item = (String, Value)>>(columns: I) -> Self {
        Self {
            columns: columns.into_iter().collect(),
        }
    }
}

impl Row {
    fn get(&self, col: &str) -> Result<&Value, DbError> {
        self.columns
            .get(col)
            .ok_or_else(|| DbError(format!("missing column {col}")))
    }
    pub fn opt_i64(&self, col: &str) -> Result<Option<i64>, DbError> {
        match self.get(col)? {
            Value::Integer(n) => Ok(Some(*n)),
            Value::Null => Ok(None),
            _ => Err(DbError(format!("expected integer in {col}"))),
        }
    }
    pub fn i64(&self, col: &str) -> Result<i64, DbError> {
        self.opt_i64(col)?
            .ok_or_else(|| DbError(format!("unexpected NULL in {col}")))
    }
    pub fn opt_text(&self, col: &str) -> Result<Option<String>, DbError> {
        match self.get(col)? {
            Value::Text(s) => Ok(Some(s.clone())),
            Value::Null => Ok(None),
            _ => Err(DbError(format!("expected text in {col}"))),
        }
    }
    pub fn text(&self, col: &str) -> Result<String, DbError> {
        self.opt_text(col)?
            .ok_or_else(|| DbError(format!("unexpected NULL in {col}")))
    }
    pub fn blob(&self, col: &str) -> Result<Vec<u8>, DbError> {
        match self.get(col)? {
            Value::Blob(b) => Ok(b.clone()),
            _ => Err(DbError(format!("expected blob in {col}"))),
        }
    }
    pub fn bool(&self, col: &str) -> Result<bool, DbError> {
        match self.i64(col)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(DbError(format!("expected 0 or 1 in {col}"))),
        }
    }
}

pub trait Db {
    fn exec(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, DbError>;
    fn transaction<T>(&self, f: impl FnOnce(&Self) -> Result<T, CoreError>)
    -> Result<T, CoreError>;
}
pub trait Clock {
    fn now_ms(&self) -> i64;
}
pub trait Entropy {
    fn fill(&self, buf: &mut [u8]);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub max_body_bytes: usize,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
pub trait Http {
    async fn send(&self, req: OutRequest) -> Result<OutResponse, NetError>;
}
pub trait Alarm {
    async fn get(&self) -> Result<Option<i64>, CoreError>;
    async fn set(&self, at_ms: i64) -> Result<(), CoreError>;
}
pub trait Commit {
    async fn commit_point(&self) -> Result<(), CoreError>;
}
pub trait Ports {
    type Db: Db;
    type Clock: Clock;
    type Entropy: Entropy;
    type Http: Http;
    type Alarm: Alarm;
    type Commit: Commit;
}

impl Row {
    /// Serialize explicitly projected, public columns for reports and exports.
    pub(crate) fn into_json(self) -> serde_json::Value {
        let object = self
            .columns
            .into_iter()
            .map(|(name, value)| {
                let json = match value {
                    Value::Null => serde_json::Value::Null,
                    Value::Integer(n) => n.into(),
                    Value::Real(n) => n.into(),
                    Value::Text(text) => text.into(),
                    Value::Blob(_) => serde_json::Value::Null,
                };
                (name, json)
            })
            .collect();
        serde_json::Value::Object(object)
    }
}
