use crate::Faults;
use kairos_core::{
    CoreError, Db, DbError,
    ports::{Row, Value},
};
use rusqlite::{Connection, params_from_iter, types::Value as SqlValue};

pub struct SqliteDb {
    connection: Connection,
    pub(crate) faults: Faults,
}

fn database_error(error: rusqlite::Error) -> DbError {
    DbError(error.to_string())
}

impl SqliteDb {
    pub fn new(faults: Faults) -> Self {
        Self {
            connection: Connection::open_in_memory().expect("open test database"),
            faults,
        }
    }
}

impl Db for SqliteDb {
    fn exec(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, DbError> {
        self.faults.before_sql(sql)?;
        let mut stmt = self.connection.prepare(sql).map_err(database_error)?;
        let names: Vec<String> = stmt.column_names().map(String::from).collect();
        let bindings = params.iter().map(|v| match v {
            Value::Null => SqlValue::Null,
            Value::Integer(n) => SqlValue::Integer(*n),
            Value::Real(n) => SqlValue::Real(*n),
            Value::Text(s) => SqlValue::Text(s.clone()),
            Value::Blob(bytes) => SqlValue::Blob(bytes.clone()),
        });
        let mut cursor = stmt
            .query(params_from_iter(bindings))
            .map_err(database_error)?;
        let mut rows = Vec::new();
        while let Some(row) = cursor.next().map_err(database_error)? {
            let columns = names
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    let value = match row.get::<_, SqlValue>(i).map_err(database_error)? {
                        SqlValue::Null => Value::Null,
                        SqlValue::Integer(n) => Value::Integer(n),
                        SqlValue::Real(n) => Value::Real(n),
                        SqlValue::Text(s) => Value::Text(s),
                        SqlValue::Blob(b) => Value::Blob(b),
                    };
                    Ok((name.clone(), value))
                })
                .collect::<Result<Row, DbError>>()?;
            rows.push(columns);
        }
        self.faults.after_sql(sql);
        Ok(rows)
    }

    fn transaction<T>(
        &self,
        f: impl FnOnce(&Self) -> Result<T, CoreError>,
    ) -> Result<T, CoreError> {
        if !self.connection.is_autocommit() {
            return Err(CoreError::Internal("nested transaction".into()));
        }
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let result = f(self)?;
        tx.commit().map_err(database_error)?;
        Ok(result)
    }
}
