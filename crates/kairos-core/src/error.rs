use std::fmt;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbError(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetError {
    Network(String),
    TooLarge,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CoreError {
    Db(DbError),
    Net(NetError),
    Invalid {
        field: &'static str,
        message: String,
    },
    Unauthorized,
    Forbidden,
    NotFound,
    TooManyRequests,
    Conflict {
        code: &'static str,
        message: String,
    },
    Internal(String),
}

impl From<DbError> for CoreError {
    fn from(error: DbError) -> Self {
        Self::Db(error)
    }
}

impl From<NetError> for CoreError {
    fn from(error: NetError) -> Self {
        Self::Net(error)
    }
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for DbError {}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(message) => f.write_str(message),
            Self::TooLarge => f.write_str("response body too large"),
        }
    }
}
impl std::error::Error for NetError {}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Db(error) => write!(f, "database: {error}"),
            Self::Net(error) => write!(f, "network: {error}"),
            Self::Invalid { field, message } => write!(f, "{field}: {message}"),
            Self::Conflict { code, message } => write!(f, "{code}: {message}"),
            Self::Internal(message) => f.write_str(message),
            Self::Unauthorized => f.write_str("unauthorized"),
            Self::Forbidden => f.write_str("forbidden"),
            Self::NotFound => f.write_str("not found"),
            Self::TooManyRequests => f.write_str("too many requests"),
        }
    }
}
impl std::error::Error for CoreError {}
