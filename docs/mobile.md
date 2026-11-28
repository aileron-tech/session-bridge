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
   `sb-<label>.local`, independent of laptop networking (REQ-017). Phones
   reach the laptop by its LAN IP address; the `.local` name is an extra,
   never required (many phones do not resolve it). The loopback admin page
   is unchanged.
2. **HTTPS under a per-bridge root the phone trusts once**, not
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
a root. As built (since 0.4.5):

- Each bridge creates its own P-256 root (`phone-ca.json`, mode 0600, never
  in source), valid 10 years, `CA:true` with path length 0, and critical
  name constraints that permit only the private IPv4 ranges 10/8,
  172.16/12, 192.168/16, 169.254/16 and the bridge's `sb-<label>.local`
  name. The phone installs and trusts it once. iOS: a configuration profile
  (`.mobileconfig`), then Settings > General > About > Certificate Trust
  Settings > full trust. Android: Settings > Security > install a CA
  certificate (Chrome honours user certificates).
- The server certificate is issued by that root for the laptop's current
  LAN addresses (IP SANs) and the `.local` name, valid 397 days, and issued
  again while the listener runs when the address set changes or it nears its
  end (checked at most every 10 s, on a new connection). A new DHCP address
  therefore needs no new profile.
- The name constraints keep a stolen root key from vouching for public
  sites: it can impersonate only private addresses and the bridge's own
  name. Checked with macOS trustd (`security verify-cert -p ssl`, the same
  evaluation iOS uses): a leaf for 192.168.1.123 or the `.local` name is
  accepted, one for 8.8.8.8 or example.com is refused (and accepted under the
  same root without constraints). The unit tests check the same with webpki.
- 0.4.4 used a self-signed `CA:false` leaf for the `.local` name only; its
  file `phone-identity.json` is removed, and phones must install the new
  profile once.
- The phone's browser keys its storage by origin (scheme, address, port).
  When the laptop's address changes, the phone app at the new address has no
  key and must pair again (the root stays trusted). A DHCP reservation for
  the laptop avoids that. The old entry stays paired and counts toward the
  5 phones: the device list shows its last use (saved at most every 5 minutes
  while a phone makes calls), and the user unpairs it.
- Revoking: unpairing or turning phone access off ends a phone's access; the
  phone keeps trusting the root until the user removes the profile (iOS:
  Settings > General > VPN & Device Management). Keep `phone-ca.json`
  private: its key can vouch for any private address and the `.local` name.

Other options considered:

| Option | Why not |
|--------|---------|
| WebCrypto over plain HTTP | No WebCrypto on HTTP; script is not authenticated (above) |
| Tap through a self-signed certificate warning | Nothing is pinned; the user learns to accept warnings; service workers are refused on certificate-error pages |
| Public certificate for a project-owned domain pointing at LAN IPs (plex.direct style) | Needs a domain, an ACME DNS service and an online dependency the bridge does not have; LAN only was the decision |
| PWA hosted on a public HTTPS origin calling the LAN bridge | Mixed content and private network access block it; the bridge would still need TLS |
| Native iOS app with pinned keys | Strongest option, but the user chose a PWA; it needs signing and App Store review. Revisit if certificate install proves too hard for users |

The certificate install is the main usability cost. The admin page explains it
step by step, and the phone app refuses to pair when the bridge's root
differs from the one named in the pairing URL. The root is replaced only when
the host name changes or it nears its end (the phone must then trust the new
one).

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
- Requires the `Host` header to name the listener (DNS rebinding; any case):
  `<address>:<port>` for one of the laptop's current LAN addresses (read
  again on each request), or `sb-<label>.local:<port>`; and an `Origin` of
  `https://` plus one of those when present. No CORS headers. The phone signs
  the host it reached, and the bridge checks the signature against the
  request's `Host`.
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

1. **Trust the bridge.** The admin page shows the root's fingerprint
   (SHA-256 as 32 hex pairs, as iOS shows it, and its short form: the first
   8 pairs) and a QR code for `http://<lan-ip>:47822/bridge.mobileconfig`
   (one per LAN address when the laptop has several; the default route's
   first, so the user picks the one on the phone's network).
   The download is not trusted: during the iOS install the user compares the
   fingerprint iOS shows with the one on the laptop screen, then turns on
   full trust in Settings > General > About > Certificate Trust Settings.
2. **Show a code.** `phone_offer` opens one offer (a new one replaces it):
   code valid 120 s, single use; a wrong code uses the offer up. The QR code
   carries `https://<lan-ip>:47821/#pair=<code>&fp=<root sha256 hex>` (one
   link per address; the `.local` name only when there is no address); the
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
address?, addresses, fingerprint, fingerprint_short, profile_url?,
profile_qr?, profile_links, offer?: {id, code, url, qr, links, expires_in,
state}, devices: [{id, name, key, paired_at, last_seen?, last_address?,
sessions}], max_devices}}`. `address` is the default route's LAN address,
`addresses` all of them in order; `profile_links` and `offer.links` are
`[{address, url, qr}]`, one per address, and `profile_url`/`profile_qr`,
`url`/`qr` repeat the first. An ended offer has no links. `fingerprint` is
the root's. QR codes are rows of `0`/`1` without a quiet zone.

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
| `ChannelDefault`, `ProjectMaster`, `ProjectStar`, `RemoveWaiting` | not in v1 (user decision); the phone sees `starred` on projects (REQ-040) |
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
  minute, for pairing codes and sign-in challenges. A sign-in costs one
  token: the session request that uses a fresh nonce is not charged again
  (the nonce is single use and was paid for), so a limited phone never
  spends its token on a nonce it cannot use. After 5 failed signatures or
  codes, that address waits 60 s; that block applies to session requests too.
- After a 429 the phone app waits the `Retry-After` before its next sign-in
  attempt; with both rules a limited phone signs in once one token has
  refilled (about 6 s), instead of starving on its 5 s refresh.
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
| Bridge root key stolen from the laptop | name constraints: impersonates only private addresses and `sb-<label>.local`, never public sites; regenerate | an attacker on the LAN can impersonate the bridge, or another private address, to that phone until the root is replaced and the phone removes the old profile |
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
   trust the name-constrained root for the server certificate at the
   laptop's IP, in Safari and in the Home Screen app? (macOS trustd enforces
   the constraints; iOS is expected to match.)
2. The pairing link by IP opens on the phone; the `.local` name is optional.
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

