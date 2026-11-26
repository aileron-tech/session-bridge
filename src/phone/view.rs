//! What the admin page shows about phone access (REQ-036). Pure, apart from
//! drawing QR codes.
use super::model::{
    MAX_DEVICES, Offer, OfferState, PhoneBook, hex_fingerprint, short_hex_fingerprint,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhonesView {
    pub enabled: bool,
    pub listening: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    /// `sb-<label>.local`
    pub host: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// SHA-256 of the certificate, as iOS shows it; empty before the first start.
    pub fingerprint: String,
    pub fingerprint_short: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_qr: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offer: Option<OfferView>,
    pub devices: Vec<DeviceView>,
    pub max_devices: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferView {
    pub id: Uuid,
    pub code: String,
    pub url: String,
    pub qr: Vec<String>,
    pub expires_in: u64,
    pub state: OfferStateView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum OfferStateView {
    Waiting,
    Scanned { device_name: String, key: String },
    Paired { device_id: Uuid },
    Declined,
    Expired,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceView {
    pub id: Uuid,
    pub name: String,
    pub key: String,
    pub paired_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_address: Option<String>,
    pub sessions: usize,
}

/// The listener's state as the service sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Runtime {
    pub listening: bool,
    pub problem: Option<String>,
    pub address: Option<String>,
    pub cert_der: Option<Vec<u8>>,
}

/// QR modules, one string per row, `1` dark; no quiet zone.
pub fn qr_rows(text: &str) -> Vec<String> {
    match qrcode::QrCode::with_error_correction_level(text, qrcode::EcLevel::M) {
        Ok(code) => {
            let width = code.width();
            code.to_colors()
                .chunks(width)
                .map(|row| {
                    row.iter()
                        .map(|c| if *c == qrcode::Color::Dark { '1' } else { '0' })
                        .collect()
                })
                .collect()
        }
        Err(e) => {
            eprintln!("QR code not drawn: {e}");
            Vec::new()
        }
    }
}

/// The pairing URL: the code in the fragment, so it is never sent in a
/// request line or logged.
pub fn pair_url(host: &str, port: u16, code: &str, cert_der: &[u8]) -> String {
    let fp: String = hex_fingerprint(cert_der).replace(' ', "");
    format!("https://{host}:{port}/#pair={code}&fp={fp}")
}

pub fn profile_url(address: &str, port: u16) -> String {
    format!("http://{address}:{}/bridge.mobileconfig", port + 1)
}

pub struct ViewInput<'a> {
    pub book: &'a PhoneBook,
    pub offer: Option<&'a Offer>,
    pub runtime: &'a Runtime,
    pub host: &'a str,
    /// Open phone sessions per device.
    pub sessions: &'a dyn Fn(Uuid) -> usize,
    pub now: u64,
}

pub fn phones_view(input: ViewInput<'_>) -> PhonesView {
    let port = input.book.port();
    let cert = input.runtime.cert_der.as_deref();
    let profile_url = match (&input.runtime.address, input.runtime.listening) {
        (Some(address), true) => Some(profile_url(address, port)),
        _ => None,
    };
    let offer = input.offer.map(|offer| {
        let offer = offer.at(input.now);
        let code = offer.code.display();
        let url = cert.map_or_else(String::new, |cert| {
            pair_url(input.host, port, offer.code.as_str(), cert)
        });
        OfferView {
            id: offer.id,
            qr: if url.is_empty() {
                Vec::new()
            } else {
                qr_rows(&url)
            },
            url,
            code,
            expires_in: offer.expires_in(input.now),
            state: match &offer.state {
                OfferState::Waiting => OfferStateView::Waiting,
                OfferState::Scanned(candidate) => OfferStateView::Scanned {
                    device_name: candidate.name.clone(),
                    key: candidate.key.short(),
                },
                OfferState::Paired { device_id, .. } => OfferStateView::Paired {
                    device_id: *device_id,
                },
                OfferState::Declined { .. } => OfferStateView::Declined,
                OfferState::Expired => OfferStateView::Expired,
                OfferState::Failed => OfferStateView::Failed,
            },
        }
    });
    PhonesView {
        enabled: input.book.enabled,
        listening: input.runtime.listening,
        problem: input.runtime.problem.clone(),
        host: input.host.to_owned(),
        port,
        address: input.runtime.address.clone(),
        fingerprint: cert.map(hex_fingerprint).unwrap_or_default(),
        fingerprint_short: cert.map(short_hex_fingerprint).unwrap_or_default(),
        profile_qr: profile_url.as_deref().map(qr_rows),
        profile_url,
        offer,
        devices: input
            .book
            .devices
            .iter()
            .map(|d| DeviceView {
                id: d.id,
                name: d.name.clone(),
                key: d.key.short(),
                paired_at: d.paired_at,
                last_seen: d.last_seen,
                last_address: d.last_address.clone(),
                sessions: (input.sessions)(d.id),
            })
            .collect(),
        max_devices: MAX_DEVICES,
    }
}
