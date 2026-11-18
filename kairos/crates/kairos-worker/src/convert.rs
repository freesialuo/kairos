use kairos_core::http;
use worker::{Headers, Request, Response, ResponseBuilder, Result};

pub(crate) async fn request(mut req: Request) -> Result<http::Request> {
    let url = req.url()?;
    let body = if req.inner().body().is_some() {
        req.bytes().await?
    } else {
        Vec::new()
    };
    Ok(http::Request {
        method: req.method().as_ref().into(),
        path: req.path(),
        query: url.query_pairs().into_owned().collect(),
        headers: req.headers().entries().collect(),
        body,
    })
}

pub(crate) fn response(res: http::Response) -> Result<Response> {
    let headers = Headers::new();
    for (name, value) in res.headers {
        headers.append(&name, &value)?;
    }
    let builder = ResponseBuilder::new()
        .with_status(res.status)
        .with_headers(headers);
    Ok(if res.body.is_empty() {
        builder.empty()
    } else {
        builder.fixed(res.body)
    })
}
