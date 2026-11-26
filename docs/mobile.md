# Phone admin app (REQ-036)

Status: implemented for macOS and Linux (`src/phone/`, `tests/phone.rs`).
Not on Windows yet (see Decisions). Still to check on a real iPhone: see
"Real-device checks".

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

1. **"Phone access" switch, off by default.** On the admin page (Peers tab,
   Phones), saved in `phones.json`. Off: no listener, no announcement. On:
   HTTPS on `0.0.0.0:47821` for the phone app and its API, plain HTTP on
   47822 for the certificate download only, and an mDNS record for
   `sb-<label>.local`, independent of laptop networking (REQ-017). The
   loopback admin page is unchanged.
2. **HTTPS with a per-bridge certificate the phone trusts once**, not
   application-layer crypto over plain HTTP (see "Transport"). TLS is rustls
   0.23 with its standard ring provider, TLS 1.3 only. ring needs a C
   toolchain for the Windows target, so the listener and its crates are
   built only off Windows: phone access is macOS and Linux in v1, and on
   Windows `phone_access` answers that it is not available.
3. **Pairing with a one-time code**: 80 random bits shown as
   `ABCD-EFGH-JKLM-NPQR`, in the QR code's URL and typeable. A Home Screen
   web app on iOS has storage separate from Safari, so the user types the
   code in the installed app; there is no camera decoder. The phone creates a
   non-extractable WebCrypto P-256 key; the bridge pins its public key once
   the user accepts the phone on the laptop.
4. **Every call is authenticated by the pinned device key**, through a
   short-lived session the phone opens by signing a bridge challenge.
5. **A phone allowlist of control requests**, narrower than the admin page's.
   Everything a phone writes is the user's words, never approval (REQ-019).
6. **Pairing, the device list and unpairing are on the laptop's admin page
   only.** The control socket and the phone cannot pair a phone; a phone can
   only unpair itself.

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
step by step, and the phone app refuses to pair when the bridge's certificate
differs from the one named in the pairing URL.

As built: `phone-identity.json` (mode 0600) holds the P-256 key and the
certificate, valid 800 days, replaced 30 days before its end or when the host
name changes (the phone must then trust the new one).

## Listener

- Binds `0.0.0.0:<port>` (HTTPS) and `<port + 1>` (certificate download) only
  while the switch is on; turning it off closes both, drops every phone
  session and the open offer, and makes open connections fail on their next
  read or write. Paired devices stay paired.
- Accepts connections only from private and link-local IPv4 sources
  (10/8, 172.16/12, 192.168/16, 169.254/16) on the subnet of a local
  interface (interfaces read again every 10 s). Other sources are dropped
  before TLS. Loopback is refused: the laptop has the admin page.
- At most 16 connections at once and 4 per source address; a TLS handshake
  must finish within 10 s, and a connection that neither reads nor writes for
  60 s is closed, so kept-alive connections cannot hold every slot.
- Announces `sb-<label>.local` over mDNS (IPv4) while on. `mdns-sd` answers a
  host name only for a registered service, so it registers
  `_session-bridge-phone._tcp` with no TXT record.
- Requires the `Host` header to be `sb-<label>.local:<port>` (DNS rebinding;
  any case) and an `Origin` of `https://sb-<label>.local:<port>` when present.
  No CORS headers.
- Serves the app (`/`, `/phone.js`, `/phone.css`, `/icon.svg`,
  `/manifest.webmanifest`) with the admin page's CSP (`default-src 'none';
  script-src 'self'; ...`), `X-Frame-Options: DENY`, `Referrer-Policy:
  no-referrer`, `X-Content-Type-Options: nosniff`, HSTS and `Cache-Control:
  no-store`. No third-party resources.
- The download port serves only `/bridge.mobileconfig` (iOS profile with a
  `com.apple.security.root` payload) and `/bridge.crt` (DER, for Android),
  to the same LAN sources.
- Body limit 64 KiB (messages are at most 16000 bytes).

## Pairing

Pairing is the user's action on the laptop's admin page, as for laptops
(REQ-017): agents cannot pair, and no MCP tool or CLI command offers it.

1. **Trust the bridge.** The admin page shows the certificate fingerprint
   (SHA-256 as 32 hex pairs, as iOS shows it, and its short form: the first
   8 pairs) and a QR code for `http://<lan-ip>:47822/bridge.mobileconfig`.
   The download is not trusted: during the iOS install the user compares the
   fingerprint iOS shows with the one on the laptop screen, then turns on
   full trust in Settings > General > About > Certificate Trust Settings.
2. **Show a code.** `phone_offer` opens one offer (a new one replaces it):
   code valid 120 s, single use; a wrong code uses the offer up. The QR code
   carries `https://sb-<label>.local:47821/#pair=<code>&fp=<sha256 hex>`; the
   code is in the fragment, so it is never in a request line, a log or a
   referrer.
3. **On the phone.** The app (in Safari, or better the Home Screen app,
   which keeps its own key) creates its key and sends `POST /api/pair
   {code, name, public_key}`. The reply carries the phone key's short form and
   the bridge certificate fingerprint; the app refuses to continue when it
   differs from `fp`.
