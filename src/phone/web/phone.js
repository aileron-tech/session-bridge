// session-bridge phone app (REQ-036). All data is written with textContent.
// The phone's key is a non-extractable WebCrypto P-256 key kept in IndexedDB;
// the bridge pins its public key when the user confirms on the laptop.
"use strict";

const $ = (id) => document.getElementById(id);
const POLL_MS = 5000;

// --- Key storage -----------------------------------------------------------

function database() {
  return new Promise((resolve, reject) => {
    const open = indexedDB.open("session-bridge-phone", 1);
    open.onupgradeneeded = () => open.result.createObjectStore("keys");
    open.onsuccess = () => resolve(open.result);
    open.onerror = () => reject(open.error);
  });
}
async function stored(action, value) {
  const db = await database();
  return new Promise((resolve, reject) => {
    const tx = db.transaction("keys", action === "get" ? "readonly" : "readwrite");
    const keys = tx.objectStore("keys");
    const request = action === "get" ? keys.get("phone")
      : action === "put" ? keys.put(value, "phone") : keys.delete("phone");
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}
function base64url(bytes) {
  let text = "";
  for (const b of new Uint8Array(bytes)) text += String.fromCharCode(b);
  return btoa(text).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
async function phoneKey() {
  let key = await stored("get");
  if (!key) {
    const pair = await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, false, ["sign"]);
    const raw = await crypto.subtle.exportKey("raw", pair.publicKey);
    key = { privateKey: pair.privateKey, publicKey: base64url(raw), deviceId: null, name: "" };
    await stored("put", key);
  }
  return key;
}

// --- Requests --------------------------------------------------------------

let session = null; // bearer token
let phone = null; // stored key record

class Refused extends Error {
  constructor(status, message, retryAfter) { super(message); this.status = status; this.retryAfter = retryAfter; }
}
// After a 429 the bridge says how long to wait (Retry-After); no sign-in is
// tried before then, so a limited phone does not spend the next token on a
// nonce it cannot use yet.
let waitUntil = 0;
function limited() {
  const seconds = Math.ceil((waitUntil - Date.now()) / 1000);
  return seconds > 0 ? new Refused(429, `Too many requests: wait ${seconds} s`, seconds) : null;
}
async function post(path, body, auth) {
  const headers = { "Content-Type": "application/json" };
  if (auth) headers.Authorization = `Bearer ${session}`;
  const response = await fetch(path, { method: "POST", headers, body: JSON.stringify(body || {}) });
  let reply = null;
  try { reply = await response.json(); } catch (_) { reply = null; }
  if (!response.ok) {
    const retryAfter = Number(response.headers.get("Retry-After")) || 0;
    if (response.status === 429) waitUntil = Date.now() + Math.max(retryAfter, 1) * 1000;
    throw new Refused(response.status, (reply && reply.message) || `Request refused (${response.status})`, retryAfter);
  }
  if (reply && reply.result === "error") throw new Refused(200, reply.message);
  return reply;
}
async function signIn() {
  const wait = limited();
  if (wait) throw wait;
  const { nonce } = await post("/api/challenge");
  const message = new TextEncoder().encode(`session-bridge phone v1\n${location.hostname.toLowerCase()}\n${nonce}`);
  const signature = await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, phone.privateKey, message);
  const reply = await post("/api/session", { device_id: phone.deviceId, nonce, signature: base64url(signature) });
  session = reply.token;
}
// A control request. A 401 happens before the bridge acts, so signing in
// again and repeating is safe.
async function call(body) {
  if (!session) await signIn();
  try {
    return await post("/api/call", body, true);
  } catch (error) {
    if (!(error instanceof Refused) || error.status !== 401) throw error;
    await signIn();
    return post("/api/call", body, true);
  }
}

function showError(error) {
  const text = error ? error.message : "";
  $("error").textContent = text;
  $("error").hidden = !text;
}
function setOffline(offline) { $("offline").hidden = !offline; }

// --- Markdown blocks (same shapes as the admin page) ----------------------

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}
function inlineNodes(items, parent) {
  for (const item of items) parent.append(inlineNode(item));
  return parent;
}
function inlineNode(item) {
  switch (item.type) {
    case "text": return document.createTextNode(item.text);
    case "code": return element("code", "md__code", item.text);
    case "strong": return inlineNodes(item.content, element("strong"));
    case "emphasis": return inlineNodes(item.content, element("em"));
    case "strike": return inlineNodes(item.content, element("del"));
    case "break": return element("br");
    case "link": {
      if (item.href && /^(https?:|mailto:)/i.test(item.href)) {
        const link = inlineNodes(item.content, element("a", "md__link"));
        link.href = item.href;
        link.target = "_blank";
        link.rel = "noopener noreferrer";
        return link;
      }
      return inlineNodes(item.content, element("span"));
    }
    default: return document.createTextNode("");
  }
}
function blockNodes(blocks, parent) {
  for (const block of blocks) parent.append(blockNode(block));
  return parent;
}
function blockNode(block) {
  switch (block.type) {
    case "paragraph": return inlineNodes(block.content, element("p", "md__p"));
    case "heading": return inlineNodes(block.content, element(`h${Math.min(6, block.level + 2)}`, "md__h"));
    case "code": {
      const pre = element("pre", "md__pre");
      pre.append(element("code", null, block.text));
      return pre;
    }
    case "list": {
      const list = element(block.start === null ? "ul" : "ol", "md__list");
      if (block.start !== null && block.start !== 1) list.start = block.start;
      for (const item of block.items) list.append(blockNodes(item, element("li")));
      return list;
    }
    case "quote": return blockNodes(block.blocks, element("blockquote", "md__quote"));
    case "table": {
      const wrap = element("div", "md__table");
      const table = element("table");
      const head = element("tr");
      for (const cell of block.head) head.append(inlineNodes(cell, element("th")));
      table.append(element("thead"));
      table.tHead.append(head);
      const body = element("tbody");
      for (const row of block.rows) {
        const tr = element("tr");
        for (const cell of row) tr.append(inlineNodes(cell, element("td")));
        body.append(tr);
      }
      table.append(body);
      wrap.append(table);
      return wrap;
    }
    case "rule": return element("hr", "md__rule");
    default: return document.createTextNode("");
  }
}

