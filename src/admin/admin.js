// session-bridge admin page. All data is written with textContent.
"use strict";

const token = (() => {
  const match = location.hash.match(/token=([A-Z2-7]+)/);
  if (match) {
    sessionStorage.setItem("session-bridge-token", match[1]);
    history.replaceState(null, "", location.pathname);
  }
  return sessionStorage.getItem("session-bridge-token") || "";
})();

const $ = (id) => document.getElementById(id);
let pairing = null; // { id, host }
let editing = false;

async function call(body) {
  const response = await fetch("/api/call", {
    method: "POST",
    headers: { "Content-Type": "application/json", Authorization: `Bearer ${token}` },
    body: JSON.stringify(body),
  });
  if (response.status === 401) {
    throw new Error("Open this page with the link from: session-bridge admin");
  }
  if (!response.ok) {
    throw new Error(`Request refused (${response.status})`);
  }
  const reply = await response.json();
  if (reply.result === "error") {
    throw new Error(reply.message);
  }
  return reply;
}

function showError(error) {
  $("error").textContent = error ? error.message : "";
  $("error").hidden = !error;
}

function cell(text, className) {
  const td = document.createElement("td");
  td.textContent = text;
  if (className) td.className = className;
  return td;
}

function stateCell(text, tone) {
  const td = document.createElement("td");
  const span = document.createElement("span");
  span.className = `state state--${tone}`;
  span.textContent = text;
  td.append(span);
  return td;
}

function renderSelf(network) {
  const state = network.port ? `networking on, port ${network.port}` : network.enabled ? "networking on, not listening" : "networking off";
  $("self").textContent = `${network.host} - key ${network.key} - ${state}`;
  if (!editing) {
    $("network-name").value = network.host;
    $("network-port").value = network.port ?? "";
  }
  $("network-toggle").textContent = network.enabled ? "Turn networking off" : "Turn networking on";
  $("network-toggle").dataset.enabled = String(network.enabled);
}

function renderIncoming(incoming) {
  $("incoming").hidden = !incoming;
  if (!incoming) return;
  $("incoming-host").textContent = `${incoming.host} (${incoming.address})`;
  $("incoming-code").textContent = incoming.code;
  $("incoming-expires").textContent = incoming.expires_in;
}

function renderHosts(hosts) {
  const body = $("hosts-body");
  body.replaceChildren();
  $("hosts").hidden = hosts.length === 0;
  $("hosts-empty").hidden = hosts.length > 0;
  for (const host of hosts) {
    const row = document.createElement("tr");
    row.append(cell(host.host), cell(host.key, "mono"), cell(host.addresses[0] ?? "-", "mono"));
    if (host.paired) {
      row.append(stateCell(host.online ? "paired, online" : "paired, offline", host.online ? "ok" : "muted"));
    } else {
      row.append(stateCell("not paired", "warn"));
    }
    const action = document.createElement("td");
    const button = document.createElement("button");
    button.type = "button";
    if (host.paired) {
      button.className = "button button--danger";
      button.textContent = "Unpair";
      button.addEventListener("click", () => unpair(host.host));
    } else {
      button.className = "button";
      button.textContent = "Pair";
      button.addEventListener("click", () => startPairing(host.host));
    }
    action.append(button);
    row.append(action);
    body.append(row);
  }
}

const tones = { queued: "ok", delivered: "ok", forwarded: "ok", pending: "warn", dispatching: "warn", held: "warn", refused: "bad", uncertain: "bad" };

// Message text arrives as typed Markdown blocks; elements are built here and
// every piece of text goes through textContent or a text node.
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
      if (block.language) pre.dataset.language = block.language;
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

let compose = null; // { session_id, label }
let shownSessions = "";
let shownMessages = "";
let sessionsBusy = false;

function openCompose(sessionId, label) {
  compose = { session_id: sessionId, label };
  $("compose-to").textContent = label;
  $("compose").hidden = false;
  $("compose-hint").textContent = "The session reads this as your words. It never counts as approval for an action that needs permission.";
  $("compose-text").focus();
}

function renderSessions(reply) {
  const key = JSON.stringify([reply.sessions, reply.unavailable_hosts ?? []]);
  if (key === shownSessions) return;
  shownSessions = key;
  const unavailable = reply.unavailable_hosts ?? [];
  $("sessions-note").textContent = unavailable.length ? `Not answering: ${unavailable.join(", ")}` : "";
  const body = $("sessions-body");
  body.replaceChildren();
  $("sessions").hidden = reply.sessions.length === 0;
  $("sessions-empty").hidden = reply.sessions.length > 0;
  for (const session of reply.sessions) {
    const row = document.createElement("tr");
    const label = `${session.name} (${session.project})${session.host ? ` on ${session.host}` : ""}`;
    row.append(
      cell(session.host ?? "this laptop"),
      cell(session.agent),
      cell(session.project),
      cell(session.name, "mono"),
      cell(session.status),
    );
    const action = document.createElement("td");
    const button = element("button", "button button--quiet", "Message");
    button.type = "button";
    button.addEventListener("click", () => openCompose(session.session_id, label));
    action.append(button);
    row.append(action);
    body.append(row);
  }
}

function address(id) {
  return `${id.provider}:${id.id}`;
}

