use super::*;
use crate::{
    calculation_bundle::query_artifact::{
        self, BuildLimits, BuiltQueryArtifact,
        tests::{fixture, receipt},
    },
    storage::ObjectStoreClient,
};
use axum::{extract::State, http::Uri};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

const SECRET: &[u8] = b"synthetic-query-secret-at-least-32-bytes";
struct Server {
    url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn serve(app: Router) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Server { url, task }
}
fn artifact() -> BuiltQueryArtifact {
    let bundle = fixture(2.0);
    query_artifact::build(
        bundle.manifest_path.parent().unwrap(),
        &receipt(&bundle),
        Uuid::new_v4(),
        &BuildLimits::default(),
    )
    .unwrap()
}
async fn object_store(
    artifacts: &[&BuiltQueryArtifact],
) -> (Server, ObjectStoreClient, Arc<AtomicUsize>) {
    let mut files = BTreeMap::new();
    for artifact in artifacts {
        for name in artifact
            .manifest
            .files
            .iter()
            .map(|file| file.name.as_str())
            .chain([query_artifact::MANIFEST_NAME])
        {
            files.insert(
                format!(
                    "/bucket/prefix/{}/{name}",
                    artifact.reference.relative_prefix()
                ),
                std::fs::read(artifact.path().join(name)).unwrap(),
            );
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let state = Arc::new((files, calls.clone()));
    let server = serve(Router::new().fallback(read).with_state(state)).await;
    let store = ObjectStoreClient::new(
        &server.url,
        "test-region",
        "bucket",
        "prefix",
        "test-key",
        "test-secret",
        None,
    )
    .unwrap();
    (server, store, calls)
}
async fn ready(cache: &Arc<Cache>, reference: &QueryArtifactRef) -> Arc<cache::CachedArtifact> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match cache.get(reference.clone()).await.unwrap() {
                Lookup::Ready(value) => break value,
                Lookup::Preparing => tokio::time::sleep(Duration::from_millis(10)).await,
                Lookup::Unavailable => panic!("preparation failed"),
            }
        }
    })
    .await
    .unwrap()
}
fn request(reference: &QueryArtifactRef) -> Value {
    json!({"schemaVersion":REQUEST_SCHEMA,"publicationId":Uuid::from_u128(99),"artifact":reference,"process":{"id":"11111111-1111-4111-8111-111111111111","version":"01.00.000"},"query":{"kind":"lci","limit":1,"cursor":null,"filter":null}})
}
fn headers(body: &[u8], timestamp: u64) -> HeaderMap {
    let timestamp = timestamp.to_string();
    let mut mac = HmacSha256::new_from_slice(SECRET).unwrap();
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    let mut headers = HeaderMap::new();
    headers.insert("x-query-timestamp", timestamp.parse().unwrap());
    headers.insert(
        "x-query-signature",
        hex::encode(mac.finalize().into_bytes()).parse().unwrap(),
    );
    headers
}

#[test]
fn signatures_and_cursors_bind_exact_body_time_and_identity() {
    let payload = b"synthetic-request";
    let signed = headers(payload, 100);
    assert!(authenticate(SECRET, &signed, payload, 120).is_ok());
    assert!(authenticate(SECRET, &signed, payload, 131).is_err());
    assert!(authenticate(SECRET, &signed, b"changed", 120).is_err());
    let reference = QueryArtifactRef {
        schema_version: query_artifact::FORMAT.to_owned(),
        artifact_id: Uuid::from_u128(1),
        manifest_sha256: "a".repeat(64),
        manifest_byte_size: 100,
    };
    let mut req: QueryRequest = serde_json::from_value(request(&reference)).unwrap();
    let cursor = Cursor {
        manifest_sha256: reference.manifest_sha256.clone(),
        publication_id: req.publication_id,
        process: req.process.clone(),
        filter: Some("co2".to_owned()),
        offset: 5,
    };
    let token = encode_cursor(SECRET, &cursor).unwrap();
    assert_eq!(
        decode_cursor(SECRET, &token, &req, Some("co2"))
            .unwrap()
            .offset,
        5
    );
    assert!(decode_cursor(SECRET, &token, &req, None).is_err());
    req.publication_id = Uuid::from_u128(100);
    assert!(decode_cursor(SECRET, &token, &req, Some("co2")).is_err());
}