// --- Navigation ------------------------------------------------------------

const panes = ["pair", "confirm", "chats", "conversation", "cron", "roles", "settings"];
let tab = "chats";
let open = null; // { kind: "channel" | "direct", project?, session_id?, title }

function show(pane) {
  for (const id of panes) $(id).hidden = id !== pane;
  const signedIn = !["pair", "confirm"].includes(pane);
  $("tabs").hidden = !signedIn;
  $("back").hidden = pane !== "conversation";
  $("title").textContent = pane === "conversation" && open ? open.title : "session-bridge";
  for (const button of $("tabs").querySelectorAll("button")) {
    if (button.dataset.tab === tab) button.setAttribute("aria-current", "page");
    else button.removeAttribute("aria-current");
  }
}
function selectTab(name) {
  tab = name;
  open = null;
  show(name);
  refresh();
}

// --- Data ------------------------------------------------------------------

let user = "";
let sessions = [];
let messages = [];
let waiting = {};
let schedules = [];
let roles = { roles: [], assignments: {}, files: {} };
// The version of the role file being edited: the bridge refuses the save
// if the file changed since (REQ-037).
let editingVersion = null;

const sid = (id) => `${id.provider}:${id.id}`;
function stateText(delivery) {
  return delivery && delivery.state ? delivery.state : "";
}
function when(seconds) {
  return new Date(seconds * 1000).toLocaleString([], { dateStyle: "short", timeStyle: "short" });
}

function conversations() {
  const projects = [...new Set(sessions.map((s) => s.project))].sort();
  const list = projects.map((project) => ({ kind: "channel", project, title: `#${project}` }));
  for (const s of sessions.slice().sort((a, b) => a.name.localeCompare(b.name))) {
    list.push({
      kind: "direct",
      session_id: s.session_id,
      title: s.host ? `${s.name} on ${s.host}` : s.name,
      project: s.project,
    });
  }
  return list;
}
function inConversation(m, c) {
  if (c.kind === "channel") return m.channel === c.project;
  if (m.channel) return false;
  const from = sid(m.sender);
  const to = sid(m.recipient);
  return (from === user && to === c.session_id) || (from === c.session_id && to === user);
}

