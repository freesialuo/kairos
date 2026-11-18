use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

use kairos_core::{
    CoreError, Db, DbError,
    ports::{Row, Value},
};
use wasm_bindgen::{JsCast, JsValue, closure::ScopedClosure, prelude::wasm_bindgen};
use worker::{
    Error, SqlStorage, Storage,
    js_sys::{ArrayBuffer, Function, Object, Uint8Array},
};

const MAX_EXACT_INTEGER: i64 = 1 << 53;

#[wasm_bindgen]
extern "C" {
    type RawStorage;

    #[wasm_bindgen(structural, method, catch, js_name = transactionSync)]
    fn transaction_sync(this: &RawStorage, callback: &Function) -> Result<JsValue, JsValue>;
}

pub(crate) struct DoDb {
    sql: SqlStorage,
    raw: RawStorage,
}

impl DoDb {
    pub(crate) fn new(storage: &Storage) -> Self {
        let raw: &JsValue = storage.as_raw().as_ref();
        Self {
            sql: storage.sql(),
            raw: raw.clone().unchecked_into(),
        }
    }
}

impl Db for DoDb {
    fn exec(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, DbError> {
        let params = params
            .iter()
            .map(encode_value)
            .collect::<Result<Vec<_>, _>>()?;
        let cursor = self.sql.exec_raw(sql, Some(params)).map_err(db_error)?;
        cursor
            .map(|row| decode_row(row.map_err(db_error)?))
            .collect()
    }

    fn transaction<T>(
        &self,
        f: impl FnOnce(&Self) -> Result<T, CoreError>,
    ) -> Result<T, CoreError> {
        let mut f = Some(f);
        let mut outcome = None;
        let transaction = {
            let mut callback = || -> Result<(), JsValue> {
                let f = f
                    .take()
                    .ok_or_else(|| JsValue::from_str("callback invoked twice"))?;
                let result = catch_unwind(AssertUnwindSafe(|| f(self)));
                let succeeded = matches!(result, Ok(Ok(_)));
                outcome = Some(result);
                if succeeded {
                    Ok(())
                } else {
                    // workerd rolls back the transaction when the callback throws.
                    Err(JsValue::from_str("rollback Kairos transaction"))
                }
            };
            let callback = ScopedClosure::borrow_mut_assert_unwind_safe(&mut callback);
            self.raw.transaction_sync(callback.as_ref().unchecked_ref())
        };
        match (transaction, outcome) {
            (_, Some(Err(payload))) => resume_unwind(payload),
            (Ok(_), Some(Ok(Ok(value)))) => Ok(value),
            (Err(_), Some(Ok(Err(error)))) => Err(error),
            (Err(error), _) => Err(db_error(Error::from(error)).into()),
            _ => Err(DbError("transactionSync did not return the callback result".into()).into()),
        }
    }
}

fn encode_value(value: &Value) -> Result<JsValue, DbError> {
    Ok(match value {
        Value::Null => JsValue::NULL,
        Value::Integer(n) if (-MAX_EXACT_INTEGER..=MAX_EXACT_INTEGER).contains(n) => {
            JsValue::from_f64(*n as f64)
        }
        Value::Integer(_) => {
            return Err(DbError("SQL integer exceeds exact JavaScript range".into()));
        }
        Value::Real(n) => JsValue::from_f64(*n),
        Value::Text(s) => JsValue::from_str(s),
        Value::Blob(bytes) => Uint8Array::from(bytes.as_slice()).into(),
    })
}

fn decode_row(row: JsValue) -> Result<Row, DbError> {
    let row = row
        .dyn_into::<Object>()
        .map_err(|_| DbError("expected a SQL row object".into()))?;
    Object::entries(&row)
        .iter()
        .map(|entry| {
            let entry = worker::js_sys::Array::from(&entry);
            let column = entry
                .get(0)
                .as_string()
                .ok_or_else(|| DbError("expected a SQL column name".into()))?;
            Ok((column, decode_value(entry.get(1))?))
        })
        .collect()
}

fn decode_value(value: JsValue) -> Result<Value, DbError> {
    if value.is_null() {
        Ok(Value::Null)
    } else if let Some(text) = value.as_string() {
        Ok(Value::Text(text))
    } else if let Some(number) = value.as_f64() {
        if number.fract() == 0.0 && number.abs() <= MAX_EXACT_INTEGER as f64 {
            Ok(Value::Integer(number as i64))
        } else {
            Ok(Value::Real(number))
        }
    } else if let Some(bytes) = value.dyn_ref::<Uint8Array>() {
        Ok(Value::Blob(bytes.to_vec()))
    } else if value.is_instance_of::<ArrayBuffer>() {
        Ok(Value::Blob(Uint8Array::new(&value).to_vec()))
    } else {
        Err(DbError("unsupported SQL column value".into()))
    }
}

fn db_error(error: Error) -> DbError {
    DbError(error.to_string())
}
