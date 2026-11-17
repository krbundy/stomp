//! S3-compatible object storage (§13.3), such as DigitalOcean Spaces.
//!
//! A small built-in client: SigV4-signed HTTPS requests, multipart uploads
//! that resume after an interruption, and retries with backoff. Each request
//! body is signed with its SHA-256 and sent with a Content-MD5, so the server
//! rejects anything damaged in transit; `stomp doctor` checks that it really
//! does (§13.3.3).

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use md5::{Digest, Md5};
use sha2::Sha256;

use super::sigv4::{self, Credentials};
use super::{Check, Destination};
use crate::archive::manifest::hex;
use crate::config::{S3Destination, S3Verify};

/// Smallest part of a multipart upload; S3 allows at most 10,000 parts.
const MIN_PART: u64 = 64 << 20;
const MAX_PARTS: u64 = 9000;
const ATTEMPTS: u32 = 6;

pub struct S3 {
    name: String,
    bucket: String,
    prefix: String,
    scheme: String,
    /// The endpoint's host, with a port if it isn't the default.
    host: String,
    region: String,
    path_style: bool,
    storage_class: Option<String>,
    verify_mode: S3Verify,
    parallel: usize,
    min_part: u64,
    creds: Credentials,
    agent: ureq::Agent,
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn ok(self, what: &str) -> Result<Response> {
        if (200..300).contains(&self.status) {
            return Ok(self);
        }
        let body = self.text();
        let code = tag(&body, "Code").unwrap_or_else(|| format!("HTTP {}", self.status));
        let message = tag(&body, "Message").unwrap_or_default();
        bail!("{what} failed: {code} {message}")
    }
}

// --- Minimal XML: S3 responses are simple, flat and attribute-free. ---

fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The inner text of every `<name>` element.
fn blocks<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<{name}>"), format!("</{name}>"));
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else { break };
        out.push(&after[..end]);
        rest = &after[end + close.len()..];
    }
    out
}

fn tag(xml: &str, name: &str) -> Option<String> {
    blocks(xml, name).first().map(|s| unescape(s))
}

impl S3 {
    pub fn new(cfg: &S3Destination) -> Result<S3> {
        let creds = sigv4::load_credentials(cfg.profile.as_deref())
            .with_context(|| format!("destination {}", cfg.name))?;
        Self::with_credentials(cfg, creds)
    }

