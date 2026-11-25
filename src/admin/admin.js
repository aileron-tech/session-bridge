// session-bridge setup page. All data is written with textContent.
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

function renderMessages(messages) {
  const body = $("messages-body");
  body.replaceChildren();
  $("messages").hidden = messages.length === 0;
  $("messages-empty").hidden = messages.length > 0;
  for (const message of messages.slice(0, 20)) {
    const row = document.createElement("tr");
    const when = new Date(message.created_at * 1000).toLocaleTimeString();
    const short = (id) => `${id.provider}:${id.id.slice(0, 8)}`;
    const state = message.delivery.state + (message.delivery.host ? ` to ${message.delivery.host}` : "");
    row.append(cell(when), cell(short(message.sender), "mono"), cell(short(message.recipient), "mono"), stateCell(state, tones[message.delivery.state] ?? "muted"));
    if (message.delivery.reason) row.title = message.delivery.reason;
    body.append(row);
  }
}

async function refresh() {
  try {
    const [lan, status] = await Promise.all([call({ method: "lan" }), call({ method: "status" })]);
    renderSelf(lan.lan.network);
    renderIncoming(lan.lan.pairing.incoming);
    renderHosts(lan.lan.hosts);
    renderMessages(status.messages);
    showError(null);
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
    await refresh();
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
    await refresh();
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
  refresh();
  setInterval(refresh, 2000);
});