function renderMessages(reply) {
  const key = JSON.stringify(reply.messages.map((m) => [m.message_id, m.delivery]));
  if (key === shownMessages) return;
  shownMessages = key;
  const user = address(reply.user);
  const list = $("messages");
  list.replaceChildren();
  list.hidden = reply.messages.length === 0;
  $("messages-empty").hidden = reply.messages.length > 0;
  for (const message of reply.messages) {
    const toYou = address(message.recipient) === user;
    const item = element("li", `message${toYou ? " message--to-you" : ""}`);
    const meta = element("div", "message__meta");
    meta.append(element("span", "message__parties", `${message.sender_label} → ${message.recipient_label}`));
    const state = message.delivery.state + (message.delivery.host ? ` to ${message.delivery.host}` : "");
    const badge = element("span", `state state--${tones[message.delivery.state] ?? "muted"}`, state);
    if (message.delivery.reason) badge.title = message.delivery.reason;
    meta.append(badge);
    const when = new Date(message.created_at * 1000);
    const sameDay = when.toDateString() === new Date().toDateString();
    meta.append(element("time", "message__time", sameDay ? when.toLocaleTimeString() : when.toLocaleString()));
    item.append(meta, blockNodes(message.blocks, element("div", "message__body md")));
    if (toYou && message.sender.provider !== "user") {
      const answer = element("button", "button button--quiet message__reply", "Reply");
      answer.type = "button";
      answer.addEventListener("click", () => openCompose(address(message.sender), message.sender_label));
      item.append(answer);
    }
    list.append(item);
  }
}

async function refresh() {
  try {
    const [lan, messages] = await Promise.all([call({ method: "lan" }), call({ method: "messages" })]);
    renderSelf(lan.lan.network);
    renderIncoming(lan.lan.pairing.incoming);
    renderHosts(lan.lan.hosts);
    renderMessages(messages);
    showError(null);
  } catch (error) {
    showError(error);
  }
}

async function refreshSessions() {
  if (sessionsBusy) return;
  sessionsBusy = true;
  try {
    renderSessions(await call({ method: "list", sender: null, project: null }));
  } catch (error) {
    showError(error);
  } finally {
    sessionsBusy = false;
  }
}

async function sendMessage(event) {
  event.preventDefault();
  if (!compose) return;
  const text = $("compose-text").value;
  if (!text.trim()) return;
  try {
    const reply = await call({ method: "user_send", session_id: compose.session_id, message: text });
    $("compose-text").value = "";
    $("compose-hint").textContent = `Sent to ${compose.label}: ${reply.receipt.delivery.state}.`;
    showError(null);
    await refresh();
  } catch (error) {
    showError(error);
  }
}

async function startPairing(host) {
  try {
    const reply = await call({ method: "pair", host });
    pairing = { id: reply.id, host };
    $("code-host").textContent = host;
    $("code-status").textContent = `${host} now shows a code (valid ${reply.expires_in} s).`;
    $("code-panel").hidden = false;
    $("code-input").value = "";
    $("code-input").focus();
    showError(null);
  } catch (error) {
    showError(error);
  }
}

async function submitCode(event) {
  event.preventDefault();
  if (!pairing) return;
  try {
    const reply = await call({ method: "pair_code", id: pairing.id, code: $("code-input").value });
    if (reply.result === "wrong_code") {
      $("code-status").textContent = `Wrong code; ${reply.attempts_left} attempts left.`;
      $("code-input").select();
      return;
    }
    $("code-status").textContent = "";
    $("code-panel").hidden = true;
    pairing = null;
    showError(null);
    await Promise.all([refresh(), refreshSessions()]);
  } catch (error) {
    $("code-panel").hidden = true;
    pairing = null;
    showError(error);
  }
}

async function unpair(host) {
  if (!confirm(`Unpair ${host}? Its sessions will no longer reach this laptop.`)) return;
  try {
    await call({ method: "unpair", host });
    await Promise.all([refresh(), refreshSessions()]);
  } catch (error) {
    showError(error);
  }
}

async function toggleNetwork(event) {
  event.preventDefault();
  const enabled = $("network-toggle").dataset.enabled !== "true";
  const port = $("network-port").value.trim();
  try {
    await call({ method: "network", enabled, name: $("network-name").value.trim() || null, port: port ? Number(port) : null });
    editing = false;
    await refresh();
  } catch (error) {
    showError(error);
  }
}

document.addEventListener("DOMContentLoaded", () => {
  $("network").addEventListener("submit", toggleNetwork);
  for (const id of ["network-name", "network-port"]) {
    $(id).addEventListener("input", () => { editing = true; });
  }
  $("pair").addEventListener("submit", (event) => {
    event.preventDefault();
    const address = $("pair-address").value.trim();
    if (address) startPairing(address);
  });
  $("code").addEventListener("submit", submitCode);
  $("code-cancel").addEventListener("click", () => { pairing = null; $("code-panel").hidden = true; });
  $("compose-form").addEventListener("submit", sendMessage);
  $("compose-text").addEventListener("keydown", (event) => {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) sendMessage(event);
  });
  $("compose-cancel").addEventListener("click", () => { compose = null; $("compose").hidden = true; });
  refresh();
  refreshSessions();
  setInterval(refresh, 2000);
  setInterval(refreshSessions, 5000);
});
