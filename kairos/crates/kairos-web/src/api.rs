use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use std::fmt;

#[derive(Debug, PartialEq)]
pub enum ApiError {
    Unauthorized,
    Confirm(String),
    Other(String),
}
impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized => f.write_str("Please sign in."),
            Self::Confirm(s) | Self::Other(s) => f.write_str(s),
        }
    }
}
pub trait JsonFields {
    fn field(&self, name: &str) -> &Value;
}
impl JsonFields for Value {
    fn field(&self, name: &str) -> &Value {
        self.get(name).unwrap_or(&Value::Null)
    }
}

fn failure(status: u16, data: &Value) -> ApiError {
    match status {
        401 => ApiError::Unauthorized,
        409 if *data.field("error") == "confirm_required" => ApiError::Confirm(
            data.field("warning")
                .as_str()
                .unwrap_or("Please confirm.")
                .into(),
        ),
        _ => ApiError::Other(if let Some(field) = data.field("field").as_str() {
            format!("Check the {field} field.")
        } else {
            data.field("error")
                .as_str()
                .map(|s| s.replace('_', " "))
                .unwrap_or_else(|| format!("Request failed ({status})."))
        }),
    }
}
#[cfg(target_arch = "wasm32")]
pub async fn call(method: &str, path: &str, body: Option<Value>) -> Result<Value, ApiError> {
    use gloo_net::http::RequestBuilder;
    let err = |e: gloo_net::Error| ApiError::Other(e.to_string());
    let builder = RequestBuilder::new(path).method(
        method
            .parse()
            .map_err(|_| ApiError::Other("Invalid request method".into()))?,
    );
    let req = if let Some(body) = body {
        builder.json(&body).map_err(err)?
    } else {
        builder.build().map_err(err)?
    };
    let response = req.send().await.map_err(err)?;
    let status = response.status();
    let data = if status == 204 {
        Value::Null
    } else {
        response.json().await.map_err(err)?
    };
    if response.ok() {
        Ok(data)
    } else {
        if status == 401
            && path.starts_with("/api/admin/")
            && let Some(window) = web_sys::window()
        {
            let _ = window.location().set_href("/login");
        }
        Err(failure(status, &data))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_auth_and_confirmation() {
        assert_eq!(failure(401, &Value::Null), ApiError::Unauthorized);
        assert_eq!(
            failure(
                409,
                &json!({"error":"confirm_required","warning":"Last key"})
            ),
            ApiError::Confirm("Last key".into())
        );
    }
}
