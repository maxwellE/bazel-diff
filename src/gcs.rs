//! Google Cloud access tokens for the GCS tier of the `serve` cache.
//!
//! Credentials resolve the way Google's own client libraries resolve *Application Default
//! Credentials*, in this order:
//!
//! 1. The JSON file named by `GOOGLE_APPLICATION_CREDENTIALS`: a service account key, or the
//!    authorized-user file `gcloud auth application-default login` writes.
//! 2. gcloud's well-known copy of that file, `application_default_credentials.json` under
//!    `$CLOUDSDK_CONFIG`, `%APPDATA%\gcloud` (Windows) or `~/.config/gcloud`.
//! 3. The Compute Engine metadata server, which hands out the attached service account's tokens
//!    on GCE, GKE (Workload Identity), Cloud Run and Cloud Build.
//!
//! Everything is plain blocking HTTP over `attohttpc`, like the Nexus tier: no async runtime, and
//! the only cryptography is the RS256 signature a service account key needs, from `ring`, which
//! rustls already links.

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::rand::SystemRandom;
use ring::signature::{RsaKeyPair, RSA_PKCS1_SHA256};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::PrivateKeyDer;
use serde::Deserialize;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use url::{form_urlencoded, Url};

/// The one scope the cache needs: read existing objects and create new ones.
pub const STORAGE_SCOPE: &str = "https://www.googleapis.com/auth/devstorage.read_write";
pub const CREDENTIALS_FILE_ENV: &str = "GOOGLE_APPLICATION_CREDENTIALS";
const CLOUDSDK_CONFIG_ENV: &str = "CLOUDSDK_CONFIG";
/// Honored like Google's clients do, so a metadata server can be stood in for.
const METADATA_HOST_ENV: &str = "GCE_METADATA_HOST";
const DEFAULT_METADATA_HOST: &str = "metadata.google.internal";
const METADATA_TOKEN_PATH: &str = "/computeMetadata/v1/instance/service-accounts/default/token";
const DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
const WELL_KNOWN_FILE: &str = "application_default_credentials.json";
/// A token is replaced this long before it expires so a request never carries one that lapses
/// in flight.
const REFRESH_MARGIN: Duration = Duration::from_secs(300);
/// Google tokens last an hour; a token endpoint that claims more is not believed past this.
const MAX_TOKEN_LIFETIME: Duration = Duration::from_secs(24 * 3600);
const DEFAULT_TOKEN_LIFETIME: u64 = 3600;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Bounds how much of a token endpoint's error body is echoed into an error.
const ERROR_BODY_LIMIT: usize = 200;

/// The process state Application Default Credentials resolve from, separated out so tests can
/// resolve against a fabricated environment.
#[derive(Clone, Debug, Default)]
pub struct Environment {
    /// `GOOGLE_APPLICATION_CREDENTIALS`.
    pub credentials_file: Option<PathBuf>,
    /// `CLOUDSDK_CONFIG`: where gcloud keeps its configuration instead of the default directory.
    pub cloudsdk_config: Option<PathBuf>,
    /// The directory gcloud's default configuration directory lives in: `%APPDATA%` on Windows,
    /// `~/.config` elsewhere.
    pub config_home: Option<PathBuf>,
    /// `GCE_METADATA_HOST`.
    pub metadata_host: Option<String>,
}

impl Environment {
    pub fn from_process() -> Self {
        let non_empty = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        let config_home = if cfg!(windows) {
            non_empty("APPDATA").map(PathBuf::from)
        } else {
            non_empty("HOME").map(|home| PathBuf::from(home).join(".config"))
        };
        Self {
            credentials_file: non_empty(CREDENTIALS_FILE_ENV).map(PathBuf::from),
            cloudsdk_config: non_empty(CLOUDSDK_CONFIG_ENV).map(PathBuf::from),
            config_home,
            metadata_host: non_empty(METADATA_HOST_ENV)
                .map(|host| host.to_string_lossy().into_owned()),
        }
    }

    fn well_known_file(&self) -> Option<PathBuf> {
        let directory = match (&self.cloudsdk_config, &self.config_home) {
            (Some(directory), _) => directory.clone(),
            (None, Some(home)) => home.join("gcloud"),
            (None, None) => return None,
        };
        Some(directory.join(WELL_KNOWN_FILE))
    }
}

