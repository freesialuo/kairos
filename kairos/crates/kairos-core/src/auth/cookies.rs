use crate::http::Request;

pub fn parse_cookies(header: Option<&str>) -> Vec<(String, String)> {
    header
        .into_iter()
        .flat_map(|header| header.split(';'))
        .filter_map(|fragment| {
            let (name, value) = fragment.trim().split_once('=')?;
            let name = name.trim();
            (!name.is_empty()).then(|| (name.into(), value.trim().into()))
        })
        .collect()
}

pub(crate) fn cookie(req: &Request, name: &str) -> Option<String> {
    let mut values = req
        .headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case("cookie"))
        .flat_map(|(_, value)| parse_cookies(Some(value)))
        .filter(|(key, _)| key == name);
    let (_, value) = values.next()?;
    values.next().is_none().then_some(value)
}

pub fn set_cookie(name: &str, value: &str, max_age_s: Option<i64>) -> (String, String) {
    let mut cookie = format!("{name}={value}; HttpOnly; Secure; SameSite=Strict; Path=/");
    if let Some(max_age) = max_age_s {
        cookie.push_str(&format!("; Max-Age={max_age}"));
    }
    ("Set-Cookie".into(), cookie)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cookie_fragments_and_renders_host_cookie() {
        assert_eq!(parse_cookies(None), vec![]);
        assert_eq!(
            parse_cookies(Some(" x=1; ignored; __Host-sid=a=b; blank=; =oops")),
            [
                ("x".into(), "1".into()),
                ("__Host-sid".into(), "a=b".into()),
                ("blank".into(), "".into())
            ]
        );
        assert_eq!(
            set_cookie("__Host-sid", "", Some(0)),
            (
                "Set-Cookie".into(),
                "__Host-sid=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0".into()
            )
        );
    }
}
