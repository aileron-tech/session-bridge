//! The iOS configuration profile that carries the bridge's phone root
//! certificate (REQ-036). Pure. The profile is unsigned; the user compares
//! the certificate fingerprint iOS shows with the one on the admin page.
use data_encoding::BASE64;
use uuid::Uuid;

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A `.mobileconfig` with one certificate payload for `host`. The UUIDs are
/// derived from the certificate, so the same certificate gives the same
/// profile and installing it again replaces it.
pub fn mobileconfig(cert_der: &[u8], host: &str) -> String {
    let profile = Uuid::new_v5(&Uuid::NAMESPACE_OID, cert_der);
    let payload = Uuid::new_v5(&profile, b"certificate");
    let host = escape(host);
    let data = BASE64.encode(cert_der);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>PayloadContent</key>
  <array>
    <dict>
      <key>PayloadCertificateFileName</key>
      <string>{host}.cer</string>
      <key>PayloadContent</key>
      <data>{data}</data>
      <key>PayloadDescription</key>
      <string>Lets this phone trust the session-bridge phone app on {host}.</string>
      <key>PayloadDisplayName</key>
      <string>{host}</string>
      <key>PayloadIdentifier</key>
      <string>local.session-bridge.phone.{payload}</string>
      <key>PayloadType</key>
      <string>com.apple.security.root</string>
      <key>PayloadUUID</key>
      <string>{payload}</string>
      <key>PayloadVersion</key>
      <integer>1</integer>
    </dict>
  </array>
  <key>PayloadDescription</key>
  <string>Compare the certificate fingerprint with the one on the session-bridge admin page before you trust it.</string>
  <key>PayloadDisplayName</key>
  <string>session-bridge phone app ({host})</string>
  <key>PayloadIdentifier</key>
  <string>local.session-bridge.phone.{profile}</string>
  <key>PayloadRemovalDisallowed</key>
  <false/>
  <key>PayloadType</key>
  <string>Configuration</string>
  <key>PayloadUUID</key>
  <string>{profile}</string>
  <key>PayloadVersion</key>
  <integer>1</integer>
</dict>
</plist>
"#
    )
}
