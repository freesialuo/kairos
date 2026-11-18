use crate::{
    CoreError,
    http::{Method, Request},
};

pub fn check_origin(req: &Request, origin: &str) -> Result<(), CoreError> {
    if !matches!(
        req.method,
        Method::Post | Method::Put | Method::Patch | Method::Delete
    ) {
        return Ok(());
    }
    let mut origins = req
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("origin"));
    if origins.next().is_some_and(|(_, value)| value == origin) && origins.next().is_none() {
        Ok(())
    } else {
        Err(CoreError::Forbidden)
    }
}