/// Where access tokens come from.
pub enum Credentials {
    /// A service account key: an RS256-signed assertion is exchanged for a token.
    ServiceAccount {
        client_email: String,
        key: Box<RsaKeyPair>,
        token_uri: Url,
    },
    /// gcloud user credentials: a refresh token is exchanged for a token.
    AuthorizedUser {
        client_id: String,
        client_secret: String,
        refresh_token: String,
        token_uri: Url,
    },
    /// The metadata server of the Google Cloud host the service runs on.
    Metadata { token_url: Url },
}

impl fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ServiceAccount {
                client_email,
                token_uri,
                ..
            } => formatter
                .debug_struct("ServiceAccount")
                .field("client_email", client_email)
                .field("token_uri", &token_uri.as_str())
                .finish_non_exhaustive(),
            Self::AuthorizedUser {
                client_id,
                token_uri,
                ..
            } => formatter
                .debug_struct("AuthorizedUser")
                .field("client_id", client_id)
                .field("client_secret", &"<redacted>")
                .field("refresh_token", &"<redacted>")
                .field("token_uri", &token_uri.as_str())
                .finish(),
            Self::Metadata { token_url } => formatter
                .debug_struct("Metadata")
                .field("token_url", &token_url.as_str())
                .finish(),
        }
    }
}

/// The fields of a credentials file this module reads; `type` says which are present.
#[derive(Deserialize)]
struct CredentialsFile {
    #[serde(rename = "type")]
    kind: String,
    client_email: Option<String>,
    private_key: Option<String>,
    token_uri: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    refresh_token: Option<String>,
}

impl Credentials {
    /// Resolves Application Default Credentials, in the order the module documentation lists.
    pub fn resolve(environment: &Environment) -> Result<Self> {
        if let Some(path) = &environment.credentials_file {
            return Self::from_file(path)
                .with_context(|| format!("{CREDENTIALS_FILE_ENV}={}", path.display()));
        }
        if let Some(path) = environment.well_known_file().filter(|path| path.is_file()) {
            return Self::from_file(&path);
        }
        let host = environment
            .metadata_host
            .as_deref()
            .unwrap_or(DEFAULT_METADATA_HOST);
        let token_url = Url::parse(&format!("http://{host}{METADATA_TOKEN_PATH}"))
            .ok()
            .filter(|url| url.host_str().is_some() && url.path() == METADATA_TOKEN_PATH)
            .ok_or_else(|| anyhow!("invalid {METADATA_HOST_ENV} {host:?}"))?;
        Ok(Self::Metadata { token_url })
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        let json = fs::read(path)
            .with_context(|| format!("failed to read credentials file {}", path.display()))?;
        Self::from_json(&json)
            .with_context(|| format!("invalid credentials file {}", path.display()))
    }

    pub fn from_json(json: &[u8]) -> Result<Self> {
        let file: CredentialsFile =
            serde_json::from_slice(json).context("not a JSON credentials file")?;
        let token_uri = match &file.token_uri {
            Some(uri) => Url::parse(uri).with_context(|| format!("invalid token_uri {uri:?}"))?,
            None => Url::parse(DEFAULT_TOKEN_URI).expect("constant URL"),
        };
        let required = |name: &str, value: Option<String>| {
            value
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("missing {name:?}"))
        };
        match file.kind.as_str() {
            "service_account" => Ok(Self::ServiceAccount {
                client_email: required("client_email", file.client_email)?,
                key: Box::new(parse_rsa_key(
                    required("private_key", file.private_key)?.as_bytes(),
                )?),
                token_uri,
            }),
            "authorized_user" => Ok(Self::AuthorizedUser {
                client_id: required("client_id", file.client_id)?,
                client_secret: required("client_secret", file.client_secret)?,
                refresh_token: required("refresh_token", file.refresh_token)?,
                token_uri,
            }),
            other => bail!(
                "unsupported credential type {other:?}; use a service account key, the user \
                 credentials of `gcloud auth application-default login`, or the attached \
                 service account of a Google Cloud host"
            ),
        }
    }
}

fn parse_rsa_key(pem: &[u8]) -> Result<RsaKeyPair> {
    let key = PrivateKeyDer::from_pem_slice(pem)
        .map_err(|error| anyhow!("private_key is not a PEM private key: {error:?}"))?;
    let parsed = match &key {
        PrivateKeyDer::Pkcs8(der) => RsaKeyPair::from_pkcs8(der.secret_pkcs8_der()),
        PrivateKeyDer::Pkcs1(der) => RsaKeyPair::from_der(der.secret_pkcs1_der()),
        _ => bail!("private_key is not an RSA key"),
    };
    parsed.map_err(|error| anyhow!("private_key is not a usable RSA key: {error}"))
}

