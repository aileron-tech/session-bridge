# Phone admin app (draft)

Status: draft design, not implemented. No requirement is allocated yet; the
proposed text for REQ-036 is at the end.

## Problem

The user wants to follow and steer their sessions from a phone while away
from the laptop but on the same network: read and send chat and channel
messages, and edit cron schedules and roles. Today the admin page (REQ-018,
REQ-019) is served on 127.0.0.1 only, so a phone cannot reach it.

Why now: the admin page already carries everything the phone needs; the work
left is reaching it safely from a second device.

Scope (user decisions): a PWA served by the bridge on the LAN, opened by
scanning a QR code on the admin page; v1 reads and sends chat and channel
messages, edits cron schedules and roles; LAN only.

Out of scope for v1: access from outside the LAN (no public port, no relay),
push notifications, starting or closing sessions, run settings, pairing
laptops, networking and remote hosts, a native app.

## Decisions

1. **"Phone access" switch, off by default.** A switch on the admin page's
   Peers tab, saved in the bridge state like the network setting. Off: no
   listener, no announcement. On: a separate HTTPS listener (default port
   47821) for the phone app only; the loopback admin page is unchanged.
2. **HTTPS with a per-bridge certificate the phone trusts once**, not
   application-layer crypto over plain HTTP. See "Transport: why not WebCrypto
   over HTTP".
3. **Pairing from a QR code** that carries the address, a one-time secret and
   the bridge certificate fingerprint. The phone generates a non-extractable
   P-256 device key in the browser; the bridge pins its public key.
4. **Every request is authenticated by the pinned device key**, through a
   short-lived session the phone obtains by signing a bridge challenge.
5. **A phone allowlist of control requests**, narrower than the admin page's.
   Everything a phone writes is the user's words, never approval (REQ-019).
6. **Pairing, device list and unpairing live on the laptop's admin page only.**
   The control socket and the phone cannot pair a phone.

## Transport: why not WebCrypto over HTTP

The proposal in the task was WebCrypto ECDH P-256 + AES-GCM over plain HTTP.
It does not work, for two reasons:

- `crypto.subtle` is only exposed in secure contexts (HTTPS, or
  `http://localhost`). A page loaded from `http://192.168.1.31:47821` has no
  WebCrypto, no service worker and no installable PWA. A pure JS crypto library
  would be needed.
- Even with a JS library, the code itself would come over plain HTTP. An active
  attacker on the LAN (ARP spoofing, rogue access point, mDNS spoofing) changes
  the script on any load and reads the keys. App-layer encryption over an
  unauthenticated page protects only against passive sniffing. That is the
  weaker attacker.

Browsers cannot pin a self-signed certificate, but the phone's OS can trust
one. The proposed option:

- The bridge creates a P-256 key and a self-signed TLS certificate for its
  mDNS host name `sb-<label>.local` (the name REQ-017 already uses), with SAN
  `DNS:sb-<label>.local`, EKU `serverAuth`, `basicConstraints CA:false`, and
  validity of at most 825 days (iOS limit for TLS server certificates). It is
  kept in the state directory with mode 0600, never in source.
- The phone installs it once. iOS: a configuration profile
  (`.mobileconfig`), then Settings > General > About > Certificate Trust
  Settings > full trust. Android: Settings > Security > install a CA
  certificate (Chrome honours user certificates).
- A `CA:false` leaf certificate cannot sign other certificates. A stolen
  bridge key then lets an attacker impersonate only `sb-<label>.local`, not
  arbitrary sites. **To verify on a device:** whether iOS accepts a trusted
  self-signed `CA:false` leaf for TLS. If it does not, fall back to a
  per-bridge root CA with `nameConstraints` permitted to
  `sb-<label>.local`, and verify that iOS enforces name constraints. If
  neither holds, stop and bring it back to the user; do not ship an
  unconstrained root.
- The host name and not the IP goes in the certificate, so a new DHCP lease
  needs no reinstall. iOS and Android resolve `.local` names over mDNS.

Other options considered:

| Option | Why not |
|--------|---------|
| WebCrypto over plain HTTP | No WebCrypto on HTTP; script is not authenticated (above) |
| Tap through a self-signed certificate warning | Nothing is pinned; the user learns to accept warnings; service workers are refused on certificate-error pages |
| Public certificate for a project-owned domain pointing at LAN IPs (plex.direct style) | Needs a domain, an ACME DNS service and an online dependency the bridge does not have; LAN only was the decision |
| PWA hosted on a public HTTPS origin calling the LAN bridge | Mixed content and private network access block it; the bridge would still need TLS |
| Native iOS app with pinned keys | Strongest option, but the user chose a PWA; it needs signing and App Store review. Revisit if certificate install proves too hard for users |

