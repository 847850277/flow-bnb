use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::{SecondsFormat, Utc};
use hmac::{Hmac, Mac};
use postman_http::{
    request::{HttpMethod, RedirectPolicy, Request, RequestBody, RequestOptions},
    response::HttpResponse,
    HttpError, HttpTransport,
};
use postman_request::RequestClient;
use sha2::Sha256;

const API_HOST: &str = "web3.binance.com";
const API_PATH_PREFIX: &str = "/build/api/";

/// HTTP transport that signs Binance Web3 API requests immediately before they are sent.
///
/// Credentials live outside flow inputs, so a serialized flow, debug event, or output cannot
/// accidentally persist the secret key. The transport refuses to attach credentials to any host
/// other than the official HTTPS endpoint.
#[derive(Clone)]
pub struct BinanceWeb3Transport {
    inner: RequestClient,
    api_key: String,
    secret_key: String,
}

impl BinanceWeb3Transport {
    pub fn new(
        api_key: impl Into<String>,
        secret_key: impl Into<String>,
    ) -> Result<Self, HttpError> {
        Ok(Self {
            inner: RequestClient::try_new(concat!("flow-bnb/", env!("CARGO_PKG_VERSION")))?,
            api_key: api_key.into(),
            secret_key: secret_key.into(),
        })
    }
}

impl HttpTransport for BinanceWeb3Transport {
    async fn execute(
        &self,
        mut request: Request,
        mut options: RequestOptions,
    ) -> Result<HttpResponse, HttpError> {
        let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        sign_request_at(&mut request, &self.api_key, &self.secret_key, &timestamp)?;
        // Flow logs the request before signing. Describe injected authentication headers
        // separately without ever passing the key or generated signature to the logger.
        log_auth_headers(&timestamp);
        // Authentication headers must never follow a redirect to another origin.
        options.redirect_policy = RedirectPolicy::DoNotFollow;
        self.inner.execute(request, options).await
    }
}

fn log_auth_headers(timestamp: &str) {
    for (header, value) in [
        ("X-OC-APIKEY", "[REDACTED]"),
        ("X-OC-SIGN", "[REDACTED]"),
        ("X-OC-TIMESTAMP", timestamp),
        ("X-OC-RECV-WINDOW", "5000"),
    ] {
        tracing::debug!(header, value, "signed request header");
    }
}

/// Adds the authentication headers required by the Binance Web3 API.
///
/// `timestamp` is accepted as an argument to keep signing deterministic and independently
/// testable. Production callers should use [`BinanceWeb3Transport`].
pub fn sign_request_at(
    request: &mut Request,
    api_key: &str,
    secret_key: &str,
    timestamp: &str,
) -> Result<String, HttpError> {
    if api_key.is_empty() || secret_key.is_empty() {
        return Err(HttpError::invalid_request(
            "Binance Web3 API credentials cannot be empty",
        ));
    }

    let url = url::Url::parse(&request.url)
        .map_err(|error| HttpError::invalid_request(format!("invalid Binance API URL: {error}")))?;
    if url.scheme() != "https" || url.host_str() != Some(API_HOST) || url.port().is_some() {
        return Err(HttpError::invalid_request(
            "refusing to send Binance credentials outside https://web3.binance.com",
        ));
    }
    if !url.path().starts_with(API_PATH_PREFIX) {
        return Err(HttpError::invalid_request(
            "Binance Web3 API path must start with /build/api/",
        ));
    }

    let request_path = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    };
    let body = signed_body(request)?;
    let pre_hash = format!("{timestamp}{}{request_path}{body}", request.method);

    let mut mac = Hmac::<Sha256>::new_from_slice(secret_key.as_bytes())
        .map_err(|_| HttpError::invalid_request("invalid Binance Web3 secret key"))?;
    mac.update(pre_hash.as_bytes());
    let signature = STANDARD.encode(mac.finalize().into_bytes());

    upsert_header(&mut request.headers, "X-OC-APIKEY", api_key);
    upsert_header(&mut request.headers, "X-OC-TIMESTAMP", timestamp);
    upsert_header(&mut request.headers, "X-OC-SIGN", &signature);
    upsert_header(&mut request.headers, "X-OC-RECV-WINDOW", "5000");

    Ok(signature)
}

fn signed_body(request: &Request) -> Result<&str, HttpError> {
    if matches!(request.method, HttpMethod::GET | HttpMethod::HEAD) {
        return Ok("");
    }

    match &request.body {
        RequestBody::None => Ok(""),
        RequestBody::Json(body) | RequestBody::Raw(body) | RequestBody::UrlEncoded(body) => {
            Ok(body)
        }
        RequestBody::File(_) | RequestBody::Multipart(_) => Err(HttpError::invalid_request(
            "Binance Web3 signing does not support file or multipart bodies",
        )),
    }
}

fn upsert_header(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
    headers.push((name.to_owned(), value.to_owned()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_the_exact_path_query_and_adds_required_headers() {
        let mut request = Request::new(
            HttpMethod::GET,
            "https://web3.binance.com/build/api/v1/dex/market/rwa/search?keyword=NVDA&platformId=ondo",
        );

        let signature = sign_request_at(
            &mut request,
            "test-api-key",
            "test-secret",
            "2026-05-11T10:08:57.715Z",
        )
        .unwrap();

        assert_eq!(signature, "m6raIuLAe6rRyzNtrRKbqHOnNwCkBqlUPGIprmrMyAk=");
        assert_eq!(header(&request, "X-OC-APIKEY"), Some("test-api-key"));
        assert_eq!(
            header(&request, "X-OC-TIMESTAMP"),
            Some("2026-05-11T10:08:57.715Z")
        );
        assert_eq!(header(&request, "X-OC-SIGN"), Some(signature.as_str()));
        assert!(!format!("{request:?}").contains("test-secret"));
    }

    #[test]
    fn refuses_to_send_credentials_to_another_host() {
        let mut request = Request::new(
            HttpMethod::GET,
            "https://example.com/build/api/v1/dex/market/rwa/search?keyword=NVDA",
        );

        let error = sign_request_at(
            &mut request,
            "test-api-key",
            "test-secret",
            "2026-05-11T10:08:57.715Z",
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("refusing to send Binance credentials"));
        assert!(request.headers.is_empty());
    }

    fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
        request
            .headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}