/// Hands out bearer tokens for [`STORAGE_SCOPE`], fetching one on first use and again once the
/// cached one is within [`REFRESH_MARGIN`] of expiring. Callers that race on an expired token
/// wait for the one refresh rather than each requesting their own.
pub struct TokenSource {
    credentials: Credentials,
    session: attohttpc::Session,
    cached: Mutex<Option<CachedToken>>,
}

struct CachedToken {
    value: String,
    expires_at: Instant,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: Option<u64>,
}

impl TokenSource {
    pub fn new(credentials: Credentials) -> Self {
        let mut session = attohttpc::Session::new();
        session.connect_timeout(CONNECT_TIMEOUT);
        session.read_timeout(READ_TIMEOUT);
        // A redirect is reported, never followed: following one would replay a refresh token or
        // signed assertion against whatever host the `Location` names.
        session.follow_redirects(false);
        Self {
            credentials,
            session,
            cached: Mutex::new(None),
        }
    }

    pub fn token(&self) -> Result<String> {
        let mut cached = self
            .cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(token) = cached
            .as_ref()
            .filter(|token| Instant::now() + REFRESH_MARGIN < token.expires_at)
        {
            return Ok(token.value.clone());
        }
        let (value, lifetime) = self.fetch()?;
        *cached = Some(CachedToken {
            value: value.clone(),
            expires_at: Instant::now() + lifetime,
        });
        Ok(value)
    }

    fn fetch(&self) -> Result<(String, Duration)> {
        let response = match &self.credentials {
            Credentials::ServiceAccount {
                client_email,
                key,
                token_uri,
            } => {
                let assertion = signed_assertion(client_email, key, token_uri)?;
                self.exchange(
                    token_uri,
                    &[
                        ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                        ("assertion", &assertion),
                    ],
                )
            }
            Credentials::AuthorizedUser {
                client_id,
                client_secret,
                refresh_token,
                token_uri,
            } => self.exchange(
                token_uri,
                &[
                    ("grant_type", "refresh_token"),
                    ("client_id", client_id),
                    ("client_secret", client_secret),
                    ("refresh_token", refresh_token),
                ],
            ),
            Credentials::Metadata { token_url } => {
                let mut url = token_url.clone();
                url.query_pairs_mut().append_pair("scopes", STORAGE_SCOPE);
                self.session
                    .get(url.as_str())
                    .header("Metadata-Flavor", "Google")
                    .send()
            }
        };
        let response = response.context("token request failed")?;
        if !response.is_success() {
            let status = response.status().as_u16();
            let body = response.text().unwrap_or_default();
            let body = body.trim();
            let shown = match body.char_indices().nth(ERROR_BODY_LIMIT) {
                Some((cut, _)) => format!("{}...", &body[..cut]),
                None => body.to_owned(),
            };
            bail!("token endpoint answered HTTP {status} with content '{shown}'");
        }
        let body = response
            .bytes()
            .context("failed to read the token response")?;
        let token: TokenResponse =
            serde_json::from_slice(&body).context("token endpoint answered unexpected JSON")?;
        let lifetime = Duration::from_secs(token.expires_in.unwrap_or(DEFAULT_TOKEN_LIFETIME))
            .min(MAX_TOKEN_LIFETIME);
        Ok((token.access_token, lifetime))
    }