The certificate install is the main usability cost. The admin page explains it
step by step, with screenshots, and the QR flow below checks that it worked
before it pairs.

## Listener

- Binds `0.0.0.0:<port>` only while the switch is on; turning it off closes
  the listener and drops every phone session. Paired devices stay paired.
- Accepts connections only from private and link-local IPv4 sources
  (10/8, 172.16/12, 192.168/16, 169.254/16) that fall within a subnet of a
  local interface. Other sources are dropped before TLS. Loopback is refused:
  the laptop already has the admin page.
- Announces the `sb-<label>.local` A record over mDNS while on, even when
  laptop networking (REQ-017) is off. It announces no service and no TXT, so
  the phone listener is not advertised as a session bridge.
- Requires the `Host` header to be `sb-<label>.local:<port>` (DNS rebinding)
  and, on API calls, an `Origin` equal to `https://sb-<label>.local:<port>`
  when present. No CORS headers.
- Serves the PWA (HTML, JS, CSS, icon, manifest, service worker) with the
  admin page's CSP (`default-src 'none'; script-src 'self'; ...`),
  `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`, HSTS for that
  host, and `Cache-Control: no-store` on API replies. No third-party
  resources. The QR decoder is vendored and served by the bridge.
- Body limit: the message size limit plus envelope overhead; larger bodies are
  rejected before parsing.

## Pairing

Pairing is the user's action on the laptop's admin page, as for laptops
(REQ-017): agents cannot pair, and no MCP tool or CLI command offers it.

QR flow (the admin page shows each step):

1. **Trust the bridge.** The page shows the certificate fingerprint (SHA-256,
   short form `ABCD-EFGH` plus the full hex) and a first QR code with a plain
   HTTP download URL for the profile. The download itself is not trusted:
   during the iOS install the user compares the fingerprint iOS shows with the
   one on the laptop screen. The admin page says so.
2. **Pair.** The page asks the bridge for a pairing offer and shows a second
   QR code:
   `https://sb-<label>.local:<port>/#pair=<secret>&fp=<cert sha256>`.
   - `secret`: 128 random bits, base32, single use, valid 2 minutes; one open
     offer at a time; a new offer cancels the old one.
   - `fp`: the certificate fingerprint. The PWA cannot read the TLS
     certificate, so `fp` is not a pin in the browser. The pin is the OS trust
     store from step 1. The bridge proves possession instead: the pairing
     reply carries the fingerprint the bridge serves, and the PWA refuses a
     mismatch, which catches a QR from another bridge.
   - The secret is in the fragment, so it never appears in a request line,
     a log or a referrer.
3. **On the phone.** The scan opens Safari. If TLS fails, the page never
   loads; the admin page shows "Phone did not connect: is the certificate
   trusted?" after 30 seconds. On load, the PWA:
   - generates a P-256 ECDSA key pair with `extractable: false` and keeps it
     in IndexedDB;
   - sends `phone_pair {secret, device_name, public_key}`;
   - receives `{device_id, cert_fp}` and checks `cert_fp` against `fp`.
4. **Confirm on the laptop.** The admin page shows "Pair phone `<device_name>`?
   key `WXYZ-1234`" and the phone shows the same key short form. The user
   confirms on the laptop. Only then is the device saved. This keeps a scanned
   or photographed QR from pairing a stranger's phone silently within the 2
   minutes.
