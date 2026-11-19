//! AWS Signature Version 4 request signing, and loading S3 credentials from
//! the environment or `~/.aws` (§13.3.1).

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Result, bail};
use sha2::{Digest, Sha256};

use crate::archive::manifest::hex;

#[derive(Clone)]
pub struct Credentials {
    pub access_key: String,
    pub secret_key: String,
    pub session_token: Option<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the secret.
        write!(f, "Credentials({})", self.access_key)
    }
}

/// Parses an INI file into sections of key/value pairs.
fn parse_ini(text: &str) -> HashMap<String, HashMap<String, String>> {
    let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_string();
        } else if let Some((k, v)) = line.split_once('=') {
            out.entry(section.clone())
                .or_default()
                .insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    out
}

fn aws_file(env: &str, name: &str) -> Option<PathBuf> {
    match std::env::var_os(env) {
        Some(p) => Some(PathBuf::from(p)),
        None => crate::paths::home_dir()
            .ok()
            .map(|h| h.join(".aws").join(name)),
    }
}

/// Finds credentials: the `AWS_*` environment variables when no profile is
/// configured, otherwise (or failing that) the profile in `~/.aws/credentials`
/// or `~/.aws/config`.
pub fn load_credentials(profile: Option<&str>) -> Result<Credentials> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    if profile.is_none()
        && let (Some(access_key), Some(secret_key)) =
            (env("AWS_ACCESS_KEY_ID"), env("AWS_SECRET_ACCESS_KEY"))
    {
        return Ok(Credentials {
            access_key,
            secret_key,
            session_token: env("AWS_SESSION_TOKEN"),
        });
    }
    let name = profile
        .map(str::to_string)
        .or_else(|| env("AWS_PROFILE"))
        .unwrap_or_else(|| "default".into());
    let sources = [
        (
            aws_file("AWS_SHARED_CREDENTIALS_FILE", "credentials"),
            name.clone(),
        ),
        (
            aws_file("AWS_CONFIG_FILE", "config"),
            if name == "default" {
                name.clone()
            } else {
                format!("profile {name}")
            },
        ),
    ];
    for (file, section) in sources {
        let Some(file) = file else { continue };
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let ini = parse_ini(&text);
        if let Some(values) = ini.get(&section)
            && let (Some(a), Some(s)) = (
                values.get("aws_access_key_id"),
                values.get("aws_secret_access_key"),
            )
        {
            return Ok(Credentials {
                access_key: a.clone(),
                secret_key: s.clone(),
                session_token: values.get("aws_session_token").cloned(),
            });
        }
    }
    bail!(
        "no S3 credentials for profile {name:?}: add aws_access_key_id and aws_secret_access_key under [{name}] in ~/.aws/credentials"
    )
}

/// Percent-encodes per SigV4: everything but unreserved characters, and `/` unless `keep_slash`.
pub fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'~')
            || (keep_slash && b == b'/')
        {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(message);
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// The canonical request and the signed-header list. `uri` must already be
/// encoded; `headers` are (lowercase name, value) and all of them are signed.
pub fn canonical_request(
    method: &str,
    uri: &str,
    query: &[(String, String)],
    headers: &[(String, String)],
    payload_hash: &str,
) -> (String, String) {
    let mut q: Vec<(String, String)> = query
        .iter()
        .map(|(k, v)| (uri_encode(k, false), uri_encode(v, false)))
        .collect();
    q.sort();
    let query_string = q
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    let mut h: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| {
            (
                k.to_ascii_lowercase(),
                v.split_whitespace().collect::<Vec<_>>().join(" "),
            )
        })
        .collect();
    h.sort();
    let canonical_headers: String = h.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed = h
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    (
        format!("{method}\n{uri}\n{query_string}\n{canonical_headers}\n{signed}\n{payload_hash}"),
        signed,
    )
}

/// The signature for a canonical request. `amz_date` is `YYYYMMDDTHHMMSSZ`.
pub fn signature(
    secret: &str,
    amz_date: &str,
    region: &str,
    service: &str,
    canonical: &str,
) -> String {
    let date = &amz_date[..8];
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical.as_bytes())
    );
    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    hex(&hmac_sha256(&k_signing, to_sign.as_bytes()))
}

/// Signs an S3 request: returns the headers to send, including Authorization.
/// `headers` must include `host`.
#[allow(clippy::too_many_arguments)]
pub fn sign(
    creds: &Credentials,
    region: &str,
    amz_date: &str,
    method: &str,
    uri: &str,
    query: &[(String, String)],
    mut headers: Vec<(String, String)>,
    payload_hash: &str,
) -> Vec<(String, String)> {
    headers.push(("x-amz-date".into(), amz_date.into()));
    headers.push(("x-amz-content-sha256".into(), payload_hash.into()));
    if let Some(token) = &creds.session_token {
        headers.push(("x-amz-security-token".into(), token.clone()));
    }
    let (canonical, signed) = canonical_request(method, uri, query, &headers, payload_hash);
    let sig = signature(&creds.secret_key, amz_date, region, "s3", &canonical);
    headers.push((
        "authorization".into(),
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{}/{region}/s3/aws4_request, SignedHeaders={signed}, Signature={sig}",
            creds.access_key,
            &amz_date[..8]
        ),
    ));
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    /// AWS's worked example for an IAM request.
    #[test]
    fn matches_the_iam_example() {
        let (canonical, signed) = canonical_request(
            "GET",
            "/",
            &[
                ("Action".into(), "ListUsers".into()),
                ("Version".into(), "2010-05-08".into()),
            ],
            &[
                (
                    "content-type".into(),
                    "application/x-www-form-urlencoded; charset=utf-8".into(),
                ),
                ("host".into(), "iam.amazonaws.com".into()),
                ("x-amz-date".into(), "20150830T123600Z".into()),
            ],
            EMPTY,
        );
        assert_eq!(signed, "content-type;host;x-amz-date");
        assert_eq!(
            sha256_hex(canonical.as_bytes()),
            "f536975d06c0309214f805bb90ccff089219ecd68b2577efef23edd43b7e1a59"
        );
        let sig = signature(
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "20150830T123600Z",
            "us-east-1",
            "iam",
            &canonical,
        );
        assert_eq!(
            sig,
            "5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"
        );
    }

    /// AWS's worked example for an S3 GET with a Range header.
    #[test]
    fn matches_the_s3_get_object_example() {
        let creds = Credentials {
            access_key: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
        };
        let headers = sign(
            &creds,
            "us-east-1",
            "20130524T000000Z",
            "GET",
            "/test.txt",
            &[],
            vec![
                ("host".into(), "examplebucket.s3.amazonaws.com".into()),
                ("range".into(), "bytes=0-9".into()),
            ],
            EMPTY,
        );
        let auth = &headers
            .iter()
            .find(|(k, _)| k == "authorization")
            .unwrap()
            .1;
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn encodes_uris() {
        assert_eq!(uri_encode("a b/c~d", true), "a%20b/c~d");
        assert_eq!(uri_encode("a b/c", false), "a%20b%2Fc");
        assert_eq!(uri_encode("é", true), "%C3%A9");
    }

    #[test]
    fn reads_profiles() {
        let ini = parse_ini(
            "[default]\naws_access_key_id = A\n\n[profile spaces]\naws_secret_access_key=S\n# c\n",
        );
        assert_eq!(ini["default"]["aws_access_key_id"], "A");
        assert_eq!(ini["profile spaces"]["aws_secret_access_key"], "S");
    }
}
