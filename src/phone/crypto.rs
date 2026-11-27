//! The phone listener's certificates and phone signatures (REQ-036), with
//! rustls' ring provider. Each bridge has its own P-256 root, valid 10 years
//! and name-constrained to private IPv4 ranges and its `sb-<label>.local`
//! name: the phone trusts it once (that is the profile). The server
//! certificate is issued by it for the current LAN addresses and issued again
//! when they change, so a new DHCP address needs no new profile.
use crate::store;
use anyhow::{Context, Result, anyhow};
use data_encoding::BASE64;
use rcgen::{
    BasicConstraints, CertificateParams, CidrSubnet, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, GeneralSubtree, IsCa, Issuer, KeyPair, KeyUsagePurpose,
    NameConstraints, PKCS_ECDSA_P256_SHA256, SanType, SerialNumber,
};
use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    server::{ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
};
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, Ipv4Addr},
    path::Path,
    sync::{Arc, RwLock},
};

/// The root: long-lived, so the phone trusts it once.
const ROOT_DAYS: i64 = 3650;
/// The server certificate: within Apple's limits for TLS server certificates.
const LEAF_DAYS: i64 = 397;
/// A certificate this close to its end is replaced.
const RENEW_DAYS: i64 = 30;
/// The address ranges the root may vouch for.
const PRIVATE_RANGES: [([u8; 4], u8); 4] = [
    ([10, 0, 0, 0], 8),
    ([172, 16, 0, 0], 12),
    ([192, 168, 0, 0], 16),
    ([169, 254, 0, 0], 16),
];