    fn exchange(
        &self,
        token_uri: &Url,
        form: &[(&str, &str)],
    ) -> std::result::Result<attohttpc::Response, attohttpc::Error> {
        let body = form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();
        self.session
            .post(token_uri.as_str())
            .header(
                attohttpc::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .text(body)
            .send()
    }
}

/// The RS256 JWT a service account presents for a token (RFC 7523): `iss` is the account,
/// `aud` the token endpoint, and the lifetime the one-hour maximum Google accepts.
fn signed_assertion(client_email: &str, key: &RsaKeyPair, token_uri: &Url) -> Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
    let claims = serde_json::json!({
        "iss": client_email,
        "scope": STORAGE_SCOPE,
        "aud": token_uri.as_str(),
        "iat": now,
        "exp": now + 3600,
    });
    let signing_input = format!("{header}.{}", URL_SAFE_NO_PAD.encode(claims.to_string()));
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        signing_input.as_bytes(),
        &mut signature,
    )
    .map_err(|error| anyhow!("failed to sign the service account assertion: {error}"))?;
    Ok(format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(signature)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{UnparsedPublicKey, RSA_PKCS1_2048_8192_SHA256};
    use serde_json::{json, Value};
    use std::sync::mpsc;
    use std::thread;
    use tiny_http::{Response, Server, StatusCode};

    /// A throwaway 2048-bit RSA key generated for these tests; it has never guarded anything.
    const TEST_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQCtja1IcIITk09X
I8u+IJL+MFrOhqpmqu0/+f2l+UI34fJbG6weanbRmP8rxgKt6z83An/UNvs9Hsv8
kO37zz65ZybfYIjQbKtG2KrYnvztDC6KGzMT5D8Vo8X/PGHqdlC/PHyB8QStLhao
B/sLCKQHVgbfTwX2Mo91xYNOV5d67ndyBX6sEH2dK4qsfrQojTdiNt+Mbh7tel8e
X154c74vRbNfxeOl/BUkTUV+/ZSulPaa7f+Bl5Ypa+LOanCoIImrNxmFR3Rm2r8E
xYS7srzOd2uK7ubZ7J49qkwnj+wxEXkuB5JajsGcjdA3MHJSGMTUtyzl30zZcYpZ
ltR28PATAgMBAAECggEABAciu2ipDC1CDSKuitYUs0OKB/sGH2XeJbr2kRCM4eEj
j8u/neEYqtqjKW/qBgFMsUkkkFdKU2vQjicK7uoraYAkXdDOvA+ZlgZXSLm/4vyC
ZAAnmlGUyTrI0JilKRzf2OBdf2KJmj9UlUfCIsprjCQJ+srJt6IKxE/2MS5FPLwc
TpQp5O1L01wDmyBL34UZXWP/zYDbbpDre0BEofDQQqvfFoa1Euea6MO6Pz+g+wrR
k9FjhGL7A/oilTGmmux7EiqiseVKwx/xkw8wXc9Ol7UuRKenJ4NeJrH3qcwz9N6E
5HMUYGYYc0LF/5ZtOK/x3b4DfjrKgHO8qNantqmLIQKBgQDV+tUsn6hEOqvhVF/p
41X43Om+qpvFvwvNp+NagpMs8BqK0bJMG/uc71VFsuaviafuEUa6C2DHX3tzo8qf
SLk7dCB8Lju8DeGkn7148iBxlNko8lM2QF/Mzlirac/IoOs6uadNTsQfIAzffIUY
1YpM/9z/ybWFAVq3Gmxj9GErkQKBgQDPooiakzEPXXnx3MPnFRVu6WJ4p8/VqOcS
Be1O0ssoeeL+aRUsjxrs87m48U6RNnbGc/JzHJbYY7/d2/vq2/BPnvw2lNFfvwTE
YzhHiasi0/82ACHdgr2EK3hHaHCRy1CHrMPFN880WfZgmEMylnLFpLxYfj2Epblk
6BisixonYwKBgE0/RzPb01gGQkSPPCzoSAIWZj/RwXhq1q0And/v4YfIihxxRUea
loP6FrOZuRPHwxr7m6fQMbjn7F471AsaqtweuRd7bU8fR8Uxcao25qV6UccpbAba
yHrcR0Bia4OHLEQ//3i2JbEC1gLcJgRoGoxflf21aNVCPuh5HoYbE/UxAoGAYm7e
LdGL14nsDrInGTzQjWUM7sA3ckYBTXUL3GtoLociCpQMmrQ5Ao5ExJ15LrRQOKKt
FM8/39PiGK56Tgv4b/aiAMoqPCbVfri+7IMiY393JMWJSHb7m1X0Tn3/p85oHfnU
UYG0tE/V0lM8wBW80V/YfmdSiLj8HA8/60SCoJsCgYBx8MlUZmqsvxX6CiLxwF/7
SBKIBemV340R/1KMUD8RthOneMcN84Bsc76sIPmhMWWI37UA3MvEjABCirW56gYM
kET5mFzRJYtNvAsKzghtmM/ugCywrtTuigL0kr0kbrKXLBJaypTHFRPkrQnbkewm
0qGa8Wl3sSgbrTO7bVXAiA==
-----END PRIVATE KEY-----
";

    /// The test key as `openssl rsa -traditional` would write it: PKCS#1, which service account
    /// files predating PKCS#8 keys carry.
    const TEST_KEY_PKCS1_PEM: &str = "-----BEGIN RSA PRIVATE KEY-----
MIIEogIBAAKCAQEArY2tSHCCE5NPVyPLviCS/jBazoaqZqrtP/n9pflCN+HyWxus
Hmp20Zj/K8YCres/NwJ/1Db7PR7L/JDt+88+uWcm32CI0GyrRtiq2J787Qwuihsz
E+Q/FaPF/zxh6nZQvzx8gfEErS4WqAf7CwikB1YG308F9jKPdcWDTleXeu53cgV+
rBB9nSuKrH60KI03YjbfjG4e7XpfHl9eeHO+L0WzX8XjpfwVJE1Ffv2UrpT2mu3/
gZeWKWvizmpwqCCJqzcZhUd0Ztq/BMWEu7K8zndriu7m2eyePapMJ4/sMRF5LgeS
Wo7BnI3QNzByUhjE1Lcs5d9M2XGKWZbUdvDwEwIDAQABAoIBAAQHIrtoqQwtQg0i
rorWFLNDigf7Bh9l3iW69pEQjOHhI4/Lv53hGKraoylv6gYBTLFJJJBXSlNr0I4n
Cu7qK2mAJF3QzrwPmZYGV0i5v+L8gmQAJ5pRlMk6yNCYpSkc39jgXX9iiZo/VJVH
wiLKa4wkCfrKybeiCsRP9jEuRTy8HE6UKeTtS9NcA5sgS9+FGV1j/82A226Q63tA
RKHw0EKr3xaGtRLnmujDuj8/oPsK0ZPRY4Ri+wP6IpUxpprsexIqorHlSsMf8ZMP
MF3PTpe1LkSnpyeDXiax96nMM/TehORzFGBmGHNCxf+WbTiv8d2+A346yoBzvKjW
p7apiyECgYEA1frVLJ+oRDqr4VRf6eNV+Nzpvqqbxb8LzafjWoKTLPAaitGyTBv7
nO9VRbLmr4mn7hFGugtgx197c6PKn0i5O3QgfC47vA3hpJ+9ePIgcZTZKPJTNkBf
zM5Yq2nPyKDrOrmnTU7EHyAM33yFGNWKTP/c/8m1hQFatxpsY/RhK5ECgYEAz6KI
mpMxD1158dzD5xUVbulieKfP1ajnEgXtTtLLKHni/mkVLI8a7PO5uPFOkTZ2xnPy
cxyW2GO/3dv76tvwT578NpTRX78ExGM4R4mrItP/NgAh3YK9hCt4R2hwkctQh6zD
xTfPNFn2YJhDMpZyxaS8WH49hKW5ZOgYrIsaJ2MCgYBNP0cz29NYBkJEjzws6EgC
FmY/0cF4atatAJ3f7+GHyIoccUVHmpaD+hazmbkTx8Ma+5un0DG45+xeO9QLGqrc
HrkXe21PH0fFMXGqNualelHHKWwG2sh63EdAYmuDhyxEP/94tiWxAtYC3CYEaBqM
X5X9tWjVQj7oeR6GGxP1MQKBgGJu3i3Ri9eJ7A6yJxk80I1lDO7AN3JGAU11C9xr
aC6HIgqUDJq0OQKORMSdeS60UDiirRTPP9/T4hiuek4L+G/2ogDKKjwm1X64vuyD
ImN/dyTFiUh2+5tV9E59/6fOaB351FGBtLRP1dJTPMAVvNFf2H5nUoi4/BwPP+tE
gqCbAoGAcfDJVGZqrL8V+goi8cBf+0gSiAXpld+NEf9SjFA/EbYTp3jHDfOAbHO+
rCD5oTFliN+1ANzLxIwAQoq1ueoGDJBE+Zhc0SWLTbwLCs4IbZjP7oAssK7U7ooC
9JK9JG6ylywSWsqUxxUT5K0J25HsJtKhmvFpd7EoG60zu21VwIg=
-----END RSA PRIVATE KEY-----
";

    fn service_account_json(token_uri: Option<&str>) -> Vec<u8> {
        let mut file = json!({
            "type": "service_account",
            "project_id": "example",
            "private_key_id": "abc123",
            "private_key": TEST_KEY_PEM,
            "client_email": "cache@example.iam.gserviceaccount.com",
            "client_id": "1234567890",
        });
        if let Some(uri) = token_uri {
            file["token_uri"] = json!(uri);
        }
        serde_json::to_vec(&file).unwrap()
    }

    fn authorized_user_json(token_uri: Option<&str>) -> Vec<u8> {
        let mut file = json!({
            "type": "authorized_user",
            "client_id": "client.apps.googleusercontent.com",
            "client_secret": "s3cret",
            "refresh_token": "1//refresh",
        });
        if let Some(uri) = token_uri {
            file["token_uri"] = json!(uri);
        }
        serde_json::to_vec(&file).unwrap()
    }

    struct CapturedRequest {
        method: String,
        url: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl CapturedRequest {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(field, _)| field.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        }

        fn form(&self) -> Vec<(String, String)> {
            form_urlencoded::parse(&self.body).into_owned().collect()
        }
    }

    /// A token endpoint (or metadata server) answering the canned `(status, body)` replies in
    /// order, capturing what it was sent.
    fn token_server(
        responses: Vec<(u16, Vec<u8>)>,
    ) -> (
        String,
        mpsc::Receiver<CapturedRequest>,
        thread::JoinHandle<()>,
    ) {
        let server = Server::http("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", server.server_addr().to_ip().unwrap());
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            for (status, body) in responses {
                let mut request = server.recv().unwrap();
                let headers = request
                    .headers()
                    .iter()
                    .map(|header| (header.field.to_string(), header.value.to_string()))
                    .collect();
                let mut received = Vec::new();
                request.as_reader().read_to_end(&mut received).unwrap();
                sender
                    .send(CapturedRequest {
                        method: request.method().to_string(),
                        url: request.url().to_owned(),
                        headers,
                        body: received,
                    })
                    .unwrap();
                request
                    .respond(Response::from_data(body).with_status_code(StatusCode(status)))
                    .unwrap();
            }
        });
        (endpoint, receiver, handle)
    }

    fn token_json(token: &str, expires_in: Option<u64>) -> Vec<u8> {
        let mut body = json!({ "access_token": token, "token_type": "Bearer" });
        if let Some(seconds) = expires_in {
            body["expires_in"] = json!(seconds);
        }
        serde_json::to_vec(&body).unwrap()
    }

    #[test]
    fn parses_service_account_and_authorized_user_files() {
        let Credentials::ServiceAccount {
            client_email,
            token_uri,
            ..
        } = Credentials::from_json(&service_account_json(None)).unwrap()
        else {
            panic!("expected a service account");
        };
        assert_eq!(client_email, "cache@example.iam.gserviceaccount.com");
        assert_eq!(token_uri.as_str(), DEFAULT_TOKEN_URI);

        let pkcs1 = serde_json::to_vec(&json!({
            "type": "service_account",
            "client_email": "legacy@example.iam.gserviceaccount.com",
            "private_key": TEST_KEY_PKCS1_PEM,
            "token_uri": "https://oauth2.example.com/token",
        }))
        .unwrap();
        assert!(matches!(
            Credentials::from_json(&pkcs1).unwrap(),
            Credentials::ServiceAccount { token_uri, .. } if token_uri.as_str() == "https://oauth2.example.com/token"
        ));

        let Credentials::AuthorizedUser {
            client_id,
            client_secret,
            refresh_token,
            token_uri,
        } = Credentials::from_json(&authorized_user_json(Some("http://localhost:1/token")))
            .unwrap()
        else {
            panic!("expected user credentials");
        };
        assert_eq!(client_id, "client.apps.googleusercontent.com");
        assert_eq!(client_secret, "s3cret");
        assert_eq!(refresh_token, "1//refresh");
        assert_eq!(token_uri.as_str(), "http://localhost:1/token");
    }

    #[test]
    fn rejects_unsupported_and_incomplete_files() {
        let error = |json: Value| {
            Credentials::from_json(&serde_json::to_vec(&json).unwrap())
                .unwrap_err()
                .to_string()
        };
        assert!(
            error(json!({ "type": "external_account" })).contains("unsupported credential type")
        );
        assert!(
            error(json!({ "type": "service_account", "private_key": TEST_KEY_PEM }))
                .contains("missing \"client_email\"")
        );
        assert!(error(
            json!({ "type": "service_account", "client_email": "a@b", "private_key": "" })
        )
        .contains("missing \"private_key\""));
        assert!(error(json!({
            "type": "service_account", "client_email": "a@b", "private_key": "not a key"
        }))
        .contains("not a PEM private key"));
        assert!(error(json!({
            "type": "service_account", "client_email": "a@b",
            "private_key": "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n"
        }))
        .contains("not a usable RSA key"));
        assert!(error(json!({
            "type": "service_account", "client_email": "a@b", "private_key": TEST_KEY_PEM,
            "token_uri": "not a url"
        }))
        .contains("invalid token_uri"));
        assert!(
            error(json!({ "type": "authorized_user", "client_id": "id" }))
                .contains("missing \"client_secret\"")
        );
        assert!(Credentials::from_json(b"[]")
            .unwrap_err()
            .to_string()
            .contains("not a JSON credentials file"));
    }

    #[test]
    fn resolves_application_default_credentials_in_order() {
        let home = tempfile::tempdir().unwrap();
        let explicit = home.path().join("key.json");
        fs::write(&explicit, service_account_json(None)).unwrap();
        let gcloud = home.path().join(".config").join("gcloud");
        fs::create_dir_all(&gcloud).unwrap();
        fs::write(gcloud.join(WELL_KNOWN_FILE), authorized_user_json(None)).unwrap();
        let custom = home.path().join("cloudsdk");
        fs::create_dir_all(&custom).unwrap();
        fs::write(
            custom.join(WELL_KNOWN_FILE),
            authorized_user_json(Some("http://localhost:1/custom")),
        )
        .unwrap();

        // 1. GOOGLE_APPLICATION_CREDENTIALS wins, and a broken one is an error, not a fallthrough.
        let environment = Environment {
            credentials_file: Some(explicit),
            cloudsdk_config: Some(custom.clone()),
            config_home: Some(home.path().join(".config")),
            metadata_host: None,
        };
        assert!(matches!(
            Credentials::resolve(&environment).unwrap(),
            Credentials::ServiceAccount { .. }
        ));
        let missing = Environment {
            credentials_file: Some(home.path().join("absent.json")),
            ..environment.clone()
        };
        let error = format!("{:#}", Credentials::resolve(&missing).unwrap_err());
        assert!(error.contains(CREDENTIALS_FILE_ENV), "{error}");
        assert!(error.contains("failed to read credentials file"), "{error}");

        // 2. gcloud's well-known file, under CLOUDSDK_CONFIG when set, else the config home.
        let well_known = Environment {
            credentials_file: None,
            ..environment.clone()
        };
        assert!(matches!(
            Credentials::resolve(&well_known).unwrap(),
            Credentials::AuthorizedUser { token_uri, .. } if token_uri.as_str() == "http://localhost:1/custom"
        ));
        let default_home = Environment {
            cloudsdk_config: None,
            ..well_known.clone()
        };
        assert!(matches!(
            Credentials::resolve(&default_home).unwrap(),
            Credentials::AuthorizedUser { token_uri, .. } if token_uri.as_str() == DEFAULT_TOKEN_URI
        ));

        // 3. Otherwise the metadata server, at GCE_METADATA_HOST when set.
        let nowhere = Environment {
            cloudsdk_config: Some(home.path().join("empty")),
            ..well_known.clone()
        };
        let Credentials::Metadata { token_url } = Credentials::resolve(&nowhere).unwrap() else {
            panic!("expected the metadata server");
        };
        assert_eq!(
            token_url.as_str(),
            "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token"
        );
        let overridden = Environment {
            metadata_host: Some("169.254.169.254:8080".to_owned()),
            ..nowhere.clone()
        };
        let Credentials::Metadata { token_url } = Credentials::resolve(&overridden).unwrap() else {
            panic!("expected the metadata server");
        };
        assert_eq!(token_url.host_str(), Some("169.254.169.254"));
        assert_eq!(token_url.port(), Some(8080));
        let invalid = Environment {
            metadata_host: Some("host/with/path".to_owned()),
            ..nowhere.clone()
        };
        assert!(Credentials::resolve(&invalid)
            .unwrap_err()
            .to_string()
            .contains(METADATA_HOST_ENV));
        assert!(Credentials::resolve(&Environment::default()).is_ok());
    }

    #[test]
    fn service_account_exchanges_a_signed_assertion_and_caches_the_token() {
        let (endpoint, requests, handle) = token_server(vec![
            (200, token_json("first", Some(3600))),
            (200, token_json("second", Some(0))),
            (200, token_json("third", None)),
        ]);
        let token_uri = format!("{endpoint}/token");
        let credentials = Credentials::from_json(&service_account_json(Some(&token_uri))).unwrap();
        let Credentials::ServiceAccount { key, .. } = &credentials else {
            panic!("expected a service account");
        };
        let public_key = key.public().as_ref().to_vec();
        let tokens = TokenSource::new(credentials);

        // A fresh token is cached for the hour the endpoint promised.
        assert_eq!(tokens.token().unwrap(), "first");
        assert_eq!(tokens.token().unwrap(), "first");
        let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "/token");
        assert_eq!(
            request.header("Content-Type"),
            Some("application/x-www-form-urlencoded")
        );
        let form = request.form();
        assert_eq!(
            form[0],
            (
                "grant_type".to_owned(),
                "urn:ietf:params:oauth:grant-type:jwt-bearer".to_owned()
            )
        );
        let (name, assertion) = &form[1];
        assert_eq!(name, "assertion");
        let [header, claims, signature] = assertion.split('.').collect::<Vec<_>>()[..] else {
            panic!("expected a three-part JWT: {assertion}");
        };
        assert_eq!(
            URL_SAFE_NO_PAD.decode(header).unwrap(),
            br#"{"alg":"RS256","typ":"JWT"}"#
        );
        let claims: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(claims).unwrap()).unwrap();
        assert_eq!(claims["iss"], "cache@example.iam.gserviceaccount.com");
        assert_eq!(claims["scope"], STORAGE_SCOPE);
        assert_eq!(claims["aud"], token_uri);
        assert_eq!(
            claims["exp"].as_u64().unwrap() - claims["iat"].as_u64().unwrap(),
            3600
        );
        let signed = assertion.rsplit_once('.').unwrap().0;
        UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, &public_key)
            .verify(
                signed.as_bytes(),
                &URL_SAFE_NO_PAD.decode(signature).unwrap(),
            )
            .expect("assertion signed by the service account key");

        // The cache is bypassed by a token inside the refresh margin; a missing lifetime is an hour.
        let mut cached = tokens.cached.lock().unwrap();
        cached.as_mut().unwrap().expires_at = Instant::now() + REFRESH_MARGIN / 2;
        drop(cached);
        assert_eq!(tokens.token().unwrap(), "second");
        assert_eq!(tokens.token().unwrap(), "third");
        assert_eq!(tokens.token().unwrap(), "third");
        let lifetime = tokens.cached.lock().unwrap().as_ref().unwrap().expires_at - Instant::now();
        assert!(lifetime > Duration::from_secs(3500) && lifetime <= Duration::from_secs(3600));
        handle.join().unwrap();
        assert_eq!(requests.try_iter().count(), 2);
    }

    #[test]
    fn authorized_user_exchanges_the_refresh_token() {
        let (endpoint, requests, handle) =
            token_server(vec![(200, token_json("user-token", Some(3599)))]);
        let credentials = Credentials::from_json(&authorized_user_json(Some(&format!(
            "{endpoint}/oauth/token"
        ))))
        .unwrap();
        let tokens = TokenSource::new(credentials);
        assert_eq!(tokens.token().unwrap(), "user-token");
        handle.join().unwrap();
        let request = requests.recv().unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "/oauth/token");
        assert_eq!(
            request.form(),
            [
                ("grant_type", "refresh_token"),
                ("client_id", "client.apps.googleusercontent.com"),
                ("client_secret", "s3cret"),
                ("refresh_token", "1//refresh"),
            ]
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
        );
    }

    #[test]
    fn metadata_server_is_asked_with_the_flavor_header() {
        let (endpoint, requests, handle) =
            token_server(vec![(200, token_json("metadata-token", Some(1800)))]);
        let tokens = TokenSource::new(Credentials::Metadata {
            token_url: Url::parse(&format!("{endpoint}{METADATA_TOKEN_PATH}")).unwrap(),
        });
        assert_eq!(tokens.token().unwrap(), "metadata-token");
        handle.join().unwrap();
        let request = requests.recv().unwrap();
        assert_eq!(request.method, "GET");
        assert_eq!(request.header("Metadata-Flavor"), Some("Google"));
        let url = Url::parse(&format!("http://localhost{}", request.url)).unwrap();
        assert_eq!(url.path(), METADATA_TOKEN_PATH);
        assert_eq!(
            url.query_pairs().next().unwrap(),
            ("scopes".into(), STORAGE_SCOPE.into())
        );
    }

    #[test]
    fn token_failures_are_reported_and_not_cached() {
        let (endpoint, requests, handle) = token_server(vec![
            (401, "x".repeat(1000).into_bytes()),
            (200, b"not json".to_vec()),
            (200, token_json("late", Some(60))),
        ]);
        let tokens = TokenSource::new(Credentials::Metadata {
            token_url: Url::parse(&format!("{endpoint}{METADATA_TOKEN_PATH}")).unwrap(),
        });
        let error = tokens.token().unwrap_err().to_string();
        assert!(
            error.starts_with("token endpoint answered HTTP 401 with content '"),
            "{error}"
        );
        assert!(
            error.contains(&format!("{}...'", "x".repeat(ERROR_BODY_LIMIT))),
            "{error}"
        );
        assert!(tokens
            .token()
            .unwrap_err()
            .to_string()
            .contains("unexpected JSON"));
        assert_eq!(tokens.token().unwrap(), "late");
        handle.join().unwrap();
        assert_eq!(requests.try_iter().count(), 3);

        let unreachable = TokenSource::new(Credentials::Metadata {
            token_url: Url::parse("http://127.0.0.1:9/token").unwrap(),
        });
        assert!(format!("{:#}", unreachable.token().unwrap_err()).contains("token request failed"));
    }
}
