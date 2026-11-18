use futures_util::TryStreamExt;
use kairos_core::{
    NetError,
    ports::{Http, OutRequest, OutResponse},
};
use worker::{AbortController, Error, Fetch, Headers, ResponseBody, js_sys::Uint8Array, web_sys};

pub(crate) struct FetchHttp;

impl Http for FetchHttp {
    async fn send(&self, req: OutRequest) -> Result<OutResponse, NetError> {
        let headers = Headers::new();
        for (name, value) in req.headers {
            headers.append(&name, &value).map_err(network_error)?;
        }
        let init = web_sys::RequestInit::new();
        init.set_method(&req.method);
        init.set_headers(headers.as_ref());
        // Keep headers and request bodies scoped to the URL the caller validated.
        init.set_redirect(web_sys::RequestRedirect::Manual);
        if let Some(body) = req.body {
            init.set_body(&Uint8Array::from(body.as_slice()));
        }
        let request = web_sys::Request::new_with_str_and_init(&req.url, &init)
            .map_err(|error| network_error(error.into()))?;
        let controller = AbortController::default();
        let mut response = Fetch::Request(request.into())
            .send_with_signal(&controller.signal())
            .await
            .map_err(network_error)?;
        let status = response.status_code();
        let headers = response.headers().entries().collect();
        let body = match response.body() {
            ResponseBody::Empty => Vec::new(),
            ResponseBody::Body(bytes) if bytes.len() <= req.max_body_bytes => bytes.clone(),
            ResponseBody::Body(_) => {
                controller.abort();
                return Err(NetError::TooLarge);
            }
            ResponseBody::Stream(_) => {
                let mut stream = response.stream().map_err(network_error)?;
                let mut body = Vec::new();
                while let Some(chunk) = stream.try_next().await.map_err(network_error)? {
                    if chunk.len() > req.max_body_bytes - body.len() {
                        controller.abort();
                        return Err(NetError::TooLarge);
                    }
                    body.extend_from_slice(&chunk);
                }
                body
            }
        };
        Ok(OutResponse {
            status,
            headers,
            body,
        })
    }
}

fn network_error(error: Error) -> NetError {
    NetError::Network(error.to_string())
}
