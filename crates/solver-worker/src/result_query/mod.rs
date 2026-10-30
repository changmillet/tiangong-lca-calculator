//! Authenticated result-only service; no solver state, queue or database startup.
pub mod cache;
use crate::calculation_bundle::query_artifact::{MAX_PAGE_SIZE, QueryArtifactRef, QueryReader};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use cache::{Cache, Lookup};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;
pub const REQUEST_SCHEMA: &str = "result.query.request.v1";
pub const RESPONSE_SCHEMA: &str = "result.query.response.v1";
const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const SIGNATURE_MAX_AGE_SECONDS: u64 = 30;

pub struct Service {
    cache: Arc<Cache>,
    secret: Vec<u8>,
    queries: Arc<Semaphore>,
}
impl Service {
    pub fn new(
        cache: Arc<Cache>,
        secret: Vec<u8>,
        concurrency: usize,
    ) -> anyhow::Result<Arc<Self>> {
        anyhow::ensure!(
            secret.len() >= 32 && concurrency > 0,
            "query service requires a 32-byte secret and positive concurrency"
        );
        Ok(Arc::new(Self {
            cache,
            secret,
            queries: Arc::new(Semaphore::new(concurrency)),
        }))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceProcess {
    pub id: Uuid,
    pub version: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryRequest {
    pub schema_version: String,
    pub publication_id: Uuid,
    pub artifact: QueryArtifactRef,
    pub process: SourceProcess,
    pub query: Query,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Query {
    Overview,
    Lcia,
    Lci {
        limit: usize,
        cursor: Option<String>,
        filter: Option<String>,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Cursor {
    manifest_sha256: String,
    publication_id: Uuid,
    process: SourceProcess,
    filter: Option<String>,
    offset: usize,
}

pub fn router(service: Arc<Service>) -> Router {
    Router::new()
        .route(
            "/health",
            get(|| async { Json(json!({"status":"ok","service":"result-query"})) }),
        )
        .route("/internal/results/query", post(query))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(service)
}

fn failure(status: StatusCode, code: &str) -> Response {
    (
        status,
        Json(json!({"schemaVersion":RESPONSE_SCHEMA,"error":{"code":code}})),
    )
        .into_response()
}

async fn query(State(service): State<Arc<Service>>, headers: HeaderMap, bytes: Bytes) -> Response {
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "clock_unavailable");
    };
    if authenticate(&service.secret, &headers, &bytes, now.as_secs()).is_err() {
        return failure(StatusCode::UNAUTHORIZED, "invalid_service_signature");
    }
    let request: QueryRequest = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return failure(StatusCode::BAD_REQUEST, "invalid_request"),
    };
    if request.schema_version != REQUEST_SCHEMA
        || request.artifact.validate().is_err()
        || validate_query(&request).is_err()
    {
        return failure(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let Ok(permit) = service.queries.clone().try_acquire_owned() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "query_capacity");
    };
    let artifact = match service.cache.get(request.artifact.clone()).await {
        Ok(Lookup::Ready(value)) => value,
        Ok(Lookup::Preparing) => return (StatusCode::ACCEPTED, [("retry-after","2")], Json(json!({"schemaVersion":RESPONSE_SCHEMA,"status":"preparing","artifactId":request.artifact.artifact_id,"publicationId":request.publication_id}))).into_response(),
        Ok(Lookup::Unavailable) => return failure(StatusCode::SERVICE_UNAVAILABLE,"artifact_unavailable"),
        Err(_) => return failure(StatusCode::BAD_REQUEST,"invalid_artifact_reference"),
    };
    let response = tokio::task::spawn_blocking(move || {
        // Owned permits and the artifact pin remain held even if the HTTP caller disconnects.
        let _permit = permit;
        let reader = artifact
            .reader
            .lock()
            .map_err(|_| anyhow::anyhow!("reader lock failed"))?;
        execute(&reader, &service.secret, &request)
    })
    .await;
    match response {
        Ok(Ok(value)) => (
            StatusCode::OK,
            [
                ("content-type", "application/json"),
                ("cache-control", "no-store"),
            ],
            value,
        )
            .into_response(),
        _ => failure(StatusCode::UNPROCESSABLE_ENTITY, "query_not_available"),
    }
}

fn validate_query(request: &QueryRequest) -> anyhow::Result<()> {
    let version = request.process.version.as_bytes();
    anyhow::ensure!(
        version.len() == 9
            && version[2] == b'.'
            && version[5] == b'.'
            && version
                .iter()
                .enumerate()
                .all(|(i, c)| matches!(i, 2 | 5) || c.is_ascii_digit()),
        "invalid source version"
    );
    if let Query::Lci {
        limit,
        cursor,
        filter,
    } = &request.query
    {
        anyhow::ensure!(
            (1..=MAX_PAGE_SIZE).contains(limit)
                && cursor.as_ref().is_none_or(|value| value.len() <= 4096)
                && filter.as_ref().is_none_or(|value| value.len() <= 256),
            "query limits exceeded"
        );
    }
    Ok(())
}

fn authenticate(secret: &[u8], headers: &HeaderMap, body: &[u8], now: u64) -> anyhow::Result<()> {
    let timestamp = headers
        .get("x-query-timestamp")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| anyhow::anyhow!("missing timestamp"))?;
    let issued: u64 = timestamp.parse()?;
    anyhow::ensure!(
        now.abs_diff(issued) <= SIGNATURE_MAX_AGE_SECONDS,
        "expired service signature"
    );
    let signature = headers
        .get("x-query-signature")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| anyhow::anyhow!("missing signature"))?;
    let mut mac = HmacSha256::new_from_slice(secret)?;
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    mac.verify_slice(&hex::decode(signature)?)?;
    Ok(())
}

fn execute(reader: &QueryReader, secret: &[u8], request: &QueryRequest) -> anyhow::Result<String> {
    let process = reader.process(request.process.id, &request.process.version)?;
    let data = match &request.query {
        Query::Overview => {
            json!({"process":process,"lciAvailable":true,"lciaAvailable":reader.manifest.impact_count > 0})
        }
        Query::Lcia => {
            json!({"process":process,"impacts":reader.lcia(process.id,&process.version)?})
        }
        Query::Lci {
            limit,
            cursor,
            filter,
        } => {
            let offset = if let Some(token) = cursor {
                decode_cursor(secret, token, request, filter.as_deref())?.offset
            } else {
                0
            };
            let (items, next, count) =
                filtered_lci(reader, &request.process, offset, *limit, filter.as_deref())?;
            let next_cursor = next
                .map(|offset| {
                    encode_cursor(
                        secret,
                        &Cursor {
                            manifest_sha256: request.artifact.manifest_sha256.clone(),
                            publication_id: request.publication_id,
                            process: request.process.clone(),
                            filter: filter.clone(),
                            offset,
                        },
                    )
                })
                .transpose()?;
            json!({"process":process,"items":items,"nextCursor":next_cursor,"nonzeroCount":count})
        }
    };
    let result = serde_json::to_string(
        &json!({"schemaVersion":RESPONSE_SCHEMA,"publicationId":request.publication_id,"artifactId":request.artifact.artifact_id,"source":request.process,"data":data}),
    )?;
    anyhow::ensure!(
        result.len() <= MAX_RESPONSE_BYTES,
        "query response exceeds limit"
    );
    Ok(result)
}

fn filtered_lci(
    reader: &QueryReader,
    process: &SourceProcess,
    start: usize,
    limit: usize,
    filter: Option<&str>,
) -> anyhow::Result<(Vec<Value>, Option<usize>, u64)> {
    let filter = filter.map(str::to_lowercase);
    let mut offset = start;
    let mut scanned = 0;
    let mut items = Vec::new();
    let mut bytes = 0;
    loop {
        let page = reader.lci(process.id, &process.version, offset, 128)?;
        let count = page.nonzero_count;
        if page.items.is_empty() {
            return Ok((items, None, count));
        }
        for item in page.items {
            offset += 1;
            scanned += 1;
            let value = serde_json::to_value(item)?;
            let matches = filter
                .as_ref()
                .is_none_or(|needle| value["item"].to_string().to_lowercase().contains(needle));
            if matches {
                bytes += serde_json::to_vec(&value)?.len();
                anyhow::ensure!(bytes <= MAX_RESPONSE_BYTES, "LCI response exceeds limit");
                items.push(value);
            }
            if items.len() == limit || scanned == 10_000 {
                return Ok((items, ((offset as u64) < count).then_some(offset), count));
            }
        }
        if page.next_offset.is_none() {
            return Ok((items, None, count));
        }
    }
}

fn encode_cursor(secret: &[u8], cursor: &Cursor) -> anyhow::Result<String> {
    let bytes = serde_json::to_vec(cursor)?;
    let mut mac = HmacSha256::new_from_slice(secret)?;
    mac.update(&bytes);
    Ok(format!(
        "{}.{}",
        hex::encode(&bytes),
        hex::encode(mac.finalize().into_bytes())
    ))
}
fn decode_cursor(
    secret: &[u8],
    token: &str,
    request: &QueryRequest,
    filter: Option<&str>,
) -> anyhow::Result<Cursor> {
    let (payload, signature) = token
        .split_once('.')
        .ok_or_else(|| anyhow::anyhow!("invalid cursor"))?;
    let bytes = hex::decode(payload)?;
    let mut mac = HmacSha256::new_from_slice(secret)?;
    mac.update(&bytes);
    mac.verify_slice(&hex::decode(signature)?)?;
    let cursor: Cursor = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        cursor.manifest_sha256 == request.artifact.manifest_sha256
            && cursor.publication_id == request.publication_id
            && cursor.process == request.process
            && cursor.filter.as_deref() == filter,
        "cursor binding mismatch"
    );
    Ok(cursor)
}

#[cfg(test)]
mod tests;