/// The bridge's root for phones, kept in `phone-ca.json` (mode 0600).
#[derive(Clone, Serialize, Deserialize)]
pub struct Authority {
    /// `sb-<label>.local`: the name in its constraints.
    pub host: String,
    /// Unix seconds.
    pub not_after: i64,
    cert: String,
    key: String,
}
impl Authority {
    pub fn cert_der(&self) -> Result<Vec<u8>> {
        Ok(BASE64.decode(self.cert.as_bytes())?)
    }
    fn key_pair(&self) -> Result<KeyPair> {
        Ok(KeyPair::try_from(BASE64.decode(self.key.as_bytes())?)?)
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
fn validity(params: &mut CertificateParams, now: i64, days: i64) {
    let today = now.div_euclid(86_400);
    let (y, m, d) = civil(today - 1);
    params.not_before = rcgen::date_time_ymd(y, m, d);
    let (y, m, d) = civil(today + days);
    params.not_after = rcgen::date_time_ymd(y, m, d);
}
fn serial() -> Result<SerialNumber> {
    let mut serial = [0u8; 16];
    getrandom::fill(&mut serial).map_err(|e| anyhow!("No system randomness: {e}"))?;
    serial[0] &= 0x7f;
    Ok(SerialNumber::from_slice(&serial))
}
/// The root's name, the same when it is created and when it issues.
fn root_name(host: &str) -> DistinguishedName {
    let mut name = DistinguishedName::new();
    name.push(
        DnType::CommonName,
        format!("session-bridge phone CA ({host})"),
    );
    name.push(DnType::OrganizationName, "session-bridge phone app");
    name
}
fn root_params(host: &str) -> CertificateParams {
    let mut params = CertificateParams::default();
    params.distinguished_name = root_name(host);
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params
}

fn generate_root(host: &str, now: i64) -> Result<Authority> {
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
    let mut params = root_params(host);
    let mut permitted: Vec<GeneralSubtree> = PRIVATE_RANGES
        .iter()
        .map(|(addr, prefix)| GeneralSubtree::IpAddress(CidrSubnet::from_v4_prefix(*addr, *prefix)))
        .collect();
    permitted.push(GeneralSubtree::DnsName(host.to_owned()));
    params.name_constraints = Some(NameConstraints {
        permitted_subtrees: permitted,
        excluded_subtrees: Vec::new(),
    });
    validity(&mut params, now, ROOT_DAYS);
    params.serial_number = Some(serial()?);
    let cert = params.self_signed(&key)?;
    Ok(Authority {
        host: host.to_owned(),
        not_after: (now.div_euclid(86_400) + ROOT_DAYS) * 86_400,
        cert: BASE64.encode(cert.der()),
        key: BASE64.encode(&key.serialize_der()),
    })
}

/// The saved root for `host`, or a new one when there is none, the host name
/// changed, or it is about to expire (the phone must then trust it again).
pub fn load_or_create(file: &Path, host: &str, now: i64) -> Result<Authority> {
    if file.exists() {
        let authority: Authority = store::read_json(file)?;
        if authority.host == host && authority.not_after - now > RENEW_DAYS * 86_400 {
            return Ok(authority);
        }
    }
    let authority = generate_root(host, now)?;
    store::write_json(file, &authority)?;
    Ok(authority)
}

/// A server certificate and its key, issued by the root.
pub struct Leaf {
    pub addresses: Vec<Ipv4Addr>,
    pub cert_der: Vec<u8>,
    key_der: Vec<u8>,
    pub not_after: i64,
}
/// A server certificate for `addresses` and the root's `.local` name.
pub fn issue_leaf(authority: &Authority, addresses: &[Ipv4Addr], now: i64) -> Result<Leaf> {
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, authority.host.clone());
    params.subject_alt_names = addresses
        .iter()
        .map(|ip| SanType::IpAddress(IpAddr::V4(*ip)))
        .collect();
    params
        .subject_alt_names
        .push(SanType::DnsName(authority.host.clone().try_into()?));
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.use_authority_key_identifier_extension = true;
    validity(&mut params, now, LEAF_DAYS);
    params.serial_number = Some(serial()?);
    let issuer = Issuer::new(root_params(&authority.host), authority.key_pair()?);
    let cert = params.signed_by(&key, &issuer)?;
    Ok(Leaf {
        addresses: addresses.to_vec(),
        cert_der: cert.der().to_vec(),
        key_der: key.serialize_der(),
        not_after: (now.div_euclid(86_400) + LEAF_DAYS) * 86_400,
    })
}
/// The leaf is for other addresses, or near its end: issue a new one.
pub fn leaf_stale(leaf: &Leaf, addresses: &[Ipv4Addr], now: i64) -> bool {
    leaf.addresses != addresses || leaf.not_after - now <= RENEW_DAYS * 86_400
}

/// The server certificate in use, replaced while the listener runs.
#[derive(Debug)]
pub struct CurrentCert(RwLock<Arc<CertifiedKey>>);
fn certified(leaf: &Leaf) -> Result<Arc<CertifiedKey>> {
    let key = rustls::crypto::ring::sign::any_ecdsa_type(&PrivateKeyDer::Pkcs8(
        PrivatePkcs8KeyDer::from(leaf.key_der.clone()),
    ))
    .context("Phone certificate key rejected")?;
    Ok(Arc::new(CertifiedKey::new(
        vec![CertificateDer::from(leaf.cert_der.clone())],
        key,
    )))
}
impl CurrentCert {
    pub fn new(leaf: &Leaf) -> Result<Arc<Self>> {
        Ok(Arc::new(Self(RwLock::new(certified(leaf)?))))
    }
    pub fn replace(&self, leaf: &Leaf) -> Result<()> {
        let next = certified(leaf)?;
        *self.0.write().unwrap_or_else(|p| p.into_inner()) = next;
        Ok(())
    }
}
impl ResolvesServerCert for CurrentCert {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.0.read().unwrap_or_else(|p| p.into_inner()).clone())
    }
}