function renderChats() {
  const list = $("chat-list");
  list.replaceChildren();
  for (const c of conversations()) {
    const item = element("li");
    const button = element("button", "list__item");
    button.type = "button";
    const name = element("span", null, c.title);
    const count = c.kind === "direct" && waiting[c.session_id] ? waiting[c.session_id].count : 0;
    if (count) name.append(element("span", "badge", `${count} unread`));
    button.append(name);
    const last = messages.filter((m) => inConversation(m, c)).sort((a, b) => b.created_at - a.created_at)[0];
    button.append(element("span", "list__meta", last ? `${last.sender_label}: ${last.message.slice(0, 80)}` : c.kind === "channel" ? "Channel" : c.project));
    button.addEventListener("click", () => { open = c; show("conversation"); renderThread(); });
    item.append(button);
    list.append(item);
  }
  if (!list.children.length) list.append(element("li", "hint", "No live sessions."));
}

function renderThread() {
  if (!open) return;
  const thread = $("thread");
  const atEnd = window.innerHeight + window.scrollY >= document.body.scrollHeight - 40;
  thread.replaceChildren();
  const shown = messages.filter((m) => inConversation(m, open)).sort((a, b) => a.created_at - b.created_at);
  for (const m of shown) {
    const mine = sid(m.sender) === user || (m.channel && m.sender.provider === "user");
    const item = element("li", mine ? "message message--mine" : "message");
    item.append(element("div", "message__meta", `${mine ? "You" : m.sender_label} - ${when(m.created_at)}${mine ? ` - ${stateText(m.delivery)}` : ""}`));
    item.append(blockNodes(m.blocks || [], element("div")));
    thread.append(item);
  }
  const note = $("waiting-note");
  const count = open.kind === "direct" && waiting[open.session_id] ? waiting[open.session_id].count : 0;
  note.textContent = count ? `${count} earlier message(s) not read yet.` : "";
  note.hidden = !count;
  if (atEnd) window.scrollTo(0, document.body.scrollHeight);
}

function renderSchedules() {
  const list = $("schedule-list");
  list.replaceChildren();
  for (const s of schedules) {
    const item = element("li");
    const button = element("button", "list__item");
    button.type = "button";
    button.append(element("span", null, `${s.cron} - #${s.project}${s.to ? ` to ${s.to}` : ""}`));
    button.append(element("span", "list__meta", s.message.slice(0, 100)));
    if (s.last_run) button.append(element("span", "list__meta", `Last run ${when(s.last_run.at)}: ${s.last_run.summary}`));
    button.addEventListener("click", () => editSchedule(s));
    const remove = element("button", "secondary", "Delete");
    remove.type = "button";
    remove.setAttribute("aria-label", `Delete schedule ${s.cron} for #${s.project}`);
    remove.addEventListener("click", () => act(async () => {
      if (!confirm("Delete this schedule?")) return;
      await call({ method: "delete_schedule", id: s.id });
      await refresh();
    }));
    item.append(button, remove);
    list.append(item);
  }
  if (!schedules.length) list.append(element("li", "hint", "No schedules."));
  const select = $("schedule-project");
  const current = select.value;
  select.replaceChildren();
  for (const project of [...new Set(sessions.map((s) => s.project).concat(schedules.map((s) => s.project)))].sort()) {
    const option = element("option", null, project);
    option.value = project;
    select.append(option);
  }
  if (current) select.value = current;
}
function editSchedule(s) {
  $("schedule-form-title").textContent = s ? "Edit schedule" : "New schedule";
  $("schedule-id").value = s ? s.id : "";
  $("schedule-cron").value = s ? s.cron : "";
  $("schedule-project").value = s ? s.project : $("schedule-project").value;
  $("schedule-to").value = s && s.to ? s.to : "";
  $("schedule-message").value = s ? s.message : "";
  $("schedule-team").checked = !!(s && s.team_report);
  $("schedule-cron").focus();
}

