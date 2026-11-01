//! Explicit final Result protocol. Edge/Database authorize the exact scope on
//! every call, including staging probes; a signed locator is never browser input.
use super::{
    Arc, Bytes, Deserialize, HeaderMap, HmacSha256, IntoResponse, Json, Lookup, MAX_PAGE_SIZE,
    MAX_RESPONSE_BYTES, Mac, Query, QueryArtifactRef, Response, Serialize, Service, State,
    StatusCode, SystemTime, UNIX_EPOCH, Uuid, Value, authenticate, json,
};
use crate::calculation_bundle::query_artifact::{
    ArtifactReader,
    final_result::{FORMAT, FinalQueryReader, ResultIdentity},
};
pub const REQUEST_SCHEMA: &str = "result.query.request.v2";
pub const RESPONSE_SCHEMA: &str = "result.query.response.v2";
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Scope {
    Publication {
        #[serde(rename = "publicationId")]
        publication_id: Uuid,
    },
    Readiness {
        #[serde(rename = "releaseRunId")]
        release_run_id: Uuid,
        #[serde(rename = "selectionHash")]
        selection_hash: String,
    },
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    pub schema_version: String,
    pub scope: Scope,
    pub artifact: QueryArtifactRef,
    pub result: ResultIdentity,
    pub query: Query,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FinalCursor {
    schema_version: String,
    scope: Scope,
    artifact_id: Uuid,
    manifest_sha256: String,
    result: ResultIdentity,
    filter: Option<String>,
    offset: usize,
}
fn failure(status: StatusCode, code: &str) -> Response {
    (
        status,
        Json(json!({"schemaVersion":RESPONSE_SCHEMA,"error":{"code":code}})),
    )
        .into_response()
}
pub(super) async fn query(
    State(service): State<Arc<Service>>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "clock_unavailable");
    };
    if authenticate(&service.secret, &headers, &bytes, now.as_secs()).is_err() {
        return failure(StatusCode::UNAUTHORIZED, "invalid_service_signature");
    }
    let request: Request = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return failure(StatusCode::BAD_REQUEST, "invalid_request"),
    };
    if validate(&request).is_err() {
        return failure(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let Ok(permit) = service.queries.clone().try_acquire_owned() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "query_capacity");
    };
    let artifact=match service.cache.get(request.artifact.clone()).await {
        Ok(Lookup::Ready(v))=>v,
        Ok(Lookup::Preparing)=>return (StatusCode::ACCEPTED,[("retry-after","2")],Json(json!({"schemaVersion":RESPONSE_SCHEMA,"status":"preparing","artifactId":request.artifact.artifact_id,"scope":request.scope}))).into_response(),
        Ok(Lookup::Unavailable)=>return failure(StatusCode::SERVICE_UNAVAILABLE,"artifact_unavailable"),
        Err(_)=>return failure(StatusCode::BAD_REQUEST,"invalid_artifact_reference"),
    };
    let response = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let reader = artifact
            .reader
            .lock()
            .map_err(|_| anyhow::anyhow!("reader lock failed"))?;
        match &*reader {
            ArtifactReader::Final(reader) => execute(
                reader,
                &service.secret,
                &request,
                &service.reader_executable_sha256,
            ),
            ArtifactReader::Legacy(_) => anyhow::bail!("v2 request requires final Result artifact"),
        }
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
fn validate(request: &Request) -> anyhow::Result<()> {
    anyhow::ensure!(
        request.schema_version == REQUEST_SCHEMA && request.artifact.schema_version == FORMAT,
        "invalid final request format"
    );
    request.artifact.validate()?;
    crate::calculation_bundle::validate_version(&request.result.version, "Result version")?;
    if let Scope::Readiness { selection_hash, .. } = &request.scope {
        crate::calculation_bundle::validate_sha256(selection_hash, "selection hash")?;
    }
    if let Query::Lci {
        limit,
        cursor,
        filter,
    } = &request.query
    {
        anyhow::ensure!(
            (1..=MAX_PAGE_SIZE).contains(limit)
                && cursor.as_ref().is_none_or(|v| v.len() <= 4096)
                && filter.as_ref().is_none_or(|v| v.len() <= 256),
            "final query limits exceeded"
        );
    }
    Ok(())
}
fn execute(
    reader: &FinalQueryReader,
    secret: &[u8],
    request: &Request,
    reader_executable_sha256: &str,
) -> anyhow::Result<String> {
    let row = reader.result(request.result.uuid, &request.result.version)?;
    // Internal hashes and ancestor/source identities never enter the actor DTO.
    let result = json!({"uuid":row.uuid,"version":row.version,"quantitativeReference":row.quantitative_reference,"relationKind":row.relation_kind});
    let data = match &request.query {
        Query::Overview => {
            json!({"result":result,"lciAvailable":true,"lciaAvailable":reader.manifest.impact_count>0})
        }
        Query::Lcia => json!({"result":result,"impacts":reader.lcia(row.uuid,&row.version)?}),
        Query::Lci {
            limit,
            cursor,
            filter,
        } => {
            let offset = cursor
                .as_ref()
                .map(|token| {
                    decode_cursor(secret, token, request, filter.as_deref()).map(|v| v.offset)
                })
                .transpose()?
                .unwrap_or(0);
            let (items, next, count) =
                filtered_lci(reader, &request.result, offset, *limit, filter.as_deref())?;
            let next_cursor = next
                .map(|offset| {
                    encode_cursor(
                        secret,
                        &FinalCursor {
                            schema_version: "result.query.cursor.v2".to_owned(),
                            scope: request.scope.clone(),
                            artifact_id: request.artifact.artifact_id,
                            manifest_sha256: request.artifact.manifest_sha256.clone(),
                            result: request.result.clone(),
                            filter: filter.clone(),
                            offset,
                        },
                    )
                })
                .transpose()?;
            json!({"result":result,"items":items,"nextCursor":next_cursor,"nonzeroCount":count})
        }
    };
    let value = serde_json::to_string(
        &json!({"schemaVersion":RESPONSE_SCHEMA,"reader":{"schemaVersion":"tiangong.final-result-reader.v1","executableSha256":reader_executable_sha256},"scope":request.scope,"artifactId":request.artifact.artifact_id,"result":request.result,"data":data}),
    )?;
    anyhow::ensure!(
        value.len() <= MAX_RESPONSE_BYTES,
        "final query response exceeds limit"
    );
    Ok(value)
}
fn filtered_lci(
    reader: &FinalQueryReader,
    result: &ResultIdentity,
    start: usize,
    limit: usize,
    filter: Option<&str>,
) -> anyhow::Result<(Vec<Value>, Option<usize>, u64)> {
    let filter = filter.map(str::to_lowercase);
    let mut offset = start;
    let mut scanned = 0;
    let mut items = vec![];
    let mut bytes = 0;
    loop {
        let page = reader.lci(result.uuid, &result.version, offset, 128)?;
        let count = page.nonzero_count;
        if page.items.is_empty() {
            return Ok((items, None, count));
        }
        for item in page.items {
            offset += 1;
            scanned += 1;
            let value = serde_json::to_value(item)?;
            if filter
                .as_ref()
                .is_none_or(|needle| value["item"].to_string().to_lowercase().contains(needle))
            {
                bytes += serde_json::to_vec(&value)?.len();
                anyhow::ensure!(
                    bytes <= MAX_RESPONSE_BYTES,
                    "final LCI response exceeds limit"
                );
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
fn encode_cursor(secret: &[u8], cursor: &FinalCursor) -> anyhow::Result<String> {
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
    request: &Request,
    filter: Option<&str>,
) -> anyhow::Result<FinalCursor> {
    let (payload, signature) = token
        .split_once('.')
        .ok_or_else(|| anyhow::anyhow!("invalid final cursor"))?;
    let bytes = hex::decode(payload)?;
    let mut mac = HmacSha256::new_from_slice(secret)?;
    mac.update(&bytes);
    mac.verify_slice(&hex::decode(signature)?)?;
    let cursor: FinalCursor = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        cursor.schema_version == "result.query.cursor.v2"
            && cursor.scope == request.scope
            && cursor.artifact_id == request.artifact.artifact_id
            && cursor.manifest_sha256 == request.artifact.manifest_sha256
            && cursor.result == request.result
            && cursor.filter.as_deref() == filter,
        "final cursor binding mismatch"
    );
    Ok(cursor)
}
