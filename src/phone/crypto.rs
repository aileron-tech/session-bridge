//! The phone listener's certificate and phone signatures (REQ-036), with
//! rustls' ring provider. The certificate is a self-signed P-256 leaf for
//! `sb-<label>.local` that cannot sign other certificates (`CA:false`); the
//! phone trusts it once.
use crate::store;
use anyhow::{Context, Result};
use data_encoding::BASE64;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};

/// iOS refuses TLS server certificates valid longer than 825 days.
const VALID_DAYS: i64 = 800;
/// A certificate this close to its end is replaced.
const RENEW_DAYS: i64 = 30;

#[derive(Clone, Serialize, Deserialize)]
pub struct Identity {
    pub host: String,
    /// Unix seconds.
    pub not_after: i64,
    cert: String,
    key: String,
}
impl Identity {
    pub fn cert_der(&self) -> Result<Vec<u8>> {
        Ok(BASE64.decode(self.cert.as_bytes())?)
    }
    fn key_der(&self) -> Result<Vec<u8>> {
        Ok(BASE64.decode(self.key.as_bytes())?)
    }
}

/// Year, month, day of a day count since 1970-01-01 (proleptic Gregorian).
fn civil(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
    (year, month, day)
}

fn generate(host: &str, now: i64) -> Result<Identity> {
    use rcgen::{
        CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
        PKCS_ECDSA_P256_SHA256, SerialNumber,
    };
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
    let mut params = CertificateParams::new(vec![host.to_owned()])?;
    params
        .distinguished_name
        .push(DnType::CommonName, host.to_owned());
    params
        .distinguished_name
        .push(DnType::OrganizationName, "session-bridge phone app");
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let today = now.div_euclid(86_400);
    let (y, m, d) = civil(today - 1);
    params.not_before = rcgen::date_time_ymd(y, m, d);
    let (y, m, d) = civil(today + VALID_DAYS);
    params.not_after = rcgen::date_time_ymd(y, m, d);
    let mut serial = [0u8; 16];
    getrandom::fill(&mut serial).map_err(|e| anyhow::anyhow!("No system randomness: {e}"))?;
    serial[0] &= 0x7f;
    params.serial_number = Some(SerialNumber::from_slice(&serial));
    let cert = params.self_signed(&key)?;
    Ok(Identity {
        host: host.to_owned(),
        not_after: (today + VALID_DAYS) * 86_400,
        cert: BASE64.encode(cert.der()),
        key: BASE64.encode(&key.serialize_der()),
    })
}

/// The saved identity for `host`, or a new one when there is none, the host
/// name changed, or it is about to expire (the phone must then trust it again).
pub fn load_or_create(file: &Path, host: &str, now: i64) -> Result<Identity> {
    if file.exists() {
        let identity: Identity = store::read_json(file)?;
        if identity.host == host && identity.not_after - now > RENEW_DAYS * 86_400 {
            return Ok(identity);
        }
    }
    let identity = generate(host, now)?;
    store::write_json(file, &identity)?;
    Ok(identity)
}

/// TLS 1.3 only, HTTP/1.1, no client certificates.
pub fn server_config(identity: &Identity) -> Result<Arc<rustls::ServerConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(identity.cert_der()?)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key_der()?)),
        )
        .context("Phone certificate rejected")?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// A WebCrypto ECDSA P-256 / SHA-256 signature (raw `r||s`, IEEE P1363) by
/// the phone key `public_key` (uncompressed point) over `message`.
pub fn verify(public_key: &[u8], message: &[u8], signature: &[u8]) -> bool {
    ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, public_key)
        .verify(message, signature)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::{
        rand::SystemRandom,
        signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair},
    };

    /* REQ-036: Day counts convert to calendar dates for certificate validity. */
    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_454), (2026, 1, 1));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }

    /* REQ-036: The phone certificate is a P-256 leaf for its host, valid at most 825 days, kept across restarts and replaced when the host changes. */
    #[test]
    fn certificate_is_kept_per_host() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("phone-identity.json");
        let now = 1_795_000_000;
        let first = load_or_create(&file, "sb-a.local", now).unwrap();
        assert!(first.not_after - now <= 825 * 86_400);
        let again = load_or_create(&file, "sb-a.local", now + 10).unwrap();
        assert_eq!(first.cert_der().unwrap(), again.cert_der().unwrap());
        let renamed = load_or_create(&file, "sb-b.local", now + 20).unwrap();
        assert_ne!(first.cert_der().unwrap(), renamed.cert_der().unwrap());
        let renewed = load_or_create(&file, "sb-b.local", renamed.not_after - 86_400).unwrap();
        assert_ne!(renamed.cert_der().unwrap(), renewed.cert_der().unwrap());
        assert!(server_config(&renewed).is_ok());
    }

    /* REQ-036: Phone signatures verify in WebCrypto's raw format and fail for another message, key or DER encoding. */
    #[test]
    fn signatures_verify_in_raw_format_only() {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let public = pair.public_key().as_ref().to_vec();
        assert_eq!(public.len(), 65);
        let message = crate::phone::model::session_message("sb-a.local", "nonce");
        let signature = pair.sign(&rng, &message).unwrap();
        assert_eq!(signature.as_ref().len(), 64);
        assert!(verify(&public, &message, signature.as_ref()));
        let other = crate::phone::model::session_message("sb-b.local", "nonce");
        assert!(!verify(&public, &other, signature.as_ref()));
        let der_pair = EcdsaKeyPair::from_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING,
            pkcs8.as_ref(),
            &rng,
        )
        .unwrap();
        let der = der_pair.sign(&rng, &message).unwrap();
        assert!(!verify(&public, &message, der.as_ref()));
        let mut wrong = public.clone();
        wrong[10] ^= 1;
        assert!(!verify(&wrong, &message, signature.as_ref()));
    }
}
