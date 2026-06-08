//! Self-signed dev certificate for the WebTransport listener.
//!
//! Mirrors the cert lifecycle of a proven internal reference project: a fresh
//! ECDSA P-256 cert generated at startup, held in memory (no disk persistence),
//! with a 10-day validity (a WebTransport `serverCertificateHashes` requirement
//! — pins must be ≤ 14 days). The browser fetches the `base64url(SHA-256(DER))`
//! hash from the `/control` endpoint and pins it. A dev-tool restart simply
//! mints a new cert; the editor re-fetches the hash on its next connect.

use anyhow::{Context, Result};
use base64::Engine;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_ECDSA_P256_SHA256};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

/// A generated self-signed certificate + its private key (DER).
pub struct GeneratedCert {
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
}

impl GeneratedCert {
    /// Generate a fresh P-256 self-signed cert for `hostname` (10-day validity).
    pub fn new(hostname: &str) -> Result<Self> {
        let key_pair =
            KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).context("generate P-256 key pair")?;

        let mut dname = DistinguishedName::new();
        dname.push(DnType::CommonName, "awsm-mcp self-signed");

        let mut params =
            CertificateParams::new(vec![hostname.to_string()]).context("certificate params")?;
        params.distinguished_name = dname;

        let now = time::OffsetDateTime::now_utc();
        params.not_before = now;
        params.not_after = now + time::Duration::days(10);

        let cert = params
            .self_signed(&key_pair)
            .context("self-sign certificate")?;

        Ok(Self {
            cert_der: cert.der().to_vec(),
            key_der: key_pair.serialize_der(),
        })
    }

    /// `base64url(SHA-256(DER))` — the value the browser pins via
    /// `serverCertificateHashes`.
    pub fn hash_base64url(&self) -> String {
        let digest = Sha256::digest(&self.cert_der);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
    }

    pub fn rustls_cert(&self) -> CertificateDer<'static> {
        CertificateDer::from(self.cert_der.clone())
    }

    pub fn rustls_key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::from(PrivatePkcs8KeyDer::from(self.key_der.clone()))
    }
}