function renderRoles() {
  const list = $("role-list");
  list.replaceChildren();
  for (const role of roles.roles) {
    const item = element("li");
    const button = element("button", "list__item");
    button.type = "button";
    const holders = Object.entries(roles.assignments).filter(([, r]) => r === role.name).map(([n]) => n);
    button.append(element("span", null, role.name));
    button.append(element("span", "list__meta", holders.length ? `Held by ${holders.join(", ")}` : "Not assigned"));
    button.addEventListener("click", () => editRole(role));
    item.append(button);
    if (role.name !== "root") {
      const remove = element("button", "secondary", "Delete");
      remove.type = "button";
      remove.setAttribute("aria-label", `Delete role ${role.name}`);
      remove.addEventListener("click", () => act(async () => {
        if (!confirm(`Delete the role ${role.name} and its assignments?`)) return;
        await call({ method: "delete_role", name: role.name });
        await refresh();
      }));
      item.append(remove);
    }
    list.append(item);
  }
  const sessionSelect = $("assign-session");
  const chosen = sessionSelect.value;
  sessionSelect.replaceChildren();
  for (const s of sessions.filter((x) => !x.host)) {
    const option = element("option", null, `${s.name} (${s.project})`);
    option.value = s.session_id;
    sessionSelect.append(option);
  }
  if (chosen) sessionSelect.value = chosen;
  const roleSelect = $("assign-role");
  const role = roleSelect.value;
  roleSelect.replaceChildren(Object.assign(element("option", null, "No role"), { value: "" }));
  for (const r of roles.roles) roleSelect.append(Object.assign(element("option", null, r.name), { value: r.name }));
  roleSelect.value = role;
}
function editRole(role) {
  $("role-form-title").textContent = role ? `Edit ${role.name}` : "New role";
  $("role-previous").value = role ? role.name : "";
  editingVersion = role ? (roles.files[role.name] || {}).version || null : null;
  $("role-name").value = role ? role.name : "";
  $("role-prompt").value = role ? role.prompt : "";
  $("role-name").focus();
}

// One refresh at a time; a request during one runs again right after it,
// so a tab switch never waits for the next poll.
let refreshing = false;
let again = false;
async function refresh() {
  if (!phone || !phone.deviceId || document.hidden) return;
  if (refreshing) { again = true; return; }
  refreshing = true;
  try {
    const listed = await call({ method: "list", sender: null, project: null });
    sessions = listed.sessions || [];
    const mail = await call({ method: "messages" });
    user = sid(mail.user);
    messages = mail.messages || [];
    waiting = (await call({ method: "waiting" })).waiting || {};
    if (tab === "cron") schedules = (await call({ method: "schedules" })).schedules || [];
    if (tab === "roles") {
      const reply = await call({ method: "roles" });
      roles = { roles: reply.roles || [], assignments: reply.assignments || {}, files: reply.files || {} };
    }
    setOffline(false);
    showError(null);
    if (tab === "chats") { if (open) renderThread(); else renderChats(); }
    if (tab === "cron") renderSchedules();
    if (tab === "roles") renderRoles();
  } catch (error) {
    if (error instanceof Refused) {
      if (error.status === 401 && /no longer paired|not a paired phone/i.test(error.message)) return forgetLocally();
      showError(error);
    } else {
      setOffline(true);
    }
  } finally {
    refreshing = false;
    if (again) { again = false; refresh(); }
  }
}
async function act(work) {
  try {
    await work();
    showError(null);
  } catch (error) {
    showError(error);
  }
}

// --- Sending ---------------------------------------------------------------

// The draft keeps its client_id until the bridge answers, so a manual
// resend after a lost reply is answered once and never sent twice.
let draftId = null;
async function send(event) {
  event.preventDefault();
  const text = $("composer-text").value.trim();
  if (!text || !open) return;
  if (!draftId) draftId = crypto.randomUUID();
  const body = open.kind === "channel"
    ? { method: "channel_send", project: open.project, message: text, client_id: draftId }
    : { method: "user_send", session_id: open.session_id, message: text, client_id: draftId };
  const state = $("send-state");
  state.hidden = false;
  state.textContent = "Sending...";
  try {
    const reply = await call(body);
    draftId = null;
    $("composer-text").value = "";
    state.textContent = reply.result === "posted" && reply.failed && reply.failed.length
      ? `Not reached: ${reply.failed.join(", ")}` : "";
    state.hidden = !state.textContent;
    await refresh();
  } catch (error) {
    if (error instanceof Refused) {
      draftId = null;
      state.textContent = error.message;
    } else {
      // No answer: the bridge may have saved it. Never resent on its own.
      state.textContent = "Unknown whether it was sent: check the conversation before sending again.";
    }
  }
}

// --- Pairing ---------------------------------------------------------------