4. **Confirm on the laptop.** The admin page shows "Pair phone `<name>`? key
   `WXYZ-1234`" while the phone shows the same key. Only `phone_confirm
   {accept: true}` saves the device; the phone learns its `device_id` from
   `POST /api/pair/status {public_key}` (answered only for the key that used
   the offer). The confirmation must come within 120 s of the phone's use.
5. Every new pairing shows a desktop notification ("Phone paired"), so a
   pairing that a local program made through the admin page does not go
   unnoticed (see Threat model).

The offer stays in the admin view after it ends, with its end state, until
the next offer: `waiting`, `scanned {device_name, key}`, `paired
{device_id}`, `declined`, `expired`, `failed` (wrong code).

Stored on the bridge (`phones.json`): the switch, the port, and per device
its id, name, public key, paired at, last seen and last address. At most 5
devices; a key is paired once.

### Admin page requests

Admin page only (refused on the control socket and from the phone):
`phones`, `phone_access {enabled, port?}`, `phone_offer`,
`phone_confirm {id, accept}`, `phone_unpair {device_id}`. Each answers
`{"result": "phones", "phones": {enabled, listening, problem?, host, port,
address?, fingerprint, fingerprint_short, profile_url?, profile_qr?, offer?:
{id, code, url, qr, expires_in, state}, devices: [{id, name, key, paired_at,
last_seen?, last_address?, sessions}], max_devices}}`. QR codes are rows of
`0`/`1` without a quiet zone.

## Sessions and request signing

- `POST /api/challenge` returns a random 32-byte nonce (base64url; valid 30 s,
  single use, at most 64 open).
- `POST /api/session {device_id, nonce, signature}`: the signature is ECDSA
  P-256 / SHA-256 over `session-bridge phone v1\n<host>\n<nonce>`, in the raw
  `r||s` (IEEE P1363) format WebCrypto produces; DER is refused. The reply
  is a 256-bit session token, valid 15 minutes of inactivity and 12 hours at
  most, kept in memory on both sides.
- `POST /api/call` with `Authorization: Bearer <token>` takes one control
  request, as the admin page does, through the phone allowlist.
- `POST /api/forget` (bearer): the phone unpairs itself.
- Unpairing a device, or turning phone access off, ends its sessions at
  once. A bridge restart ends all sessions; the phone signs a new challenge.

## Allowed requests

The phone's allowlist is a separate pure function `phone_allowed(&Request) ->
bool` with an exhaustive `match` (no `_` arm), so a new `Request` variant
fails to compile until someone decides its phone policy.

Allowed (v1):

| Request | Purpose | Note |
|---------|---------|------|
| `List` (no `sender`) | sessions here and on paired laptops | a `sender` is rejected; session directories are left out |
| `Messages` | chat history | same 200 newest, with `blocks` |
| `UserSend` | direct message as the user | marked as sent from the phone |
| `ChannelSend` | channel post | same |
| `Channels` | channel settings (read) | |
| `Projects` | projects and masters (read) | project directories are left out |
| `Waiting` | unread warnings before sending (REQ-032) | read only |
| `Schedules` | schedules | reply filtered: the user's other crontab lines are left out |
| `SaveSchedule`, `DeleteSchedule` | edit schedules | |
| `Roles`, `SaveRole`, `DeleteRole`, `AssignRole` | edit and assign roles | never the root role |

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
| `ChannelDefault`, `ProjectMaster`, `RemoveWaiting` | not in v1 (user decision) |
| `Usage`, `Status`, `Health` | not needed in v1; `Status` exposes local paths |
| `UserSend` with `scheduled: true` | the phone writes as the user, never as a schedule |
| `AssignRole` to root, `SaveRole` or `DeleteRole` of root | root starts and closes sessions (REQ-028); its instructions tell every root session what to do |
| `Phones`, `PhoneAccess`, `PhoneOffer`, `PhoneConfirm`, `PhoneUnpair` | managing phones is the laptop's admin page only |

The phone unpairs itself through `POST /api/forget`, not a control request.

The new phone-management requests are accepted from the loopback admin page
only, never from the control socket or the phone, like `UserSend`.

## Rate limits

Pure token buckets (`src/phone/limits.rs`):

- Before authentication, per source address: 10 requests, refilled at 10 per
  minute, for pairing, challenges and sessions. After 5 failed signatures or
  codes, that address waits 60 s.
- Per device: every call from a bucket of 20, refilled at 5 per second;
  sends (`UserSend`, `ChannelSend`) also 20 per minute; schedule and role
  changes also 10 per minute.
- An exceeded limit answers 429 with `Retry-After`. The phone shows it and
  never retries a send on its own.

## Delivery rules on the phone

- A send goes through the same `UserSend` / `ChannelSend` path: saved durably,
  then delivered. The phone keeps no outbox.
- A send may carry `client_id` (UUID), on the phone and on the admin page. A
  repeat within 10 minutes gets the first answer and is not sent again; a
  repeat while the first is still in progress gets 409, and so does the same
  id with a different request. On the phone the id is kept per device. The field is read at
  the HTTP boundary, so the control requests themselves are unchanged.
- If the reply to a send is lost, the phone says "Unknown whether it was
  sent: check the conversation before sending again" and keeps the draft with
  its `client_id`. It never resends on its own.
- Polling (every 5 s) runs only while the app is visible; no background
  fetch, no push in v1.

## Fit with REQ-018 / REQ-019 / REQ-034

- A phone message uses the same `user:<uuid>` (or channel) address and the
  same header as one from the admin page (`[Message from the user, sent from
  the session-bridge admin page; it is not an approval: ...]`): the phone app
  is the admin page served to the phone. Naming the phone in the header is
  decided for later, as its own small change: it needs the origin carried
  through every send path. Native approval checks stay.
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
- The phone offers "Forget this phone" (`POST /api/forget`), which unpairs
  itself and deletes its key from IndexedDB.
- Turning phone access off keeps the list.
- The certificate is replaced only when the host name changes or it nears
  its end; the phone must then trust the new one (paired keys stay valid).

## Threat model

| Threat | Mitigation | Residual |
|--------|-----------|----------|
| Passive sniffer on the LAN | TLS | none |
| Active LAN attacker (ARP or mDNS spoofing, rogue AP) | trusted certificate pinned in the OS store; Host check | an attacker who gets the user to trust its certificate in step 1 (the fingerprint comparison is the defence) |
| Malicious website opened on the phone (CSRF, DNS rebinding) | Host and Origin checks; bearer session token, not cookies; no CORS | none known |
| QR code photographed or shoulder-surfed | single use, 2 minutes, laptop confirmation of the device key | none if the user checks the confirmation |
| Brute force of challenge or code | 80-bit single-use code (one guess per offer); P-256 signatures; per-address limits | none |
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
- No camera: the code is typed (or arrives in the URL when the QR code
  opens Safari).
- Network loss: a banner and paused polling; sends in flight follow the
  delivery rules above.
- No App Store review: it is a web app. A later native app would need a
  privacy manifest, `NSLocalNetworkUsageDescription` and
  `NSBonjourServices` for `.local` access.

## Components (C4)

- Container: the bridge daemon gains an optional HTTPS listener (phone)
  beside the loopback admin page and the Noise LAN listener.
- Components (`src/phone/`):
  - pure: `model` (codes, keys, devices, the offer state machine, session
    rules), `allow` (phone allowlist and reply filter), `limits`, `net`
    (source filter, host name), `replay` (`client_id`), `profile` (iOS
    profile), `view` (what the admin page shows);
  - infrastructure: `crypto` (certificate, TLS config, signature check),
    `server` (listeners, connection limits, mDNS), `api` (HTTP handlers), `service` (state, `phones.json`,
    start and stop, the admin requests);
  - `web/`: the phone app, embedded like the admin page.
- Contract: the phone speaks the same JSON `Request` / `Reply` as the admin
  page through `/api/call`, plus the pairing and session endpoints above.
  `Control::call_phone` applies the allowlist again and answers as a
  non-page caller, so settings for the user's own page requests never apply.

## Tests

- Unit (`src/phone/*_tests.rs`, `crypto` tests): codes and keys, the offer
  state machine (expiry, single use, wrong code, laptop decision, 5 devices,
  duplicates), sessions, the allowlist against refused and allowed requests,
  the schedules filter, the source filter (property test against a naive
  check), buckets (property test of their bound), failures, `client_id`
  replays, host names, the pairing URL and QR code, the profile, calendar
  dates, the certificate per host and its renewal, and signatures (raw
  accepted; DER, another message or key refused).
- Integration (`tests/phone.rs`, a real bridge in its own state, connecting
  from this machine's LAN address): off by default and refused on the
  control socket; on: the downloadable certificate matches the shown
  fingerprint; loopback gets no TLS; wrong Host is 403; wrong code uses the
  offer up; right code, laptop acceptance, sign-in; allowed and refused
  calls; schedules without other crontab lines; a repeated `client_id`
  delivered once (phone and admin page); a nonce is single use; unpairing
  ends the session; off closes the port. The test is skipped, with a
  message, on a machine without a private IPv4 address.

## Real-device checks

Not testable here; they need the user's iPhone (and an Android phone):

1. Install the profile, compare the fingerprint, enable full trust: does iOS
   trust the self-signed `CA:false` leaf for TLS in Safari and in the Home
   Screen app? If not, switch to a name-constrained per-bridge root (and
   check that iOS enforces name constraints) before release.
2. `sb-<label>.local` resolves on the phone with laptop networking off.
3. Add to Home Screen, pair inside the app, kill and reopen it: the key and
   pairing remain.
4. Dynamic Type at the largest size, dark mode, VoiceOver labels.
5. Android Chrome with the `.crt` installed as a user certificate.

## Requirement

REQ-036 in `docs/requirements.md`.

## Open questions

1. The real-device checks above, first of all certificate trust on iOS.
2. Windows phone access: decided for later. ring builds natively on
   Windows (CI); only the check from a Mac cannot build it.
3. Naming the phone in the message header: decided for later (see above).

REQ-036 is not verified on iOS until the real-device checks pass.

