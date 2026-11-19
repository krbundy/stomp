//! The S3 client against a small in-process mock of the S3 API: uploads,
//! multipart uploads that resume, retries, verification and the doctor's
//! checksum probes.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use md5::{Digest, Md5};
use sha2::Sha256;

use super::*;
use crate::config::S3Destination;

/// An unfinished multipart upload: the recorded checksum, the parts so far, and its key.
type Upload = (Option<String>, BTreeMap<u32, Vec<u8>>, String);

#[derive(Default)]
struct State {
    objects: HashMap<String, (Vec<u8>, Option<String>)>,
    uploads: HashMap<String, Upload>,
    next_upload: usize,
    parts_received: usize,
    /// Fail uploads of this part number with a non-retryable error.
    refuse_part: Option<u32>,
    /// Fail this many requests with a 503 first.
    throttle: usize,
    /// Don't check Content-MD5 or the payload hash, like a careless server.
    lenient: bool,
}

struct Mock {
    endpoint: String,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn header(req: &tiny_http::Request, name: &str) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

fn respond(req: tiny_http::Request, status: u16, body: Vec<u8>, headers: &[(&str, String)]) {
    let mut r = tiny_http::Response::from_data(body).with_status_code(status);
    for (k, v) in headers {
        r.add_header(tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap());
    }
    let _ = req.respond(r);
}

fn error(req: tiny_http::Request, status: u16, code: &str) {
    let body = format!("<Error><Code>{code}</Code><Message>mock</Message></Error>");
    respond(req, status, body.into_bytes(), &[]);
}

fn mock() -> Mock {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();
    let state = Arc::new(Mutex::new(State::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let (st, sp) = (state.clone(), stop.clone());
    std::thread::spawn(move || {
        while !sp.load(Ordering::SeqCst) {
            let Ok(Some(mut req)) = server.recv_timeout(std::time::Duration::from_millis(50))
            else {
                continue;
            };
            let mut body = Vec::new();
            req.as_reader().read_to_end(&mut body).unwrap();
            let url = req.url().to_string();
            let (path, query) = url.split_once('?').unwrap_or((&url, ""));
            let query: HashMap<String, String> = query
                .split('&')
                .filter(|s| !s.is_empty())
                .map(|kv| {
                    let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                    (k.to_string(), v.replace("%2F", "/").replace("%20", " "))
                })
                .collect();
            // Path style: /<bucket>/<key>
            let key = path.splitn(3, '/').nth(2).unwrap_or("").replace("%20", " ");
            let method = req.method().as_str().to_string();
            let mut s = st.lock().unwrap();
            if s.throttle > 0 {
                s.throttle -= 1;
                drop(s);
                error(req, 503, "SlowDown");
                continue;
            }
            if !s.lenient && matches!(method.as_str(), "PUT" | "POST") {
                if let Some(md5) = header(&req, "content-md5")
                    && md5 != BASE64.encode(Md5::digest(&body))
                {
                    drop(s);
                    error(req, 400, "BadDigest");
                    continue;
                }
                if header(&req, "x-amz-content-sha256").as_deref()
                    != Some(&hex(&Sha256::digest(&body)))
                {
                    drop(s);
                    error(req, 400, "XAmzContentSHA256Mismatch");
                    continue;
                }
            }
            match (
                method.as_str(),
                query.contains_key("uploads"),
                query.get("uploadId"),
            ) {
                ("POST", true, _) => {
                    s.next_upload += 1;
                    let id = format!("upload-{}", s.next_upload);
                    s.uploads.insert(
                        id.clone(),
                        (
                            header(&req, "x-amz-meta-stomp-sha256"),
                            BTreeMap::new(),
                            key.clone(),
                        ),
                    );
                    drop(s);
                    respond(
                        req,
                        200,
                        format!("<R><UploadId>{id}</UploadId></R>").into_bytes(),
                        &[],
                    );
                }
                ("PUT", _, Some(id)) => {
                    let n: u32 = query["partNumber"].parse().unwrap();
                    if s.refuse_part == Some(n) {
                        drop(s);
                        error(req, 403, "AccessDenied");
                        continue;
                    }
                    s.parts_received += 1;
                    let etag = format!("\"{}\"", hex(&Md5::digest(&body)));
                    s.uploads.get_mut(id).unwrap().1.insert(n, body);
                    drop(s);
                    respond(req, 200, Vec::new(), &[("ETag", etag)]);
                }
                ("GET", true, None) => {
                    let prefix = query.get("prefix").cloned().unwrap_or_default();
                    let mut xml = String::from("<ListMultipartUploadsResult>");
                    for (id, (_, _, k)) in &s.uploads {
                        if k.starts_with(&prefix) {
                            xml += &format!(
                                "<Upload><Key>{k}</Key><UploadId>{id}</UploadId></Upload>"
                            );
                        }
                    }
                    xml += "<IsTruncated>false</IsTruncated></ListMultipartUploadsResult>";
                    drop(s);
                    respond(req, 200, xml.into_bytes(), &[]);
                }
                ("DELETE", _, Some(id)) => {
                    let found = s.uploads.remove(id).is_some();
                    drop(s);
                    if found {
                        respond(req, 204, Vec::new(), &[]);
                    } else {
                        error(req, 404, "NoSuchUpload");
                    }
                }
                ("GET", _, Some(id)) => {
                    let Some((_, parts, _)) = s.uploads.get(id) else {
                        drop(s);
                        error(req, 404, "NoSuchUpload");
                        continue;
                    };
                    let mut xml = String::from("<ListPartsResult>");
                    for (n, data) in parts {
                        xml += &format!(
                            "<Part><PartNumber>{n}</PartNumber><ETag>&quot;{}&quot;</ETag></Part>",
                            hex(&Md5::digest(data))
                        );
                    }
                    xml += "<IsTruncated>false</IsTruncated></ListPartsResult>";
                    drop(s);
                    respond(req, 200, xml.into_bytes(), &[]);
                }
                ("POST", _, Some(id)) => {
                    let (meta, parts, _) = s.uploads.remove(id).unwrap();
                    let data: Vec<u8> = parts.into_values().flatten().collect();
                    s.objects.insert(key, (data, meta));
                    drop(s);
                    respond(req, 200, b"<CompleteMultipartUploadResult/>".to_vec(), &[]);
                }
                ("PUT", _, None) => {
                    s.objects
                        .insert(key, (body, header(&req, "x-amz-meta-stomp-sha256")));
                    drop(s);
                    respond(req, 200, Vec::new(), &[]);
                }
                ("HEAD", _, _) => match s.objects.get(&key) {
                    Some((data, meta)) => {
                        let mut headers = vec![];
                        if let Some(m) = meta {
                            headers.push(
                                tiny_http::Header::from_bytes(
                                    "x-amz-meta-stomp-sha256",
                                    m.as_bytes(),
                                )
                                .unwrap(),
                            );
                        }
                        let len = data.len();
                        drop(s);
                        // Keep Content-Length: tiny_http switches big responses to chunked encoding.
                        let r = tiny_http::Response::new(
                            tiny_http::StatusCode(200),
                            headers,
                            std::io::empty(),
                            Some(len),
                            None,
                        )
                        .with_chunked_threshold(usize::MAX);
                        let _ = req.respond(r);
                    }
                    None => {
                        drop(s);
                        respond(req, 404, Vec::new(), &[]);
                    }
                },
                ("GET", _, None) if query.contains_key("list-type") => {
                    let prefix = query.get("prefix").cloned().unwrap_or_default();
                    let bucket_only = key.is_empty();
                    assert!(bucket_only, "listing must address the bucket");
                    let mut xml = String::from("<ListBucketResult>");
                    let mut keys: Vec<_> = s
                        .objects
                        .iter()
                        .filter(|(k, _)| k.starts_with(&prefix))
                        .collect();
                    keys.sort_by(|a, b| a.0.cmp(b.0));
                    for (k, (data, _)) in keys {
                        xml += &format!(
                            "<Contents><Key>{k}</Key><Size>{}</Size></Contents>",
                            data.len()
                        );
                    }
                    xml += "<IsTruncated>false</IsTruncated></ListBucketResult>";
                    drop(s);
                    respond(req, 200, xml.into_bytes(), &[]);
                }
                ("GET", _, None) => match s.objects.get(&key) {
                    Some((data, _)) => {
                        let data = data.clone();
                        drop(s);
                        respond(req, 200, data, &[]);
                    }
                    None => {
                        drop(s);
                        error(req, 404, "NoSuchKey");
                    }
                },
                ("DELETE", _, _) => {
                    s.objects.remove(&key);
                    drop(s);
                    respond(req, 204, Vec::new(), &[]);
                }
                _ => {
                    drop(s);
                    error(req, 400, "Unsupported");
                }
            }
        }
    });
    Mock {
        endpoint: format!("http://127.0.0.1:{port}"),
        state,
        stop,
    }
}

fn client(mock: &Mock, verify: S3Verify) -> S3 {
    let cfg = S3Destination {
        name: "test".into(),
        bucket: "bucket".into(),
        prefix: Some("stomp/u".into()),
        endpoint: Some(mock.endpoint.clone()),
        region: Some("us-east-1".into()),
        profile: None,
        force_path_style: Some(true),
        storage_class: None,
        parallel_uploads: Some(2),
        verify: Some(verify),
    };
    let creds = Credentials {
        access_key: "test".into(),
        secret_key: "secret".into(),
        session_token: None,
    };
    let mut s3 = S3::with_credentials(&cfg, creds).unwrap();
    s3.set_min_part(64 << 10);
    s3
}

fn file(dir: &Path, name: &str, len: usize) -> (std::path::PathBuf, String) {
    let data: Vec<u8> = (0..len).map(|i| (i * 7 % 251) as u8).collect();
    let path = dir.join(name);
    std::fs::write(&path, &data).unwrap();
    (path, sigv4::sha256_hex(&data))
}

#[test]
fn uploads_verifies_downloads_lists_and_deletes() {
    let m = mock();
    let s3 = client(&m, S3Verify::ServerChecksum);
    let tmp = tempfile::tempdir().unwrap();
    let (small, sha) = file(tmp.path(), "small", 1000);
    s3.put(&small, "G/A/small", &sha, tmp.path()).unwrap();
    assert_eq!(s3.location("G/A/small"), "s3://bucket/stomp/u/G/A/small");

    let back = tmp.path().join("back");
    s3.get("G/A/small", &back).unwrap();
    assert_eq!(
        std::fs::read(&back).unwrap(),
        std::fs::read(&small).unwrap()
    );
    assert_eq!(s3.list("G/A").unwrap(), [("small".to_string(), 1000)]);
    s3.delete("G/A/small").unwrap();
    assert!(s3.verify("G/A/small", &sha, 1000, tmp.path()).is_err());
}

#[test]
fn multipart_upload_resumes_after_an_interruption() {
    let m = mock();
    let s3 = client(&m, S3Verify::Readback);
    let tmp = tempfile::tempdir().unwrap();
    let (big, sha) = file(tmp.path(), "big", 300 << 10); // five 64 KiB parts

    m.state.lock().unwrap().refuse_part = Some(4);
    assert!(s3.put(&big, "G/A/big", &sha, tmp.path()).is_err());
    let sent_first = m.state.lock().unwrap().parts_received;
    assert!(
        tmp.path().join("big.upload").exists(),
        "the upload ID is kept for resuming"
    );

    m.state.lock().unwrap().refuse_part = None;
    s3.put(&big, "G/A/big", &sha, tmp.path()).unwrap();
    let s = m.state.lock().unwrap();
    assert_eq!(
        s.parts_received - sent_first,
        5 - sent_first,
        "only missing parts are sent again"
    );
    assert_eq!(s.objects["stomp/u/G/A/big"].0, std::fs::read(&big).unwrap());
    drop(s);
    assert!(!tmp.path().join("big.upload").exists());
}

#[test]
fn deleting_an_archive_aborts_its_unfinished_uploads() {
    let m = mock();
    let s3 = client(&m, S3Verify::ServerChecksum);
    let tmp = tempfile::tempdir().unwrap();
    let (small, sha) = file(tmp.path(), "small", 1000);
    s3.put(&small, "G/A/chunk-0001.tar.zst", &sha, tmp.path())
        .unwrap();
    s3.put(&small, "G/A/manifest.jsonl.zst", &sha, tmp.path())
        .unwrap();
    s3.put(&small, "G/B/chunk-0001.tar.zst", &sha, tmp.path())
        .unwrap();
    // An upload that was interrupted: its parts are stored until aborted.
    let (big, big_sha) = file(tmp.path(), "big", 300 << 10);
    m.state.lock().unwrap().refuse_part = Some(4);
    assert!(
        s3.put(&big, "G/A/chunk-0002.tar.zst", &big_sha, tmp.path())
            .is_err()
    );
    assert_eq!(m.state.lock().unwrap().uploads.len(), 1);

    crate::dest::delete_archive(&s3, "G/A").unwrap();
    let s = m.state.lock().unwrap();
    assert!(s.uploads.is_empty(), "the unfinished upload was aborted");
    let left: Vec<&String> = s.objects.keys().collect();
    assert_eq!(
        left,
        ["stomp/u/G/B/chunk-0001.tar.zst"],
        "only the other archive is left"
    );
}

#[test]
fn retries_when_throttled() {
    let m = mock();
    let s3 = client(&m, S3Verify::ServerChecksum);
    let tmp = tempfile::tempdir().unwrap();
    let (small, sha) = file(tmp.path(), "small", 10);
    m.state.lock().unwrap().throttle = 1;
    s3.put(&small, "G/A/x", &sha, tmp.path()).unwrap();
}

#[test]
fn detects_a_damaged_object() {
    let m = mock();
    let tmp = tempfile::tempdir().unwrap();
    let (small, sha) = file(tmp.path(), "small", 5000);
    client(&m, S3Verify::ServerChecksum)
        .put(&small, "G/A/x", &sha, tmp.path())
        .unwrap();
    m.state
        .lock()
        .unwrap()
        .objects
        .get_mut("stomp/u/G/A/x")
        .unwrap()
        .0[10] ^= 1;
    // The recorded checksum still matches, so only reading it back can tell.
    assert!(
        client(&m, S3Verify::ServerChecksum)
            .verify("G/A/x", &sha, 5000, tmp.path())
            .is_ok()
    );
    assert!(
        client(&m, S3Verify::Readback)
            .verify("G/A/x", &sha, 5000, tmp.path())
            .is_err()
    );
}

#[test]
fn doctor_probes_checksum_enforcement() {
    let m = mock();
    let checks = client(&m, S3Verify::ServerChecksum).check();
    assert!(
        checks
            .iter()
            .any(|c| matches!(c, Check::Pass(t) if t.contains("Content-MD5 or SHA-256"))),
        "{checks:?}"
    );
    m.state.lock().unwrap().lenient = true;
    let checks = client(&m, S3Verify::ServerChecksum).check();
    assert!(
        checks
            .iter()
            .any(|c| matches!(c, Check::Fail(t) if t.contains("readback"))),
        "{checks:?}"
    );
    assert!(
        m.state.lock().unwrap().objects.is_empty(),
        "the test object is cleaned up"
    );
    let _ = AtomicUsize::new(0);
}