/// TLS 1.3 only, HTTP/1.1, no client certificates, the current leaf.
pub fn server_config(current: Arc<CurrentCert>) -> Result<Arc<rustls::ServerConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_cert_resolver(current);
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
        signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair as _},
    };
    use rustls::{client::WebPkiServerVerifier, pki_types::ServerName};

    const NOW: i64 = 1_795_000_000;

    /* REQ-036: Day counts convert to calendar dates for certificate validity. */
    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_454), (2026, 1, 1));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }

    /// Whether a client that trusts `root` accepts `leaf` for `name`
    /// (webpki: chain, validity, name constraints, SAN).
    fn accepted(root: &[u8], leaf: &[u8], name: &str) -> bool {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(root.to_vec())).unwrap();
        let verifier = WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            Arc::new(rustls::crypto::ring::default_provider()),
        )
        .build()
        .unwrap();
        use rustls::client::danger::ServerCertVerifier;
        verifier
            .verify_server_cert(
                &CertificateDer::from(leaf.to_vec()),
                &[],
                &ServerName::try_from(name.to_owned()).unwrap(),
                &[],
                rustls::pki_types::UnixTime::since_unix_epoch(std::time::Duration::from_secs(
                    NOW as u64,
                )),
            )
            .is_ok()
    }

    /* REQ-036: The phone root is kept across restarts and replaced when the host name changes or it nears its end. */
    #[test]
    fn root_is_kept_per_host() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("phone-ca.json");
        let first = load_or_create(&file, "sb-a.local", NOW).unwrap();
        let again = load_or_create(&file, "sb-a.local", NOW + 10).unwrap();
        assert_eq!(first.cert_der().unwrap(), again.cert_der().unwrap());
        let renamed = load_or_create(&file, "sb-b.local", NOW + 20).unwrap();
        assert_ne!(first.cert_der().unwrap(), renamed.cert_der().unwrap());
        let renewed = load_or_create(&file, "sb-b.local", renamed.not_after - 86_400).unwrap();
        assert_ne!(renamed.cert_der().unwrap(), renewed.cert_der().unwrap());
    }

    /* REQ-036: A client trusting the root accepts the server certificate for each LAN address and the .local name; a new address gets a new server certificate from the same root. */
    #[test]
    fn leaf_follows_the_addresses_under_one_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = load_or_create(&dir.path().join("ca.json"), "sb-a.local", NOW).unwrap();
        let root_der = root.cert_der().unwrap();
        let first = [Ipv4Addr::new(192, 168, 1, 123), Ipv4Addr::new(10, 0, 0, 5)];
        let leaf = issue_leaf(&root, &first, NOW).unwrap();
        assert!(accepted(&root_der, &leaf.cert_der, "192.168.1.123"));
        assert!(accepted(&root_der, &leaf.cert_der, "10.0.0.5"));
        assert!(accepted(&root_der, &leaf.cert_der, "sb-a.local"));
        assert!(!accepted(&root_der, &leaf.cert_der, "192.168.1.124"));
        assert!(!leaf_stale(&leaf, &first, NOW));
        let moved = [Ipv4Addr::new(192, 168, 1, 77)];
        assert!(leaf_stale(&leaf, &moved, NOW));
        let next = issue_leaf(&root, &moved, NOW).unwrap();
        assert!(accepted(&root_der, &next.cert_der, "192.168.1.77"));
        assert!(!accepted(&root_der, &next.cert_der, "192.168.1.123"));
        assert!(leaf_stale(&next, &moved, next.not_after - 86_400));
        assert!(CurrentCert::new(&next).is_ok());
    }

    /* REQ-036: The root cannot vouch for a public address, even in a certificate it signed itself. */
    #[test]
    fn root_is_constrained_to_private_addresses_and_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = load_or_create(&dir.path().join("ca.json"), "sb-a.local", NOW).unwrap();
        let root_der = root.cert_der().unwrap();
        let public = issue_leaf(&root, &[Ipv4Addr::new(8, 8, 8, 8)], NOW).unwrap();
        assert!(!accepted(&root_der, &public.cert_der, "8.8.8.8"));
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
        let message = crate::phone::model::session_message("192.168.1.123", "nonce");
        let signature = pair.sign(&rng, &message).unwrap();
        assert_eq!(signature.as_ref().len(), 64);
        assert!(verify(&public, &message, signature.as_ref()));
        let other = crate::phone::model::session_message("192.168.1.124", "nonce");
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