5. A desktop notification announces every new pairing ("Phone `<name>`
   paired"), so a pairing that a local program made through the admin page
   does not go unnoticed (see Threat model).

iOS storage: a Home Screen web app has storage separate from Safari. A key
created in a Safari tab is not visible to the installed app. The pairing page
therefore asks the user to "Add to Home Screen" first, and the installed app
pairs on its first start: it shows "Scan the pairing code" and reads the
second QR with the camera (`getUserMedia`, vendored decoder). A Safari tab can
pair too, for users who do not install. Home Screen web apps are not subject
to Safari's 7-day storage removal; a Safari tab is, and must be paired again
after 7 days without use.

Stored on the bridge (`phones.json`, mode 0600): `device_id` (UUID), name,
public key (SPKI), fingerprint, paired at, last seen, last source address.
At most 5 devices.

## Sessions and request signing

- `POST /api/session/challenge` returns a random 32-byte nonce (valid 30 s,
  single use).
- `POST /api/session` with `{device_id, nonce, signature}`. The signature is
  ECDSA P-256 / SHA-256 over `"session-bridge phone v1" || host || nonce`.
  WebCrypto produces the raw `r||s` (IEEE P1363) format, not DER, and the
  server verifies that format. The reply is a 256-bit session token, valid 15
  minutes of inactivity and at most 12 hours, kept in memory on both sides
  only.
- `POST /api/call` with `Authorization: Bearer <session token>` takes one
  control request, as the admin page does. The token is compared in constant
  time.
- Unpairing a device, or turning phone access off, drops its sessions at
  once. A bridge restart drops all sessions; the phone signs a new challenge.

The device key gives per-device identity and revocation, which a bearer token
in the QR would not. TLS gives confidentiality and integrity.

## Allowed requests

The phone's allowlist is a separate pure function `phone_allowed(&Request) ->
bool` with an exhaustive `match` (no `_` arm), so a new `Request` variant
fails to compile until someone decides its phone policy.

Allowed (v1):

| Request | Purpose | Note |
|---------|---------|------|
| `List` (no `sender`) | sessions here and on paired laptops | a `sender` is rejected |
| `Messages` | chat history | same 200 newest, with `blocks` |
| `UserSend` | direct message as the user | marked as sent from the phone |
| `ChannelSend` | channel post | same |
| `Channels` | channel settings (read) | |
| `Projects` | projects and masters (read) | for grouping and the master badge |
| `Waiting` | unread warnings before sending (REQ-032) | read only |
| `Schedules` | schedules | reply filtered: the user's other crontab lines are left out |
| `SaveSchedule`, `DeleteSchedule` | edit schedules | |
| `Roles`, `SaveRole`, `DeleteRole`, `AssignRole` | edit and assign roles | |
| `PhoneForget` (new) | the phone unpairs itself | only its own `device_id` |

Never on the phone:

| Request | Why |
|---------|-----|
| `Send`, `NativePeer`, `ClaimSpawn` | agent and process traffic, not the user's |
| `Shutdown`, `Admin` | control socket only; `Admin` returns the loopback token |
| `Network`, `Lan`, `Pair`, `PairCode`, `Unpair` | laptop pairing is a laptop action |
| `Remotes`, `AddRemote`, `RemoveRemote` | `AddRemote` runs `ssh` with the user's keys |
| `Spawn`, `Close` | out of v1 scope; starts processes |
| `SpawnSettings`, `SaveSpawnSettings`, `ImportAliases` | approvals and flags for started sessions (REQ-034); the phone must not lower approvals |
| `RunSchedule` | cron's request; not needed to edit schedules |
| `ChannelDefault`, `ProjectMaster`, `RemoveWaiting` | not in v1; candidates for v2 |
| `Usage`, `Status`, `Health` | not needed in v1; `Status` exposes local paths |
| `PhoneAccess`, `PhonePair`, `PhoneDevices`, `PhoneUnpair` (new) | managing phones is the laptop's admin page only |

The new phone-management requests are accepted from the loopback admin page
only, never from the control socket or the phone, like `UserSend`.

## Rate limits

Pure token buckets in the domain, fed a clock by the listener:

- Before authentication, per source IP: 10 requests per minute for challenge,
  session and pairing. After 5 failed signatures or pairing attempts, that
  address waits 60 s.
- Pairing: one open offer; the secret is single use. A wrong secret consumes
  the offer (the user scans a new code).
- Per device: 5 requests per second, burst 20 (polling `Messages` and
  `Waiting` included); sends (`UserSend`, `ChannelSend`) 20 per minute;
  writes to schedules and roles 10 per minute.
- At most 2 concurrent connections per device and 10 in total.
- An exceeded limit answers 429 with `Retry-After`. The phone shows it and
  never retries a send on its own.

## Delivery rules on the phone

- A send goes through the same `UserSend` / `ChannelSend` path: saved durably,
  then delivered. The phone does not keep its own outbox.
- If the reply to a send is lost (network loss, app suspended), the phone shows
  the message as "unknown: check the conversation before sending again" and
  never resends automatically, as the bridge's rule for uncertain delivery
  requires. Proposal: the phone sends a `client_id` (UUID) per message and the
  bridge drops a repeat within 10 minutes. A manual resend after a lost reply
  is then harmless (open question 3).
- Polling runs only while the app is visible (`visibilitychange`); no
  background fetch, no push in v1.

## Fit with REQ-018 / REQ-019 / REQ-034

- A phone message uses the same `user:<uuid>` (or channel) address. The
  recipient's header says it was written by the user through the phone admin
  app (`<device name>`) and is not an approval. Native approval checks stay.
- The device key is a stronger authenticator than the loopback token, but it
  is still not consent: a phone can be unlocked by someone else, and an agent
  can ask the user to type anything. A phone message therefore never carries
  approval, exactly like the admin page.
- Role texts edited on the phone become first prompts of sessions the user
  starts later (REQ-028). That is the user's words, like an edit on the admin
  page; approvals for those sessions come only from the Run tab on the laptop,
  which the phone cannot change.
- Schedules saved from the phone install the same managed crontab line
  (`<cron> <command> scheduled <id>`); no user text enters the crontab. The
  existing `validate` applies.
- REQ-019's limit remains: any program of the laptop user can drive the
  loopback admin page, so it can also turn phone access on and pair a device.
  The laptop confirmation (pairing step 4), the desktop notification and the
  device list make that visible; they do not prevent it.

## Device list and unpairing

- The Peers tab lists phones: name, key short form, paired at, last seen,
  last address, active sessions. "Unpair" removes the key and drops its
  sessions at once. A request from an unpaired key is refused with 401.
- The phone offers "Forget this phone" (`PhoneForget`), which unpairs itself
  and deletes its key from IndexedDB.
- Turning phone access off keeps the list. A "Remove all phones" button is
  offered next to the switch.
- Regenerating the certificate (button, with a warning) unpairs every phone,
  because each needs to trust the new certificate.

## Threat model

| Threat | Mitigation | Residual |
|--------|-----------|----------|
| Passive sniffer on the LAN | TLS | none |
| Active LAN attacker (ARP or mDNS spoofing, rogue AP) | trusted certificate pinned in the OS store; Host check | an attacker who gets the user to trust its certificate in step 1 (the fingerprint comparison is the defence) |
| Malicious website opened on the phone (CSRF, DNS rebinding) | Host and Origin checks; bearer session token, not cookies; no CORS | none known |
| QR code photographed or shoulder-surfed | single use, 2 minutes, laptop confirmation of the device key | none if the user checks the confirmation |
| Brute force of challenge or secret | 128-bit secret; P-256 signatures; per-IP limits | none |
| Lost or stolen phone | unpair from the laptop; sessions end after 15 min idle; the phone lock screen | messages already shown on the phone; device storage is as safe as the phone |
| XSS in the PWA reading messages | strict CSP, no inline script, text-only rendering from typed blocks (REQ-019); the device key is non-extractable | script running in the page can use the key while the page is open |
| Phone message taken as approval | marked as the user's words, never approval; native checks stay | an agent may still be persuaded by text, as today |
| Phone lowering approvals or starting processes | `SaveSpawnSettings`, `Spawn`, `AddRemote` are never allowed | none |
| Local program on the laptop pairs its own device for remote persistence | laptop confirmation, desktop notification, device list | not prevented (REQ-019 limit); such a program can already act as the user |
| Bridge certificate key stolen from the laptop | `CA:false` leaf (or name-constrained CA): impersonates only `sb-<label>.local`; regenerate | attacker on the LAN can impersonate the bridge to the phone until it is regenerated |
| Flooding from the LAN | rate limits, connection caps, body limit, private-source filter | the listener's port is reachable while on |

## iOS platform notes

- Home Screen app: `manifest.webmanifest` with `display: standalone`, icons,
  `theme_color` for light and dark; `viewport-fit=cover` and
  `env(safe-area-inset-*)` paddings.
- Dynamic Type: `font: -apple-system-body` and relative units; dark mode
  through `prefers-color-scheme`; VoiceOver: semantic HTML (lists, headings,
  buttons with labels), `aria-live="polite"` for new messages, focus moved
  to the composer after sending.
- No keychain for web apps: the non-extractable WebCrypto key in IndexedDB is
  the closest equivalent. Deleting the Home Screen app deletes the key; the
  laptop list still shows the device until it is unpaired.
- Camera permission is asked only when the user taps "Scan pairing code".
- Network loss: a banner and paused polling; sends in flight follow the
  delivery rules above.
- No App Store review: it is a web app. A later native app would need a
  privacy manifest, `NSLocalNetworkUsageDescription` and
  `NSBonjourServices` for `.local` access, and `NSCameraUsageDescription`.

## Components (C4)

- Container: the bridge daemon gains an optional HTTPS listener (phone),
  beside the loopback admin page and the Noise LAN listener.
- Components:
  - `phone` domain (pure): device and offer types, pairing state machine
    (offer, scanned, confirmed, expired, used), `phone_allowed`, rate-limit
    buckets, challenge rules. Returns `Result`, no I/O.
  - `phone` infrastructure: TLS listener (rustls with a pure Rust provider if
    it cross-builds for Windows; see the REQ-017 note on `ring`), certificate
    creation, `phones.json`, mDNS host record, signature verification.
  - Admin page: Phone access switch, QR steps, confirmation, device list.
  - PWA: static files embedded like the admin page; reuses the admin page's
    Markdown block renderer.
- Contract: the phone speaks the same JSON `Request` / `Reply` as the admin
  page through `/api/call`, plus the session and pairing endpoints above.

## Test plan

Unit (pure, annotated with the new REQ id):

- `phone_allowed` against every `Request` variant: the allowed table above
  passes and every other variant is refused (the exhaustive `match` also
  breaks the build on a new variant).
- Pairing state machine: expiry at 2 minutes, single use, a wrong secret ends
  the offer, a new offer cancels the old one, no save without laptop
  confirmation, at most 5 devices.
- Source filter: private and link-local addresses on a local subnet pass;
  public, loopback and off-subnet addresses fail (property test over random
  IPv4 addresses against a naive implementation).
- Rate limits: buckets refill and refuse as specified (property test over
  random request timings).
- Signature verification: fixed WebCrypto-generated vectors in P1363 format;
  DER, wrong host, reused nonce and expired nonce fail.
- `Schedules` reply filter drops foreign crontab lines.

Integration (a separate daemon with its own `SESSION_BRIDGE_STATE`, never the
installed service):

- Switch off: nothing listens on the port. On: TLS with the generated
  certificate (`curl --cacert`), wrong Host gets 403, cross Origin gets 403.
- Pair through the endpoints with a test key, confirm through the admin API,
  open a session, call each allowed request, get 403 on each refused one.
- Unpair while a session is open: the next call gets 401. Turning off drops
  the session. A restart keeps `phones.json` and drops sessions.
- A send with a lost reply is not resent; a repeat `client_id` is dropped
  (if adopted).
- Rate limit answers 429 with `Retry-After`.

End-to-end:

- iOS Simulator (Xcode current, iPhone with the current iOS): add the
  certificate with `xcrun simctl keychain booted add-root-cert`, open the QR
  URL in Safari, pair, send a channel post and see the answer arrive; also
  test Dynamic Type at the largest size, dark mode, and VoiceOver labels
  through the Accessibility Inspector; take screenshots.
- Playwright WebKit against the test daemon for chat, schedule and role
  edits.
- A real iPhone for what the simulator cannot show: profile install and the
  fingerprint screen, whether a `CA:false` leaf is trusted, `.local`
  resolution, the camera QR scan in the Home Screen app, and storage across
  app restarts.
- An Android phone with Chrome: user certificate install and the same flow.

## Proposed requirement

- REQ-036 (draft): When the user turns on "Phone access" on the admin page,
  the bridge serves a phone admin app over HTTPS on the local network only,
  with a certificate the phone trusts once (fingerprint shown on the admin
  page). The user pairs a phone by scanning a one-time QR code (valid 2
  minutes, single use) and confirming the phone's key on the laptop. Every
  call is authenticated by that phone's pinned key. A paired phone lists
  sessions, reads and sends chat and channel messages, and edits schedules
  and roles; it cannot change networking, pairing, remote hosts, run settings
  or approvals, or start or close sessions. Phone messages are the user's
  words, never approval (REQ-019). Phone access is off by default; only the
  admin page turns it on, pairs and unpairs phones.

## Open questions

1. Certificate model: is the one-time certificate install acceptable to the
   user, or should v1 be a native app instead? Recommendation: the
   certificate install, verified on a real iPhone first.
2. Laptop confirmation of each pairing (step 4): keep it? Recommendation:
   yes; it costs one click.
3. `client_id` dedup for phone sends: adopt it? It adds an optional field to
   `UserSend` / `ChannelSend`. Recommendation: yes.
4. Announcing the `sb-<label>.local` host record when laptop networking is
   off: acceptable, or should phone access require networking on?
5. v1 allowlist: are `ChannelDefault`, `ProjectMaster` and `RemoveWaiting`
   wanted on the phone? They are left out here.
6. Default port 47821 and the limit of 5 phones.
