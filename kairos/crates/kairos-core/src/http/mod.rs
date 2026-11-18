pub(crate) mod auth_routes;
#[cfg(feature = "test-hooks")]
mod internal;
pub mod router;

use crate::CoreError;
use serde::Serialize;
use serde_json::json;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Other,
}
impl From<&str> for Method {
    fn from(name: &str) -> Self {
        match name {
            "GET" => Self::Get,
            "POST" => Self::Post,
            "PUT" => Self::Put,
            "PATCH" => Self::Patch,
            "DELETE" => Self::Delete,
            _ => Self::Other,
        }
    }
}
pub struct Request {
    pub method: Method,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
impl Response {
    pub fn json<T: Serialize>(status: u16, body: &T) -> Response {
        let (status, body) = match serde_json::to_vec(body) {
            Ok(bytes) => (status, bytes),
            Err(_) => (500, br#"{"error":"internal"}"#.to_vec()),
        };
        Response {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body,
        }
    }
    pub fn empty(status: u16) -> Response {
        Response {
            status,
            headers: vec![],
            body: vec![],
        }
    }
    pub fn from_error(e: &CoreError) -> Response {
        let (status, body) = match e {
            CoreError::Invalid { field, .. } => (400, json!({"error":"invalid", "field":field})),
            CoreError::Unauthorized => (401, json!({"error":"unauthorized"})),
            CoreError::Forbidden => (403, json!({"error":"forbidden"})),
            CoreError::NotFound => (404, json!({"error":"not_found"})),
            CoreError::Conflict { code, message } => {
                (409, json!({"error":code, "warning":message}))
            }
            CoreError::TooManyRequests => (429, json!({"error":"too_many_requests"})),
            CoreError::Internal(_) | CoreError::Db(_) | CoreError::Net(_) => {
                (500, json!({"error":"internal"}))
            }
        };
        Self::json(status, &body)
    }
}