function standalone() {
  return window.matchMedia("(display-mode: standalone)").matches || navigator.standalone === true;
}
async function pair(event) {
  event.preventDefault();
  const fragment = new URLSearchParams(location.hash.slice(1));
  const expected = (fragment.get("fp") || "").toUpperCase();
  await act(async () => {
    const name = $("pair-name").value.trim();
    const reply = await post("/api/pair", { code: $("pair-code").value, name, public_key: phone.publicKey });
    if (expected && reply.fingerprint.toUpperCase() !== expected) {
      throw new Error("This bridge's certificate does not match the pairing code's. Do not continue.");
    }
    phone.name = name;
    await stored("put", phone);
    history.replaceState(null, "", "/");
    $("confirm-key").textContent = reply.key;
    $("confirm-cert").textContent = reply.fingerprint_short;
    show("confirm");
    waitForLaptop();
  });
}
async function waitForLaptop() {
  try {
    const status = await post("/api/pair/status", { public_key: phone.publicKey });
    if (status.state === "paired") {
      phone.deviceId = status.device_id;
      await stored("put", phone);
      selectTab("chats");
      return;
    }
    if (status.state === "waiting") {
      setTimeout(waitForLaptop, 2000);
      return;
    }
    $("confirm-state").textContent = status.state === "declined"
      ? "The laptop declined. Show a new code there to try again."
      : "The code expired. Show a new code on the laptop.";
    setTimeout(() => show("pair"), 4000);
  } catch (error) {
    showError(error);
    setTimeout(waitForLaptop, 4000);
  }
}
async function forgetLocally() {
  await stored("delete");
  session = null;
  phone = await phoneKey();
  show("pair");
}

// --- Start -----------------------------------------------------------------

async function start() {
  if (!window.isSecureContext || !crypto.subtle) {
    showError(new Error("Open this app over https from the pairing code."));
    return;
  }
  phone = await phoneKey();
  $("back").addEventListener("click", () => { open = null; show("chats"); renderChats(); });
  for (const button of $("tabs").querySelectorAll("button")) {
    button.addEventListener("click", () => selectTab(button.dataset.tab));
  }
  $("pair-form").addEventListener("submit", pair);
  $("composer").addEventListener("submit", send);
  $("composer-text").addEventListener("input", () => { if (!$("composer-text").value) draftId = null; });
  $("schedule-new").addEventListener("click", () => editSchedule(null));
  $("schedule-form").addEventListener("submit", (event) => {
    event.preventDefault();
    act(async () => {
      const id = $("schedule-id").value;
      await call({
        method: "save_schedule",
        id: id || null,
        cron: $("schedule-cron").value.trim(),
        project: $("schedule-project").value,
        to: $("schedule-to").value.trim() || null,
        message: $("schedule-message").value,
        team_report: $("schedule-team").checked,
      });
      editSchedule(null);
      await refresh();
    });
  });
  $("role-new").addEventListener("click", () => editRole(null));
  $("role-form").addEventListener("submit", (event) => {
    event.preventDefault();
    act(async () => {
      await call({
        method: "save_role",
        name: $("role-name").value.trim(),
        prompt: $("role-prompt").value,
        previous: $("role-previous").value || null,
        version: editingVersion,
      });
      editRole(null);
      await refresh();
    });
  });
  $("assign-form").addEventListener("submit", (event) => {
    event.preventDefault();
    act(async () => {
      await call({ method: "assign_role", session_id: $("assign-session").value, role: $("assign-role").value || null });
      await refresh();
    });
  });
  $("forget").addEventListener("click", () => act(async () => {
    if (!confirm("Forget this phone? Pair it again from the laptop to use it.")) return;
    if (!session) await signIn();
    await post("/api/forget", {}, true);
    await forgetLocally();
  }));
  document.addEventListener("visibilitychange", () => { if (!document.hidden) refresh(); });
  setInterval(refresh, POLL_MS);

  const fragment = new URLSearchParams(location.hash.slice(1));
  if (fragment.get("pair")) $("pair-code").value = fragment.get("pair").replace(/(.{4})(?=.)/g, "$1-");
  $("install-hint").hidden = standalone();
  $("settings-name").textContent = phone.name ? `Paired as ${phone.name}.` : "";
  if (phone.deviceId) {
    selectTab("chats");
  } else {
    show("pair");
  }
}
start().catch(showError);