#[tokio::test]
async fn cache_singleflight_pins_active_reads_and_releases_owned_files() {
    let first = artifact();
    let second = artifact();
    let (_server, store, calls) = object_store(&[&first, &second]).await;
    let directory = tempfile::tempdir().unwrap();
    let cache = Cache::new(
        directory.path(),
        store,
        cache::CacheLimits {
            entries: 1,
            ..cache::CacheLimits::default()
        },
    )
    .unwrap();
    for _ in 0..10 {
        assert!(matches!(
            cache.get(first.reference.clone()).await.unwrap(),
            Lookup::Preparing | Lookup::Ready(_)
        ));
    }
    let active = ready(&cache, &first.reference).await;
    assert_eq!(
        calls.load(Ordering::Relaxed),
        first.manifest.files.len() + 1
    );
    assert!(matches!(
        cache.get(second.reference.clone()).await.unwrap(),
        Lookup::Unavailable
    ));
    drop(active);
    let active = ready(&cache, &second.reference).await;
    assert_eq!(
        calls.load(Ordering::Relaxed),
        first.manifest.files.len() + second.manifest.files.len() + 2
    );
    drop(active);
    drop(cache);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
}

#[tokio::test]
async fn http_auth_precedes_storage_and_service_reads_without_solver() {
    let artifact = artifact();
    let (_objects, store, calls) = object_store(&[&artifact]).await;
    let directory = tempfile::tempdir().unwrap();
    let cache = Cache::new(directory.path(), store, cache::CacheLimits::default()).unwrap();
    let server = serve(router(
        Service::new(cache.clone(), SECRET.to_vec(), 2).unwrap(),
    ))
    .await;
    let client = reqwest::Client::new();
    let url = format!("{}/internal/results/query", server.url);
    let bytes = serde_json::to_vec(&request(&artifact.reference)).unwrap();
    assert_eq!(
        client
            .post(&url)
            .body(bytes.clone())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let response = client
        .post(&url)
        .headers(headers(&bytes, now))
        .body(bytes.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let active = ready(&cache, &artifact.reference).await;
    let response = client
        .post(&url)
        .headers(headers(&bytes, now))
        .body(bytes)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(
        body["artifactId"],
        artifact.reference.artifact_id.to_string()
    );
    assert_eq!(body["data"]["items"].as_array().unwrap().len(), 1);
    assert!(body["data"]["items"][0]["amount"].as_f64().unwrap() > 0.0);
    drop(active);
}

#[tokio::test]
async fn capacity_rejection_and_dedicated_cache_ownership_fail_closed() {
    let artifact = artifact();
    let (_server, store, calls) = object_store(&[&artifact]).await;
    let unrelated = tempfile::tempdir().unwrap();
    std::fs::write(unrelated.path().join("business-data"), b"preserve").unwrap();
    assert!(
        Cache::new(
            unrelated.path(),
            store.clone(),
            cache::CacheLimits::default()
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(unrelated.path().join("business-data")).unwrap(),
        b"preserve"
    );
    let directory = tempfile::tempdir().unwrap();
    let cache = Cache::new(
        directory.path(),
        store,
        cache::CacheLimits {
            memory_bytes: 1,
            ..cache::CacheLimits::default()
        },
    )
    .unwrap();
    assert!(matches!(
        cache.get(artifact.reference.clone()).await.unwrap(),
        Lookup::Preparing
    ));
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(
                cache.get(artifact.reference.clone()).await.unwrap(),
                Lookup::Unavailable
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1); // Manifest only; no oversized data download.
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
}

type MockStore = Arc<(BTreeMap<String, Vec<u8>>, Arc<AtomicUsize>)>;
async fn read(State(state): State<MockStore>, uri: Uri) -> Response {
    state.1.fetch_add(1, Ordering::Relaxed);
    match state.0.get(uri.path()) {
        Some(bytes) => bytes.clone().into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