    pub fn with_credentials(cfg: &S3Destination, creds: Credentials) -> Result<S3> {
        let region = cfg
            .region
            .clone()
            .unwrap_or_else(|| "us-east-1".to_string());
        let endpoint = cfg
            .endpoint
            .clone()
            .unwrap_or_else(|| format!("https://s3.{region}.amazonaws.com"));
        let (scheme, rest) = endpoint.split_once("://").with_context(|| {
            format!(
                "destination {}: endpoint must start with https://",
                cfg.name
            )
        })?;
        let host = rest.trim_end_matches('/').to_string();
        if host.is_empty() || host.contains('/') {
            bail!(
                "destination {}: endpoint should be just a URL like https://nyc3.digitaloceanspaces.com",
                cfg.name
            );
        }
        let prefix = match &cfg.prefix {
            Some(p) => crate::paths::expand(p)?
                .to_string_lossy()
                .trim_matches('/')
                .to_string(),
            None => String::new(),
        };
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(30)))
            .user_agent(format!("stomp/{}", env!("CARGO_PKG_VERSION")))
            .build();
        Ok(S3 {
            name: cfg.name.clone(),
            bucket: cfg.bucket.clone(),
            prefix,
            scheme: scheme.to_string(),
            host,
            region,
            path_style: cfg.force_path_style.unwrap_or(false),
            storage_class: cfg.storage_class.clone().filter(|c| c != "STANDARD"),
            verify_mode: cfg.verify.unwrap_or(S3Verify::ServerChecksum),
            parallel: cfg.parallel_uploads.unwrap_or(4).clamp(1, 16) as usize,
            min_part: MIN_PART,
            creds,
            agent: config.new_agent(),
        })
    }

    /// Smaller parts, so tests can exercise multipart uploads with small files.
    #[cfg(test)]
    pub fn set_min_part(&mut self, bytes: u64) {
        self.min_part = bytes;
    }

    fn key(&self, rel: &str) -> String {
        if self.prefix.is_empty() {
            rel.to_string()
        } else {
            format!("{}/{rel}", self.prefix)
        }
    }

    /// Host and path for an object, or for the bucket itself.
    fn target(&self, key: Option<&str>) -> (String, String) {
        let encoded = key.map(|k| sigv4::uri_encode(k, true)).unwrap_or_default();
        if self.path_style {
            let path = match key {
                Some(_) => format!("/{}/{encoded}", self.bucket),
                None => format!("/{}", self.bucket),
            };
            (self.host.clone(), path)
        } else {
            (
                format!("{}.{}", self.bucket, self.host),
                format!("/{encoded}"),
            )
        }
    }

    /// Sends a signed request, retrying network errors, throttling and server errors.
    fn request(
        &self,
        method: &str,
        key: Option<&str>,
        query: &[(&str, String)],
        extra: &[(&str, String)],
        body: &[u8],
        payload_hash: Option<&str>,
    ) -> Result<Response> {
        let (host, path) = self.target(key);
        let query: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        let query_string = query
            .iter()
            .map(|(k, v)| {
                format!(
                    "{}={}",
                    sigv4::uri_encode(k, false),
                    sigv4::uri_encode(v, false)
                )
            })
            .collect::<Vec<_>>()
            .join("&");
        let url = if query_string.is_empty() {
            format!("{}://{host}{path}", self.scheme)
        } else {
            format!("{}://{host}{path}?{query_string}", self.scheme)
        };
        let payload = payload_hash.map_or_else(|| sigv4::sha256_hex(body), str::to_string);
        let mut last = String::new();
        for attempt in 0..ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(Duration::from_secs(1 << (attempt - 1).min(4)));
            }
            let amz_date = jiff::Timestamp::now()
                .strftime("%Y%m%dT%H%M%SZ")
                .to_string();
            let mut headers = vec![("host".to_string(), host.clone())];
            headers.extend(
                extra
                    .iter()
                    .map(|(k, v)| (k.to_ascii_lowercase(), v.clone())),
            );
            let signed = sigv4::sign(
                &self.creds,
                &self.region,
                &amz_date,
                method,
                &path,
                &query,
                headers,
                &payload,
            );
            let mut builder = ureq::http::Request::builder().method(method).uri(&url);
            for (k, v) in &signed {
                if k != "host" {
                    builder = builder.header(k.as_str(), v.as_str());
                }
            }
            let result = if body.is_empty() && method != "PUT" && method != "POST" {
                self.agent.run(builder.body(())?)
            } else {
                self.agent.run(builder.body(body)?)
            };
            match result {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let headers = response
                        .headers()
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                        .collect();
                    let body = response
                        .into_body()
                        .into_with_config()
                        .limit(64 << 20)
                        .read_to_vec()?;
                    let r = Response {
                        status,
                        headers,
                        body,
                    };
                    if status == 429 || status >= 500 {
                        last = format!(
                            "HTTP {status}: {}",
                            tag(&r.text(), "Code").unwrap_or_default()
                        );
                        continue;
                    }
                    return Ok(r);
                }
                Err(e) => last = e.to_string(),
            }
        }
        bail!(
            "{method} {} kept failing ({last})",
            self.location(key.unwrap_or(""))
        )
    }

    fn content_md5(data: &[u8]) -> (String, String) {
        let digest = Md5::digest(data);
        (BASE64.encode(digest), format!("\"{}\"", hex(&digest)))
    }

    fn put_object(&self, key: &str, data: &[u8], sha256: &str) -> Result<()> {
        let (md5, _) = Self::content_md5(data);
        let mut headers = vec![
            ("content-md5", md5),
            ("x-amz-meta-stomp-sha256", sha256.to_string()),
        ];
        if let Some(class) = &self.storage_class {
            headers.push(("x-amz-storage-class", class.clone()));
        }
        self.request("PUT", Some(key), &[], &headers, data, None)?
            .ok("upload")?;
        Ok(())
    }

    fn create_multipart(&self, key: &str, sha256: &str) -> Result<String> {
        let mut headers = vec![("x-amz-meta-stomp-sha256", sha256.to_string())];
        if let Some(class) = &self.storage_class {
            headers.push(("x-amz-storage-class", class.clone()));
        }
        let r = self
            .request(
                "POST",
                Some(key),
                &[("uploads", String::new())],
                &headers,
                &[],
                None,
            )?
            .ok("starting a multipart upload")?;
        tag(&r.text(), "UploadId").context("the server didn't return an upload ID")
    }

    fn upload_part(&self, key: &str, upload_id: &str, n: u32, data: &[u8]) -> Result<String> {
        let (md5, expected) = Self::content_md5(data);
        let r = self
            .request(
                "PUT",
                Some(key),
                &[
                    ("partNumber", n.to_string()),
                    ("uploadId", upload_id.to_string()),
                ],
                &[("content-md5", md5)],
                data,
                None,
            )?
            .ok(&format!("uploading part {n}"))?;
        let etag = r.header("etag").unwrap_or_default().to_string();
        if etag != expected {
            bail!("part {n} came back with ETag {etag}, expected {expected}");
        }
        Ok(etag)
    }

    /// Parts already uploaded, or `None` if the upload no longer exists.
    fn list_parts(&self, key: &str, upload_id: &str) -> Result<Option<BTreeMap<u32, String>>> {
        let mut parts = BTreeMap::new();
        let mut marker = String::new();
        loop {
            let mut query = vec![("uploadId", upload_id.to_string())];
            if !marker.is_empty() {
                query.push(("part-number-marker", marker.clone()));
            }
            let r = self.request("GET", Some(key), &query, &[], &[], None)?;
            if r.status == 404 {
                return Ok(None);
            }
            let text = r.ok("listing uploaded parts")?.text();
            for part in blocks(&text, "Part") {
                if let (Some(n), Some(etag)) = (tag(part, "PartNumber"), tag(part, "ETag")) {
                    parts.insert(n.parse()?, etag);
                }
            }
            match (
                tag(&text, "IsTruncated").as_deref(),
                tag(&text, "NextPartNumberMarker"),
            ) {
                (Some("true"), Some(next)) => marker = next,
                _ => return Ok(Some(parts)),
            }
        }
    }

    fn complete(&self, key: &str, upload_id: &str, parts: &BTreeMap<u32, String>) -> Result<()> {
        let mut xml = String::from("<CompleteMultipartUpload>");
        for (n, etag) in parts {
            xml += &format!(
                "<Part><PartNumber>{n}</PartNumber><ETag>{}</ETag></Part>",
                escape(etag)
            );
        }
        xml += "</CompleteMultipartUpload>";
        let r = self
            .request(
                "POST",
                Some(key),
                &[("uploadId", upload_id.to_string())],
                &[],
                xml.as_bytes(),
                None,
            )?
            .ok("finishing a multipart upload")?;
        // S3 can report a failure inside a 200 response.
        if let Some(code) = blocks(&r.text(), "Error")
            .first()
            .and_then(|e| tag(e, "Code"))
        {
            bail!("finishing a multipart upload failed: {code}");
        }
        Ok(())
    }

    /// Size and recorded SHA-256 of an object, or `None` if it doesn't exist.
    fn head(&self, key: &str) -> Result<Option<(u64, Option<String>)>> {
        let r = self.request("HEAD", Some(key), &[], &[], &[], None)?;
        if r.status == 404 {
            return Ok(None);
        }
        let r = r.ok("checking an object")?;
        let size = r.header("content-length").unwrap_or("0").parse()?;
        Ok(Some((
            size,
            r.header("x-amz-meta-stomp-sha256").map(str::to_string),
        )))
    }

    fn list_objects(&self, prefix: &str, limit: Option<usize>) -> Result<Vec<(String, u64)>> {
        let mut out = Vec::new();
        let mut token = String::new();
        loop {
            let mut query = vec![
                ("list-type", "2".to_string()),
                ("prefix", prefix.to_string()),
            ];
            if let Some(n) = limit {
                query.push(("max-keys", n.to_string()));
            }
            if !token.is_empty() {
                query.push(("continuation-token", token.clone()));
            }
            let text = self
                .request("GET", None, &query, &[], &[], None)?
                .ok("listing")?
                .text();
            for item in blocks(&text, "Contents") {
                if let (Some(key), Some(size)) = (tag(item, "Key"), tag(item, "Size")) {
                    out.push((key, size.parse().unwrap_or(0)));
                }
            }
            match (
                tag(&text, "IsTruncated").as_deref(),
                tag(&text, "NextContinuationToken"),
            ) {
                (Some("true"), Some(next)) if limit.is_none() => token = next,
                _ => return Ok(out),
            }
        }
    }

    /// Streams an object, returning its SHA-256 and size.
    fn hash_object(&self, key: &str) -> Result<(String, u64)> {
        let mut hasher = Sha256::new();
        let size = self.stream(key, &mut |chunk| {
            hasher.update(chunk);
            Ok(())
        })?;
        Ok((hex(&hasher.finalize()), size))
    }

    /// Downloads an object in pieces, retrying from scratch on failure.
    fn stream(&self, key: &str, sink: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<u64> {
        let (host, path) = self.target(Some(key));
        let url = format!("{}://{host}{path}", self.scheme);
        let amz_date = jiff::Timestamp::now()
            .strftime("%Y%m%dT%H%M%SZ")
            .to_string();
        let empty = sigv4::sha256_hex(b"");
        let signed = sigv4::sign(
            &self.creds,
            &self.region,
            &amz_date,
            "GET",
            &path,
            &[],
            vec![("host".into(), host)],
            &empty,
        );
        let mut builder = ureq::http::Request::builder().method("GET").uri(&url);
        for (k, v) in &signed {
            if k != "host" {
                builder = builder.header(k.as_str(), v.as_str());
            }
        }
        let response = self.agent.run(builder.body(())?)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            bail!("downloading {} failed: HTTP {status}", self.location(key));
        }
        let mut reader = response
            .into_body()
            .into_with_config()
            .limit(u64::MAX)
            .reader();
        let mut buf = vec![0u8; 1 << 20];
        let mut total = 0u64;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                return Ok(total);
            }
            sink(&buf[..n])?;
            total += n as u64;
        }
    }

    fn multipart(&self, local: &Path, key: &str, size: u64, part: u64, sha256: &str) -> Result<()> {
        // The upload ID is kept beside the file, so an interrupted upload resumes.
        let mut state = local.as_os_str().to_owned();
        state.push(".upload");
        let state = std::path::PathBuf::from(state);
        let mut existing = BTreeMap::new();
        let resumed = fs::read_to_string(&state)
            .ok()
            .map(|s| s.trim().to_string());
        let upload_id = match resumed {
            Some(id) => match self.list_parts(key, &id)? {
                Some(parts) => {
                    existing = parts;
                    id
                }
                None => self.start_upload(key, sha256, &state)?,
            },
            None => self.start_upload(key, sha256, &state)?,
        };
        let count = size.div_ceil(part) as u32;
        let file = File::open(local)?;
        let next = AtomicU32::new(1);
        let done: Mutex<BTreeMap<u32, String>> = Mutex::new(BTreeMap::new());
        let failure: Mutex<Option<anyhow::Error>> = Mutex::new(None);
        std::thread::scope(|scope| {
            for _ in 0..self.parallel.min(count as usize) {
                scope.spawn(|| {
                    loop {
                        if failure.lock().unwrap().is_some() {
                            return;
                        }
                        let n = next.fetch_add(1, Ordering::SeqCst);
                        if n > count {
                            return;
                        }
                        let offset = u64::from(n - 1) * part;
                        let mut buf = vec![0u8; part.min(size - offset) as usize];
                        let result = file
                            .read_exact_at(&mut buf, offset)
                            .map_err(anyhow::Error::from)
                            .and_then(|()| {
                                let (_, etag) = Self::content_md5(&buf);
                                if existing.get(&n) == Some(&etag) {
                                    Ok(etag) // already uploaded before an interruption
                                } else {
                                    self.upload_part(key, &upload_id, n, &buf)
                                }
                            });
                        match result {
                            Ok(etag) => {
                                done.lock().unwrap().insert(n, etag);
                            }
                            Err(e) => {
                                *failure.lock().unwrap() = Some(e);
                                return;
                            }
                        }
                    }
                });
            }
        });
        if let Some(e) = failure.into_inner().unwrap() {
            return Err(e);
        }
        self.complete(key, &upload_id, &done.into_inner().unwrap())?;
        let _ = fs::remove_file(&state);
        Ok(())
    }

    fn start_upload(&self, key: &str, sha256: &str, state: &Path) -> Result<String> {
        let id = self.create_multipart(key, sha256)?;
        fs::write(state, &id)?;
        Ok(id)
    }
}

