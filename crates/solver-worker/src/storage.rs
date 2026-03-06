use std::collections::BTreeMap;

use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::{Method, StatusCode, Url};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const SIGV4_ALGORITHM: &str = "AWS4-HMAC-SHA256";
const SIGV4_SERVICE: &str = "s3";
const SIGV4_TERMINATOR: &str = "aws4_request";

type HmacSha256 = Hmac<Sha256>;
const EMPTY_PAYLOAD_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// S3-compatible object storage client using path-style URL uploads.
#[derive(Debug, Clone)]
pub struct ObjectStoreClient {
    endpoint: String,
    region: String,
    bucket: String,
    prefix: String,
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
    client: reqwest::Client,
}

impl ObjectStoreClient {
    /// Creates storage client from config.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        endpoint: &str,
        region: &str,
        bucket: &str,
        prefix: &str,
        access_key_id: &str,
        secret_access_key: &str,
        session_token: Option<String>,
    ) -> anyhow::Result<Self> {
        let endpoint = endpoint.trim_end_matches('/').to_owned();
        let region = region.trim().to_owned();
        let bucket = bucket.trim().to_owned();
        let access_key_id = access_key_id.trim().to_owned();
        let secret_access_key = secret_access_key.trim().to_owned();

        if endpoint.is_empty() {
            return Err(anyhow::anyhow!("S3 endpoint must not be empty"));
        }
        if region.is_empty() {
            return Err(anyhow::anyhow!("S3 region must not be empty"));
        }
        if bucket.is_empty() {
            return Err(anyhow::anyhow!("S3 bucket must not be empty"));
        }
        if access_key_id.is_empty() {
            return Err(anyhow::anyhow!("S3 access key id must not be empty"));
        }
        if secret_access_key.is_empty() {
            return Err(anyhow::anyhow!("S3 secret access key must not be empty"));
        }

        Ok(Self {
            endpoint,
            region,
            bucket,
            prefix: prefix.trim_matches('/').to_owned(),
            access_key_id,
            secret_access_key,
            session_token,
            client: reqwest::Client::new(),
        })
    }

    /// Uploads bytes to object storage and returns object URL.
    pub async fn upload_result(
        &self,
        snapshot_id: Uuid,
        job_id: Uuid,
        suffix: &str,
        extension: &str,
        content_type: &str,
        bytes: Vec<u8>,
    ) -> anyhow::Result<String> {
        let key = self.object_key(snapshot_id, job_id, suffix, extension);
        self.upload_object(&key, content_type, bytes).await
    }

    /// Uploads one snapshot artifact object and returns object URL.
    pub async fn upload_snapshot_artifact(
        &self,
        snapshot_id: Uuid,
        extension: &str,
        content_type: &str,
        bytes: Vec<u8>,
    ) -> anyhow::Result<String> {
        let key = if self.prefix.is_empty() {
            format!("snapshots/{snapshot_id}/snapshot/sparse.{extension}")
        } else {
            format!(
                "{}/snapshots/{snapshot_id}/snapshot/sparse.{extension}",
                self.prefix
            )
        };
        self.upload_object(&key, content_type, bytes).await
    }

    /// Deletes an object by full object URL.
    pub async fn delete_object_url(&self, object_url: &str) -> anyhow::Result<()> {
        let url = Url::parse(object_url)
            .map_err(|err| anyhow::anyhow!("invalid object URL {object_url}: {err}"))?;
        let host = canonical_host(&url)?;

        let payload_hash = EMPTY_PAYLOAD_SHA256;
        let (amz_date, date_stamp) = sigv4_timestamps();
        let signed = self.sign_request(
            &Method::DELETE,
            SigV4Input {
                canonical_uri: url.path(),
                canonical_query: url.query().unwrap_or_default(),
                host: &host,
                content_type: None,
                payload_hash,
                amz_date: &amz_date,
                date_stamp: &date_stamp,
            },
        )?;

        let mut request = self
            .client
            .delete(url)
            .header("host", host)
            .header("x-amz-content-sha256", payload_hash)
            .header("x-amz-date", amz_date)
            .header("authorization", signed.authorization);
        if let Some(token) = &self.session_token {
            request = request.header("x-amz-security-token", token);
        }

        let response = request.send().await?;
        if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
            return Ok(());
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let body_preview = body.chars().take(400).collect::<String>();
        Err(anyhow::anyhow!(
            "object delete failed status={status} body={body_preview}"
        ))
    }

    /// Downloads bytes from object URL.
    pub async fn download_object_url(&self, object_url: &str) -> anyhow::Result<Vec<u8>> {
        let url = Url::parse(object_url)
            .map_err(|err| anyhow::anyhow!("invalid object URL {object_url}: {err}"))?;
        let host = canonical_host(&url)?;
        let unsigned_response = self.client.get(url.clone()).send().await?;
        if unsigned_response.status().is_success() {
            return Ok(unsigned_response.bytes().await?.to_vec());
        }

        // Retry with SigV4 for private buckets.
        let payload_hash = EMPTY_PAYLOAD_SHA256;
        let (amz_date, date_stamp) = sigv4_timestamps();
        let signed = self.sign_request(
            &Method::GET,
            SigV4Input {
                canonical_uri: url.path(),
                canonical_query: url.query().unwrap_or_default(),
                host: &host,
                content_type: None,
                payload_hash,
                amz_date: &amz_date,
                date_stamp: &date_stamp,
            },
        )?;

        let mut request = self
            .client
            .get(url)
            .header("host", host)
            .header("x-amz-content-sha256", payload_hash)
            .header("x-amz-date", amz_date)
            .header("authorization", signed.authorization);
        if let Some(token) = &self.session_token {
            request = request.header("x-amz-security-token", token);
        }

        let response = request.send().await?;
        if response.status().is_success() {
            return Ok(response.bytes().await?.to_vec());
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let body_preview = body.chars().take(400).collect::<String>();
        Err(anyhow::anyhow!(
            "object download failed status={status} body={body_preview}"
        ))
    }

    async fn upload_object(
        &self,
        key: &str,
        content_type: &str,
        bytes: Vec<u8>,
    ) -> anyhow::Result<String> {
        let object_url = format!("{}/{}/{}", self.endpoint, self.bucket, key);
        let url = Url::parse(&object_url)
            .map_err(|err| anyhow::anyhow!("invalid S3 URL {object_url}: {err}"))?;
        let host = canonical_host(&url)?;

        let payload_hash = sha256_hex(bytes.as_slice());
        let (amz_date, date_stamp) = sigv4_timestamps();
        let signed = self.sign_request(
            &Method::PUT,
            SigV4Input {
                canonical_uri: url.path(),
                canonical_query: url.query().unwrap_or_default(),
                host: &host,
                content_type: Some(content_type),
                payload_hash: &payload_hash,
                amz_date: &amz_date,
                date_stamp: &date_stamp,
            },
        )?;

        let mut request = self
            .client
            .put(url)
            .header("host", host)
            .header("content-type", content_type)
            .header("x-amz-content-sha256", payload_hash)
            .header("x-amz-date", amz_date)
            .header("authorization", signed.authorization)
            .body(bytes);

        if let Some(token) = &self.session_token {
            request = request.header("x-amz-security-token", token);
        }

        let response = request.send().await?;
        if response.status().is_success() {
            return Ok(object_url);
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let body_preview = body.chars().take(400).collect::<String>();
        let auth_hint = if status == StatusCode::FORBIDDEN || status == StatusCode::UNAUTHORIZED {
            " (check S3 key/secret/region, bucket policy, and endpoint)"
        } else {
            ""
        };

        Err(anyhow::anyhow!(
            "object upload failed status={status}{auth_hint} body={body_preview}"
        ))
    }

    fn object_key(&self, snapshot_id: Uuid, job_id: Uuid, suffix: &str, extension: &str) -> String {
        if self.prefix.is_empty() {
            return format!("snapshots/{snapshot_id}/jobs/{job_id}/{suffix}.{extension}");
        }

        format!(
            "{}/snapshots/{snapshot_id}/jobs/{job_id}/{suffix}.{extension}",
            self.prefix
        )
    }

    fn sign_request(
        &self,
        method: &Method,
        input: SigV4Input<'_>,
    ) -> anyhow::Result<SignedRequest> {
        let mut headers = BTreeMap::<&str, String>::new();
        headers.insert("host", input.host.to_owned());
        headers.insert("x-amz-content-sha256", input.payload_hash.to_owned());
        headers.insert("x-amz-date", input.amz_date.to_owned());
        if let Some(content_type) = input.content_type {
            headers.insert("content-type", content_type.trim().to_owned());
        }
        if let Some(token) = &self.session_token {
            headers.insert("x-amz-security-token", token.trim().to_owned());
        }

        let canonical_headers = headers
            .iter()
            .map(|(name, value)| format!("{name}:{}", value.trim()))
            .collect::<Vec<_>>()
            .join("\n");
        let signed_headers = headers.keys().copied().collect::<Vec<_>>().join(";");

        let canonical_request = format!(
            "{}\n{}\n{}\n{canonical_headers}\n\n{signed_headers}\n{}",
            method.as_str(),
            input.canonical_uri,
            input.canonical_query,
            input.payload_hash
        );
        let canonical_request_hash = sha256_hex(canonical_request.as_bytes());
        let credential_scope = format!(
            "{}/{}/{SIGV4_SERVICE}/{SIGV4_TERMINATOR}",
            input.date_stamp, self.region
        );
        let string_to_sign = format!(
            "{SIGV4_ALGORITHM}\n{}\n{credential_scope}\n{canonical_request_hash}",
            input.amz_date
        );

        let signing_key = self.signing_key(input.date_stamp)?;
        let signature = hmac_sha256_hex(signing_key.as_slice(), &string_to_sign)?;
        let authorization = format!(
            "{SIGV4_ALGORITHM} Credential={}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}",
            self.access_key_id
        );

        Ok(SignedRequest { authorization })
    }

    fn signing_key(&self, date_stamp: &str) -> anyhow::Result<Vec<u8>> {
        let date_key = hmac_sha256_bytes(
            format!("AWS4{}", self.secret_access_key).as_bytes(),
            date_stamp,
        )?;
        let region_key = hmac_sha256_bytes(date_key.as_slice(), &self.region)?;
        let service_key = hmac_sha256_bytes(region_key.as_slice(), SIGV4_SERVICE)?;
        hmac_sha256_bytes(service_key.as_slice(), SIGV4_TERMINATOR)
    }
}

#[derive(Debug)]
struct SignedRequest {
    authorization: String,
}

#[derive(Debug, Clone, Copy)]
struct SigV4Input<'a> {
    canonical_uri: &'a str,
    canonical_query: &'a str,
    host: &'a str,
    content_type: Option<&'a str>,
    payload_hash: &'a str,
    amz_date: &'a str,
    date_stamp: &'a str,
}

fn sigv4_timestamps() -> (String, String) {
    let now = Utc::now();
    (
        now.format("%Y%m%dT%H%M%SZ").to_string(),
        now.format("%Y%m%d").to_string(),
    )
}

fn canonical_host(url: &Url) -> anyhow::Result<String> {
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("S3 endpoint URL is missing host"))?;
    match url.port() {
        Some(port) => Ok(format!("{host}:{port}")),
        None => Ok(host.to_owned()),
    }
}

fn sha256_hex(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    hex::encode(hasher.finalize())
}

fn hmac_sha256_bytes(key: &[u8], data: &str) -> anyhow::Result<Vec<u8>> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|_| anyhow::anyhow!("failed to initialize HMAC-SHA256"))?;
    mac.update(data.as_bytes());
    Ok(mac.finalize().into_bytes().to_vec())
}

fn hmac_sha256_hex(key: &[u8], data: &str) -> anyhow::Result<String> {
    let bytes = hmac_sha256_bytes(key, data)?;
    Ok(hex::encode(bytes))
}