impl Destination for S3 {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> &'static str {
        "s3"
    }

    fn location(&self, rel: &str) -> String {
        let key = if rel.is_empty() {
            self.prefix.clone()
        } else {
            self.key(rel)
        };
        format!("s3://{}/{key}", self.bucket)
    }

    fn put(&self, local: &Path, rel: &str, sha256: &str, scratch: &Path) -> Result<()> {
        let key = self.key(rel);
        let size = fs::metadata(local)?.len();
        let part = self.min_part.max(size.div_ceil(MAX_PARTS));
        if size <= part {
            self.put_object(&key, &fs::read(local)?, sha256)?;
        } else {
            self.multipart(local, &key, size, part, sha256)?;
        }
        self.verify(rel, sha256, size, scratch)
    }

    fn verify(&self, rel: &str, sha256: &str, size: u64, _scratch: &Path) -> Result<()> {
        let key = self.key(rel);
        let (stored, recorded) = self
            .head(&key)?
            .ok_or_else(|| anyhow!("{} isn't there", self.location(rel)))?;
        if stored != size {
            bail!("{} is {stored} bytes, expected {size}", self.location(rel));
        }
        match self.verify_mode {
            S3Verify::Size => Ok(()),
            S3Verify::ServerChecksum => {
                if recorded.as_deref() == Some(sha256) {
                    Ok(())
                } else {
                    bail!("{} doesn't carry the expected checksum", self.location(rel))
                }
            }
            S3Verify::Readback => {
                let (actual, _) = self.hash_object(&key)?;
                if actual == sha256 {
                    Ok(())
                } else {
                    bail!("{} read back with the wrong checksum", self.location(rel))
                }
            }
        }
    }

    fn get(&self, rel: &str, local: &Path) -> Result<()> {
        let tmp = local.with_extension("part");
        let mut out = std::io::BufWriter::new(File::create(&tmp)?);
        let result = self.stream(&self.key(rel), &mut |chunk| {
            std::io::Write::write_all(&mut out, chunk)?;
            Ok(())
        });
        match result {
            Ok(_) => {
                out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
                fs::rename(&tmp, local)?;
                Ok(())
            }
            Err(e) => {
                drop(out);
                let _ = fs::remove_file(&tmp);
                Err(e)
            }
        }
    }

    fn delete(&self, rel: &str) -> Result<()> {
        let r = self.request("DELETE", Some(&self.key(rel)), &[], &[], &[], None)?;
        if r.status != 404 {
            r.ok("deleting")?;
        }
        Ok(())
    }

    fn delete_dir(&self, _rel: &str) -> Result<()> {
        Ok(()) // object storage has no directories
    }

    fn list(&self, rel: &str) -> Result<Vec<(String, u64)>> {
        let prefix = format!("{}/", self.key(rel));
        Ok(self
            .list_objects(&prefix, None)?
            .into_iter()
            .map(|(k, size)| (k.strip_prefix(&prefix).unwrap_or(&k).to_string(), size))
            .collect())
    }

    fn check(&self) -> Vec<Check> {
        let mut out = vec![Check::Pass(format!(
            "credentials found (access key {}…)",
            &self.creds.access_key[..self.creds.access_key.len().min(6)]
        ))];
        if let Err(e) = self.list_objects(&self.prefix, Some(1)) {
            out.push(Check::Fail(format!(
                "can't reach bucket {} at {}: {e:#}",
                self.bucket, self.host
            )));
            return out;
        }
        out.push(Check::Pass(format!(
            "bucket {} is reachable at {}",
            self.bucket, self.host
        )));

        let rel = format!(".stomp-doctor-{}", std::process::id());
        let key = self.key(&rel);
        let data = b"stomp doctor test object";
        let sha = sigv4::sha256_hex(data);
        match self
            .put_object(&key, data, &sha)
            .and_then(|()| self.verify(&rel, &sha, data.len() as u64, Path::new(".")))
        {
            Ok(()) => out.push(Check::Pass("can upload and verify objects".into())),
            Err(e) => {
                out.push(Check::Fail(format!("can't upload a test object: {e:#}")));
                return out;
            }
        }

        // Does the server check what it receives? Send deliberately wrong checksums.
        let wrong_md5 = BASE64.encode(Md5::digest(b"something else"));
        let md5_checked = self
            .request(
                "PUT",
                Some(&key),
                &[],
                &[("content-md5", wrong_md5)],
                data,
                None,
            )
            .is_ok_and(|r| r.status == 400);
        let sha_checked = self
            .request(
                "PUT",
                Some(&key),
                &[],
                &[],
                data,
                Some(&sigv4::sha256_hex(b"something else")),
            )
            .is_ok_and(|r| (400..500).contains(&r.status));
        let _ = self.delete(&rel);
        match (md5_checked, sha_checked) {
            (true, true) => out.push(Check::Pass(
                "the server rejects uploads whose Content-MD5 or SHA-256 doesn't match".into(),
            )),
            (true, false) | (false, true) => out.push(Check::Pass(format!(
                "the server rejects uploads whose {} doesn't match",
                if md5_checked { "Content-MD5" } else { "SHA-256" }
            ))),
            (false, false) if self.verify_mode == S3Verify::ServerChecksum => out.push(Check::Fail(
                "the server accepted uploads with wrong checksums; set verify = \"readback\" for this destination".into(),
            )),
            (false, false) => out.push(Check::Warn(
                "the server accepted uploads with wrong checksums, so stomp relies on verify for integrity".into(),
            )),
        }
        out
    }
}

#[cfg(test)]
mod tests;
