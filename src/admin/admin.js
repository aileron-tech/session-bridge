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
  if (response.status === 409) {
    // A role file changed on disk (REQ-037) says so; a repeated send does not.
    const reply = await response.json().catch(() => null);
    throw new Error(reply?.message ?? "Still sending the same message; wait a moment");
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

let selfName = "this laptop";
let shownHosts = [];

// Usage (REQ-029), shown on the Peers tab only: host name -> summary and this
// laptop's session usage; refreshed every 5 s while that tab is visible.
let usage = { loaded: false, hosts: new Map(), sessions: [] };
let usageBusy = false;

function hostUsage(name) {
  if (!usage.loaded) return "";
  return usage.hosts.get(name) ?? "usage unavailable";
}

function renderSessionUsage() {
  const rows = usage.sessions;
  $("usage-sessions").hidden = !usage.loaded || rows.length === 0;
  $("usage-sessions-empty").hidden = !usage.loaded || rows.length > 0;
  const live = new Map(sessions.map((s) => [s.session_id, s]));
  $("usage-sessions-body").replaceChildren(...rows.map((row) => {
    const session = live.get(row.session_id);
    const tr = document.createElement("tr");
    tr.append(cell(session?.name ?? row.session_id), cell(session?.project ?? "-"), cell(session?.agent ?? "-"));
    // A shared process (Codex app server) is marked; its numbers are the whole process's.
    tr.append(row.usage.kind === "shared" ? stateCell(row.summary, "warn") : cell(row.summary, "usage-cell"));
    return tr;
  }));
}

async function refreshUsage() {
  if (usageBusy || $("tab-peers").hidden) return;
  usageBusy = true;
  try {
    const reply = await call({ method: "usage" });
    usage = {
      loaded: true,
      hosts: new Map(reply.hosts.map((h) => [h.host, h.summary])),
      sessions: reply.sessions,
    };
    const own = reply.hosts.find((h) => h.local);
    $("self-usage").textContent = own ? own.summary : "";
    renderHosts(shownHosts);
    renderSessionUsage();
  } catch (error) {
    showError(error);
  } finally {
    usageBusy = false;
  }
}

function renderSelf(network) {
  selfName = network.host;
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
  shownHosts = hosts;
  renderPeersNav();
  const body = $("hosts-body");
  body.replaceChildren();
  $("hosts").hidden = hosts.length === 0;
  $("hosts-empty").hidden = hosts.length > 0;
  for (const host of hosts) {
    const row = document.createElement("tr");
    row.dataset.qa = `host:${host.host}`;
    row.append(cell(host.host), cell(host.key, "mono"), cell(host.addresses[0] ?? "-", "mono"));
    if (host.paired) {
      row.append(stateCell(host.online ? "paired, online" : "paired, offline", host.online ? "ok" : "muted"));
    } else {
      row.append(stateCell("not paired", "warn"));
    }
    row.append(cell(host.paired ? hostUsage(host.host) : "-", "usage-cell"));
    const action = document.createElement("td");
    const button = document.createElement("button");
    button.type = "button";
    if (host.paired) {
      button.className = "button button--danger";
      button.textContent = "Unpair";
      button.dataset.qa = "host-unpair";
      button.addEventListener("click", () => unpair(host.host));
    } else {
      button.className = "button";
      button.textContent = "Pair";
      button.dataset.qa = "host-pair";
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

// Chat: one conversation per pair of parties, like direct messages. The
// user's conversations are listed per session; conversations between agents
// are read-only. A session that is no longer live has ended (REQ-038): its
// conversations leave the sidebar once the page knows the live sessions, and
// a channel is listed only while its project has a live session.
let sessions = [];
let sessionsLoaded = false;
let unavailable = [];
let mail = { user: null, messages: [] };
let selected = null; // { kind: "dm", address } | { kind: "pair", key } | { kind: "channel", project }
let shownSidebar = "";
let shownThread = "";
let shownSelection = "";
let sessionsBusy = false;
let channelSettings = {};
let roleBook = { roles: [], assignments: {}, folder: "", files: {}, not_loaded: [] };

function address(id) {
  return `${id.provider}:${id.id}`;
}

function pairKey(a, b) {
  return [a, b].sort().join("|");
}

function initials(label) {
  const words = label.replace(/\(.*\)/, "").trim().split(/[\s_-]+/).filter(Boolean);
  return (words.length > 1 ? words[0][0] + words[1][0] : (words[0] ?? "?").slice(0, 2)).toUpperCase();
}

function avatarTone(text) {
  let hash = 0;
  for (const c of text) hash = (hash * 31 + c.charCodeAt(0)) >>> 0;
  return hash % 6;
}

function avatar(label, key) {
  const mark = element("span", `avatar avatar--${key === "you" ? "you" : avatarTone(key)}`, key === "you" ? "You" : initials(label));
  // Decorative: the name is printed next to it.
  mark.setAttribute("aria-hidden", "true");
  return mark;
}

// Everyone the page can name: live sessions, then parties seen in messages.
function parties() {
  const known = new Map();
  for (const m of mail.messages) {
    known.set(address(m.sender), { label: m.sender_label, live: false });
    known.set(address(m.recipient), { label: m.recipient_label, live: false });
  }
  for (const session of sessions) {
    known.set(session.session_id, { label: session.name, live: true, session });
  }
  return known;
}

function isEnded(party) {
  return sessionsLoaded && /^(claude|codex):/.test(party) && !sessions.some((s) => s.session_id === party);
}

function conversations() {
  const user = mail.user && address(mail.user);
  const byKey = new Map();
  const channels = new Map();
  for (const m of mail.messages) {
    if (m.channel) {
      const entry = channels.get(m.channel) ?? { project: m.channel, messages: [], last: 0 };
      entry.messages.push(m);
      entry.last = Math.max(entry.last, m.created_at);
      channels.set(m.channel, entry);
      continue;
    }
    const from = address(m.sender);
    const to = address(m.recipient);
    const key = pairKey(from, to);
    const entry = byKey.get(key) ?? { key, members: [from, to], messages: [], last: 0 };
    entry.messages.push(m);
    entry.last = Math.max(entry.last, m.created_at);
    byKey.set(key, entry);
  }
  const dms = new Map();
  const agents = [];
  for (const entry of byKey.values()) {
    if (entry.members.some(isEnded)) continue;
    if (user && entry.members.includes(user)) {
      const other = entry.members.find((a) => a !== user) ?? user;
      dms.set(other, entry);
    } else {
      agents.push(entry);
    }
  }
  agents.sort((a, b) => b.last - a.last);
  for (const session of sessions) {
    if (!channels.has(session.project)) channels.set(session.project, { project: session.project, messages: [], last: 0 });
  }
  const channelList = [...channels.values()]
    .filter((c) => !sessionsLoaded || channelMembers(c.project).length > 0)
    .sort((a, b) => b.last - a.last || a.project.localeCompare(b.project));
  return { user, dms, agents, channels: channelList };
}

function sessionLine(session) {
  return [session.project, session.host ?? "this laptop", session.agent, session.status].join(" - ");
}

function badge(text, kind) {
  return element("span", `badge badge--${kind}`, text);
}

// `qa`: a stable hook for the qa/ tests (data-qa), never shown.
function sidebarItem(label, sub, tone, active, onClick, time, mark, badges = [], qa = "") {
  const item = element("li");
  const button = element("button", `chat__item${active ? " chat__item--active" : ""}`);
  button.type = "button";
  if (active) button.setAttribute("aria-current", "true");
  if (qa) button.dataset.qa = qa;
  const channelMark = mark ? element("span", "avatar avatar--channel", mark) : null;
  if (channelMark) channelMark.setAttribute("aria-hidden", "true");
  button.append(channelMark ?? avatar(label, tone));
  const text = element("span", "chat__item-text");
  const name = element("span", "chat__item-name", label);
  for (const extra of badges) name.append(" ", extra);
  text.append(name, element("span", "chat__item-sub", sub));
  button.append(text);
  if (time) button.append(element("time", "chat__item-time", shortTime(time)));
  button.addEventListener("click", onClick);
  item.append(button);
  return item;
}

function shortTime(seconds) {
  const when = new Date(seconds * 1000);
  return when.toDateString() === new Date().toDateString()
    ? when.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
    : when.toLocaleDateString([], { month: "short", day: "numeric" });
}

function channelMembers(project) {
  return sessions.filter((s) => s.project === project);
}

// A party's project: a live session's own, else the one in its label "name (project)".
function projectOf(address, known) {
  const party = known.get(address);
  if (party?.session) return party.session.project;
  const match = party?.label.match(/\(([^)]+)\)( on .+)?$/);
  return match ? match[1] : "other";
}

function list(items) {
  const ul = element("ul", "chat__list");
  for (const item of items) ul.append(item);
  return ul;
}

// The laptop (peer) a party is on: a live session's host or this laptop,
// else the "on <host>" of its label.
function peerOf(address, known) {
  const party = known.get(address);
  if (party?.session) return party.session.host ?? selfName;
  const match = party?.label.match(/ on (.+)$/);
  return match ? match[1] : selfName;
}

function sessionBadges(address, known, { localPeer = true } = {}) {
  const party = known.get(address);
  const peer = peerOf(address, known);
  const out = localPeer || peer !== selfName ? [badge(peer, "peer")] : [];
  const name = party?.session?.name;
  const role = name && roleBook.assignments[name];
  if (role) out.push(badge(role, "role"));
  const waiting = waitingBy[address]?.count;
  if (waiting) out.push(badge(`${waiting} waiting`, "waiting"));
  return out;
}

// What waits for each session (REQ-032): unsent in the bridge, or unread in Codex's queue.
let waitingBy = {};
let waitingBusy = false;

async function refreshWaiting() {
  if (waitingBusy || $("tab-chats").hidden) return;
  waitingBusy = true;
  try {
    const next = (await call({ method: "waiting" })).waiting;
    if (JSON.stringify(next) !== JSON.stringify(waitingBy)) {
      waitingBy = next;
      shownSidebar = "";
      shownThread = "";
      renderChat();
    }
  } catch (error) {
    showError(error);
  } finally {
    waitingBusy = false;
  }
}

async function removeWaiting(sessionId, item) {
  if (!confirm("Remove this message? It will not be delivered.")) return;
  try {
    const request = { method: "remove_waiting", session_id: sessionId, message_id: item.message_id };
    if (item.queued_id) request.queued_id = item.queued_id;
    waitingBy = (await call(request)).waiting;
    $("thread-waiting").dataset.key = "";
    shownSidebar = "";
    shownThread = "";
    renderChat();
    showError(null);
  } catch (error) {
    showError(error);
  }
}

function waitingLine(name, view) {
  const oldest = new Date(view.items[0].created_at * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const inCodex = view.items.some((i) => i.place === "codex");
  return `${name} has ${view.count} message${view.count === 1 ? "" : "s"} waiting unread (oldest ${oldest}${inCodex ? ", in Codex's queue" : ", not handed over yet"})`;
}

function renderWaiting(known) {
  const address = selected?.kind === "dm" ? selected.address : null;
  const view = address ? waitingBy[address] : null;
  const box = $("thread-waiting");
  box.hidden = !view;
  const warning = $("composer-warning");
  if (view) {
    const name = known.get(address)?.session?.name ?? "This session";
    const key = JSON.stringify(view);
    if (box.dataset.key !== key) {
      box.dataset.key = key;
      $("thread-waiting-summary").textContent = waitingLine(name, view);
      $("thread-waiting-items").replaceChildren(...view.items.map((item) => {
        const li = element("li", "waiting__item");
        if (item.message_id) li.dataset.qa = `waiting:${item.message_id}`;
        const when = new Date(item.created_at * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
        li.append(element("time", "waiting__time", when), element("span", "waiting__place", item.place === "codex" ? "Codex queue" : "bridge"), element("span", "waiting__text", item.preview));
        // A bridge message can be taken back before it is read.
        if (item.message_id) {
          const remove = element("button", "button button--danger waiting__remove", "Remove");
          remove.type = "button";
          remove.dataset.qa = "waiting-remove";
          remove.addEventListener("click", () => removeWaiting(address, item));
          li.append(remove);
        }
        return li;
      }));
    }
    warning.textContent = `${waitingLine(name, view)}. Your message queues behind them.`;
    warning.hidden = false;
    return;
  }
  if (selected?.kind === "channel") {
    const busy = channelMembers(selected.project)
      .map((m) => [m.name, waitingBy[m.session_id]])
      .filter(([, v]) => v);
    warning.hidden = busy.length === 0;
    warning.textContent = busy.length ? `Waiting unread: ${busy.map(([n, v]) => `${n} ${v.count}`).join(", ")}. New posts queue behind them.` : "";
    return;
  }
  warning.hidden = true;
}

function renderPeerFilter(peers) {
  const select = $("peer-filter");
  const key = peers.join("|");
  if (select.dataset.key === key) return;
  select.dataset.key = key;
  const current = select.value;
  select.replaceChildren(new Option("All", ""), ...peers.map((p) => new Option(p === selfName ? `${p} (this laptop)` : p, p)));
  select.value = peers.includes(current) ? current : "";
}

function renderSidebar() {
  const { dms, agents, channels } = conversations();
  const known = parties();
  const groupBy = $("group-by").value;
  const peerFilter = $("peer-filter").value;
  const sortBy = $("sort-by").value;
  const key = JSON.stringify([[...dms].map(([a, e]) => [a, e.last]), agents.map((a) => [a.key, a.last]), channels.map((c) => [c.project, c.last]), sessions.map((s) => [s.session_id, s.status]), unavailable, selected, groupBy, peerFilter, sortBy, roleBook.assignments, selfName]);
  if (key === shownSidebar) return;
  shownSidebar = key;
  $("sessions-note").hidden = unavailable.length === 0;
  $("sessions-note").textContent = `Not answering: ${unavailable.join(", ")}`;

  // Sessions you can write to: live ones and past conversations.
  const people = new Map();
  for (const session of sessions) people.set(session.session_id, dms.get(session.session_id)?.last ?? 0);
  for (const [other, entry] of dms) people.set(other, Math.max(people.get(other) ?? 0, entry.last));
  const peers = [...new Set([selfName, ...[...people.keys()].map((a) => peerOf(a, known))])].sort((a, b) => (a === selfName ? -1 : b === selfName ? 1 : a.localeCompare(b)));
  renderPeerFilter(peers);
  const rows = [...people].map(([address, last]) => ({ address, last, peer: peerOf(address, known), project: projectOf(address, known) }))
    .filter((row) => !peerFilter || row.peer === peerFilter);
  const label = (address) => known.get(address)?.label ?? address;
  const order = (a, b) => (sortBy === "name" ? label(a.address).localeCompare(label(b.address)) : b.last - a.last || label(a.address).localeCompare(label(b.address)));
  const personItem = (row) => {
    const party = known.get(row.address) ?? { label: row.address, live: false };
    const sub = party.session ? [party.session.project, party.session.agent, party.session.status].join(" - ") : "not live";
    const active = selected?.kind === "dm" && selected.address === row.address;
    return sidebarItem(party.label, sub, row.address, active, () => select({ kind: "dm", address: row.address }), row.last || null, null, sessionBadges(row.address, known, { localPeer: false }), `chat:${row.address}`);
  };
  const channelItem = (channel) => {
    const live = channelMembers(channel.project).length;
    const active = selected?.kind === "channel" && selected.project === channel.project;
    const def = channelSettings[channel.project]?.default_recipient;
    const sub = `${live} live session${live === 1 ? "" : "s"}${def ? `, default @${def}` : ""}`;
    return sidebarItem(`#${channel.project}`, sub, `#${channel.project}`, active, () => select({ kind: "channel", project: channel.project }), channel.last || null, "#", [], `channel:${channel.project}`);
  };

  const container = $("projects");
  container.replaceChildren();
  $("dm-empty").hidden = rows.length > 0 || channels.length > 0;
  if (groupBy === "peer") {
    // Channels span laptops; then each laptop's sessions.
    const sortedChannels = channels.slice().sort((a, b) => (sortBy === "name" ? a.project.localeCompare(b.project) : b.last - a.last || a.project.localeCompare(b.project)));
    if (sortedChannels.length) {
      container.append(element("h2", "chat__heading", "Channels"), list(sortedChannels.map(channelItem)));
    }
    for (const peer of peers.filter((p) => !peerFilter || p === peerFilter)) {
      const members = rows.filter((row) => row.peer === peer).sort(order);
      if (!members.length) continue;
      container.append(element("h2", "chat__heading", peer === selfName ? `${peer} (this laptop)` : peer), list(members.map(personItem)));
    }
  } else {
    const projects = new Map();
    for (const channel of channels) projects.set(channel.project, { name: channel.project, channel, people: [], last: channel.last });
    for (const row of rows) {
      if (!projects.has(row.project)) projects.set(row.project, { name: row.project, channel: null, people: [], last: 0 });
      const entry = projects.get(row.project);
      entry.people.push(row);
      entry.last = Math.max(entry.last, row.last);
    }
    const ordered = [...projects.values()].sort((a, b) => (sortBy === "name" ? a.name.localeCompare(b.name) : b.last - a.last || a.name.localeCompare(b.name)));
    for (const entry of ordered) {
      const items = [];
      if (entry.channel) items.push(channelItem(entry.channel));
      items.push(...entry.people.sort(order).map(personItem));
      container.append(element("h2", "chat__heading", entry.name), list(items));
    }
  }

  // Conversations between agents, grouped by the agent that started each.
  const groups = new Map();
  for (const entry of agents) {
    const first = entry.messages.slice().sort((a, b) => a.created_at - b.created_at)[0];
    const source = address(first.sender);
    if (peerFilter && !entry.members.some((m) => peerOf(m, known) === peerFilter)) continue;
    if (!groups.has(source)) groups.set(source, { source, label: first.sender_label, entries: [], last: 0 });
    const group = groups.get(source);
    group.entries.push(entry);
    group.last = Math.max(group.last, entry.last);
  }
  const agentGroups = $("agent-groups");
  agentGroups.replaceChildren();
  $("agents-heading").hidden = groups.size === 0;
  for (const group of [...groups.values()].sort((a, b) => b.last - a.last)) {
    agentGroups.append(element("h3", "chat__group", `from ${group.label}`));
    const items = group.entries.map((entry) => {
      const other = entry.members.find((m) => m !== group.source) ?? group.source;
      const active = selected?.kind === "pair" && selected.key === entry.key;
      return sidebarItem(`to ${label(other)}`, `${entry.messages.length} message${entry.messages.length === 1 ? "" : "s"}`, other, active, () => select({ kind: "pair", key: entry.key }), entry.last, null, [], `pair:${entry.key}`);
    });
    agentGroups.append(list(items));
  }
}

function select(next) {
  kickoffNext = false;
  selected = next;
  shownThread = "";
  renderSidebar();
  renderThread();
  if (next.kind !== "pair") $("composer-text").focus();
}

function dayLabel(when) {
  const today = new Date();
  const yesterday = new Date(today.getTime() - 86400000);
  if (when.toDateString() === today.toDateString()) return "Today";
  if (when.toDateString() === yesterday.toDateString()) return "Yesterday";
  return when.toLocaleDateString([], { weekday: "long", month: "long", day: "numeric" });
}

function renderThread() {
  const { user, dms, agents, channels } = conversations();
  const known = parties();
  let entry = null;
  let title = "Pick a session to start a conversation";
  let subtitle = "";
  if (selected?.kind === "dm") {
    entry = dms.get(selected.address) ?? null;
    const party = known.get(selected.address);
    title = party?.label ?? selected.address;
    subtitle = party?.session ? sessionLine(party.session) : "Not live: messages wait until it returns";
  } else if (selected?.kind === "channel") {
    entry = channels.find((c) => c.project === selected.project) ?? null;
    title = `#${selected.project}`;
    const members = channelMembers(selected.project);
    subtitle = members.length
      ? members.map((m) => (m.host ? `${m.name} on ${m.host}` : m.name)).join(", ")
      : "No live sessions; posts need someone to mention";
  } else if (selected?.kind === "pair") {
    entry = agents.find((a) => a.key === selected.key) ?? null;
    title = entry ? entry.members.map((m) => known.get(m)?.label ?? m).join(" / ") : "Conversation";
    subtitle = "Between agents; read only";
  }
  const messages = groupPosts((entry?.messages ?? []).slice().sort(chronological));
  const key = JSON.stringify([selected, title, subtitle, messages.map((m) => [m.message_id, m.deliveries.map((d) => d.delivery)])]);
  $("composer").hidden = !selected || selected.kind === "pair";
  const def = selected?.kind === "channel" ? channelSettings[selected.project]?.default_recipient : null;
  $("composer-text").placeholder = selected?.kind === "channel"
    ? (def ? `Message ${title}: goes to @${def} unless you mention someone` : `Message ${title}: mention @channel or @name`)
    : selected?.kind === "dm" ? `Message ${title}` : "";
  renderDefault();
  renderMentions();
  renderSessionActions(known);
  renderWaiting(known);
  if (key === shownThread) return;
  shownThread = key;
  const switched = JSON.stringify(selected) !== shownSelection;
  shownSelection = JSON.stringify(selected);

  $("thread-title").textContent = title;
  $("thread-subtitle").textContent = subtitle;
  $("thread-badges").replaceChildren(...(selected?.kind === "dm" ? sessionBadges(selected.address, known, { localPeer: false }) : []));
  const thread = $("thread");
  const atBottom = thread.scrollHeight - thread.scrollTop - thread.clientHeight < 40;
  thread.replaceChildren();
  if (selected && messages.length === 0) {
    const start = { dm: `This is the start of your conversation with ${title}.`, channel: `This is the start of ${title}. Mention @channel to reach every session, or @name for one.`, pair: "No messages." };
    thread.append(element("li", "thread__empty", start[selected.kind]));
  }
  let previous = null;
  let day = "";
  for (const message of messages) {
    const when = new Date(message.created_at * 1000);
    const label = dayLabel(when);
    if (label !== day) {
      day = label;
      thread.append(element("li", "thread__day", label));
      previous = null;
    }
    const from = address(message.sender);
    const fromYou = from === user || mine(message);
    const continued = previous && previous.from === from && message.created_at - previous.at < 300;
    const item = element("li", `msg${continued ? " msg--continued" : ""}`);
    item.append(continued ? element("span", "msg__gutter", when.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })) : avatar(message.sender_label, fromYou ? "you" : from));
    const content = element("div", "msg__content");
    const head = element("div", "msg__head");
    if (!continued) {
      head.append(element("span", "msg__name", fromYou ? "you" : message.sender_label));
      if (!fromYou && message.sender.provider !== "user") head.append(...sessionBadges(from, known));
      head.append(element("time", "msg__time", when.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })));
    }
    if (message.scheduled) head.append(element("span", "msg__tag", "scheduled"));
    const multiple = message.deliveries.length > 1 || selected?.kind !== "dm";
    if (selected?.kind === "pair" && !continued) head.append(element("span", "msg__to", `to ${message.recipient_label}`));
    for (const delivery of message.deliveries) {
      const state = delivery.delivery.state + (delivery.delivery.host ? ` to ${delivery.delivery.host}` : "");
      const text = multiple && fromYou ? `${delivery.label}: ${state}` : state;
      const badge = element("span", `state state--${tones[delivery.delivery.state] ?? "muted"} msg__state`, text);
      if (delivery.delivery.reason) badge.title = delivery.delivery.reason;
      head.append(badge);
    }
    content.append(head, blockNodes(message.blocks, element("div", "msg__body md")));
    item.append(content);
    thread.append(item);
    previous = { from, at: message.created_at };
  }
  if (atBottom || switched) thread.scrollTop = thread.scrollHeight;
}

// The bridge labels every address of this laptop's user (direct and channel) "you".
function mine(message) {
  return message.sender_label === "you";
}

// Oldest first; within one second (timestamps are whole seconds) the user's
// messages come before the answers they caused.
function chronological(a, b) {
  return a.created_at - b.created_at || Number(mine(b)) - Number(mine(a));
}

// One channel post reaches several sessions as several messages; show it once.
function groupPosts(messages) {
  const out = [];
  const posts = new Map();
  for (const m of messages) {
    const delivery = { label: m.recipient_label, delivery: m.delivery };
    const key = m.channel ? `${address(m.sender)}|${m.message}` : null;
    const post = key ? posts.get(key) : null;
    if (post && m.created_at - post.created_at <= 5) {
      post.deliveries.push(delivery);
      continue;
    }
    const item = { ...m, deliveries: [delivery] };
    out.push(item);
    if (key) posts.set(key, item);
  }
  return out;
}

// Starting and closing sessions (REQ-028), and the kickoff template.
const KICKOFF = "Kick off: report the status of your issues, then a table of your queued work in priority order.";
const REBALANCE = "Rebalance: using the team table below, rebalance the work across the team (owner, scope and dependencies of each task), send each session its updated assignment, and report the new assignment table (session, role, task, status).";
let spawnSettings = { short_names: {}, terminal: "terminal", claude: { approvals: "ask", flags: "" }, codex: { approvals: "ask", flags: "" }, preview: {} };

function renderSessionActions(known) {
  const project = selected?.kind === "channel" ? selected.project : null;
  const party = selected?.kind === "dm" ? known.get(selected.address) : null;
  const local = party?.session && !party.session.host && party.session.agent !== "user";
  $("spawn-open").hidden = !project;
  if (!project) $("spawn-form").hidden = true;
  $("close-session").hidden = !local;
  const chat = selected?.kind === "dm" || selected?.kind === "channel";
  $("composer-kickoff").hidden = !chat;
  $("composer-rebalance").hidden = !chat;
  // A live session with a role can be sent its instructions again.
  $("send-role").hidden = !(party?.session && roleBook.assignments[party.session.name]);
  if (!$("spawn-form").hidden) renderSpawnPreview();
}

function shortName(project) {
  const own = spawnSettings.short_names[project];
  if (own) return own;
  const cleaned = project.replace(/[^A-Za-z0-9._-]/g, "-").replace(/^[-._]+|[-._]+$/g, "").slice(0, 32);
  return cleaned || "project";
}

function renderSpawnPreview() {
  const project = selected?.project;
  const role = $("spawn-role").value;
  if (!project || !role) {
    $("spawn-preview").textContent = "Add a role on the Roles tab first.";
    return;
  }
  const short = $("spawn-short").value.trim() || shortName(project);
  const taken = new Set(sessions.filter((s) => s.project === project).map((s) => s.name.toLowerCase()));
  let n = 1;
  while (taken.has(`${short}-${role}-${n}`.toLowerCase())) n += 1;
  $("spawn-preview").textContent = `Opens ${short}-${role}-${n} in a terminal on this laptop, in the directory of #${project}, with the ${role} instructions (and yours, if any) as its first prompt. It runs as set on the Run tab.`;
}

async function openSpawnForm() {
  try {
    spawnSettings = await call({ method: "spawn_settings" });
  } catch (error) {
    showError(error);
  }
  const roleSelect = $("spawn-role");
  const pick = roleSelect.value;
  roleSelect.replaceChildren(...roleBook.roles.map((r) => new Option(r.name, r.name)));
  if ([...roleSelect.options].some((o) => o.value === pick)) roleSelect.value = pick;
  $("spawn-short").value = shortName(selected.project);
  $("spawn-form").hidden = false;
  renderSpawnPreview();
}

async function startSession(event) {
  event.preventDefault();
  const project = selected?.project;
  if (!project) return;
  try {
    const short = $("spawn-short").value.trim();
    if (short !== shortName(project)) {
      spawnSettings = await call({ method: "save_spawn_settings", project, short_name: short || null });
    }
    const note = $("spawn-note").value.trim();
    const reply = await call({ method: "spawn", project, agent: $("spawn-agent").value, role: $("spawn-role").value, note: note || null });
    $("spawn-form").hidden = true;
    $("spawn-note").value = "";
    $("composer-hint").textContent = `Starting ${reply.name} (${reply.agent}, ${reply.role}); it appears here once it is running.`;
    showError(null);
    refreshRoles();
  } catch (error) {
    showError(error);
  }
}

async function closeSession() {
  const party = parties().get(selected?.address);
  const session = party?.session;
  if (!session) return;
  if (!confirm(`Close ${session.name}? Its process ends; work it has not saved is lost.`)) return;
  try {
    const reply = await call({ method: "close", session_id: session.session_id });
    $("composer-hint").textContent = `Closed ${reply.name}.`;
    showError(null);
    await refreshSessions();
  } catch (error) {
    showError(error);
  }
}

// The Kickoff and Rebalance buttons mark the next send (direct or channel) to
// carry the team table (REQ-031).
let kickoffNext = false;

function insertShortcut(text) {
  kickoffNext = true;
  const input = $("composer-text");
  input.value = input.value.trim() ? `${input.value.trimEnd()}\n\n${text}` : text;
  input.focus();
  input.setSelectionRange(input.value.length, input.value.length);
}

// Sends a session its role's instructions again, after asking (REQ-026).
async function sendRole() {
  const party = parties().get(selected?.address);
  const name = party?.session?.name;
  const role = name && roleBook.assignments[name];
  if (!role) return;
  if (!confirm(`Send the ${role} instructions to ${name} again? They arrive as your message.`)) return;
  try {
    const reply = await call({ method: "assign_role", session_id: party.session.session_id, role });
    afterRoles(reply);
    $("composer-hint").textContent = `Sent the ${role} instructions to ${name}. Your words, never an approval. Cmd+Enter sends.`;
    showError(null);
    shownThread = "";
    await refresh();
  } catch (error) {
    showError(error);
  }
}

// The channel's default recipient: posts without a mention go to it.
function renderDefault() {
  const box = $("default-box");
  const project = selected?.kind === "channel" ? selected.project : null;
  box.hidden = !project;
  if (!project) return;
  const names = channelMembers(project).map((m) => m.name);
  const current = channelSettings[project]?.default_recipient ?? "";
  if (current && !names.includes(current)) names.push(current);
  const key = JSON.stringify([project, names, current]);
  const select = $("default-select");
  if (select.dataset.key === key) return;
  select.dataset.key = key;
  select.replaceChildren(new Option("None: mention someone", ""));
  for (const name of names) {
    const live = channelMembers(project).some((m) => m.name === name);
    select.append(new Option(live ? `@${name}` : `@${name} (not live)`, name));
  }
  select.value = current;
}

async function changeDefault() {
  if (selected?.kind !== "channel") return;
  const name = $("default-select").value || null;
  try {
    const reply = await call({ method: "channel_default", project: selected.project, name });
    channelSettings = reply.channels;
    shownSidebar = "";
    shownThread = "";
    renderChat();
    showError(null);
  } catch (error) {
    showError(error);
    $("default-select").dataset.key = "";
    renderDefault();
  }
}

function renderMentions() {
  const box = $("composer-mentions");
  const project = selected?.kind === "channel" ? selected.project : null;
  const members = project ? channelMembers(project) : [];
  const roles = [...new Set(members.map((m) => roleBook.assignments[m.name]).filter(Boolean))];
  const names = project ? [...new Set(["channel", ...roles, ...members.map((m) => m.name)])] : [];
  const key = names.join("|");
  if (box.dataset.key === key) return;
  box.dataset.key = key;
  box.replaceChildren();
  box.hidden = names.length === 0;
  for (const name of names) {
    const chip = element("button", "chip", `@${name}`);
    chip.type = "button";
    chip.addEventListener("click", () => insertMention(name));
    box.append(chip);
  }
}

function insertMention(name) {
  const input = $("composer-text");
  const at = input.selectionStart ?? input.value.length;
  const before = input.value.slice(0, at);
  const text = `${before && !/\s$/.test(before) ? " " : ""}@${name} `;
  input.setRangeText(text, at, input.selectionEnd ?? at, "end");
  input.focus();
}

function renderChat() {
  // A conversation whose session ended is no longer shown or selected.
  const idle = selected?.kind === "channel" && sessionsLoaded && channelMembers(selected.project).length === 0;
  if (idle || (selected?.kind === "dm" && isEnded(selected.address)) || (selected?.kind === "pair" && selected.key.split("|").some(isEnded))) {
    selected = null;
  }
  if (!selected && sessionsLoaded) {
    // Open your latest conversation, else the most active channel.
    const { dms, channels } = conversations();
    const latest = [...dms.entries()].sort((a, b) => b[1].last - a[1].last)[0];
    if (latest) selected = { kind: "dm", address: latest[0] };
    else if (channels.length) selected = { kind: "channel", project: channels[0].project };
  }
  renderSidebar();
  renderThread();
}

async function refresh() {
  try {
    const [lan, messages, channels] = await Promise.all([call({ method: "lan" }), call({ method: "messages" }), call({ method: "channels" })]);
    channelSettings = channels.channels;
    renderSelf(lan.lan.network);
    renderIncoming(lan.lan.pairing.incoming);
    renderHosts(lan.lan.hosts);
    mail = messages;
    renderChat();
    showError(null);
  } catch (error) {
    showError(error);
  }
}

async function refreshSessions() {
  if (sessionsBusy) return;
  sessionsBusy = true;
  try {
    const reply = await call({ method: "list", sender: null, project: null });
    sessions = reply.sessions;
    sessionsLoaded = true;
    unavailable = reply.unavailable_hosts ?? [];
    renderChat();
    renderSchedules();
    renderRoles();
  } catch (error) {
    showError(error);
  } finally {
    sessionsBusy = false;
  }
}

// A send keeps its client_id until it succeeds, so pressing Send again after
// a lost answer cannot post the same message twice (the bridge answers a
// repeat with the first reply).
let pendingSend = null;
// The key covers everything the request carries, since the bridge refuses a
// reused id with a different body (HTTP 409).
function sendId(text) {
  const key = JSON.stringify([selected, text, kickoffNext]);
  if (pendingSend?.key !== key) pendingSend = { key, id: crypto.randomUUID() };
  return pendingSend.id;
}

async function sendMessage(event) {
  event.preventDefault();
  if (selected?.kind !== "dm" && selected?.kind !== "channel") return;
  const text = $("composer-text").value;
  if (!text.trim()) return;
  try {
    let sent;
    if (selected.kind === "channel") {
      const reply = await call({ method: "channel_send", project: selected.project, message: text, team_report: kickoffNext, client_id: sendId(text) });
      sent = `Posted${kickoffNext ? " with the team table" : ""} to ${reply.receipts.length} session${reply.receipts.length === 1 ? "" : "s"}`
        + (reply.failed.length ? `; not reached: ${reply.failed.join("; ")}` : "");
      kickoffNext = false;
    } else {
      const reply = await call({ method: "user_send", session_id: selected.address, message: text, team_report: kickoffNext, client_id: sendId(text) });
      sent = `Sent${kickoffNext ? " with the team table" : ""}: ${reply.receipt.delivery.state}`;
      kickoffNext = false;
    }
    $("composer-text").value = "";
    pendingSend = null;
    $("composer-hint").textContent = `${sent}. Your words, never an approval. Cmd+Enter sends.`;
    showError(null);
    shownThread = "";
    await refresh();
    $("thread").scrollTop = $("thread").scrollHeight;
  } catch (error) {
    showError(error);
  }
}

// Scheduled messages: the bridge's own crontab lines (REQ-025).
let schedules = { schedules: [], other_lines: [], available: true };
let editingSchedule = null;
let shownSchedules = "";

function describeCron(cron) {
  const every = cron.match(/^\*\/(\d+) \* \* \* \*$/);
  if (every) return `every ${every[1]} minutes`;
  const daily = cron.match(/^(\d+) (\d+) \* \* \*$/);
  if (daily) return `daily at ${daily[2].padStart(2, "0")}:${daily[1].padStart(2, "0")}`;
  const weekdays = cron.match(/^(\d+) (\d+) \* \* 1-5$/);
  if (weekdays) return `weekdays at ${weekdays[2].padStart(2, "0")}:${weekdays[1].padStart(2, "0")}`;
  return { "@hourly": "every hour", "@daily": "every day", "@midnight": "every day", "@weekly": "every week", "@monthly": "every month", "@yearly": "every year", "@annually": "every year", "@reboot": "at login" }[cron] ?? cron;
}

function scheduleTarget(to) {
  if (!to) return "channel default";
  return to.toLowerCase() === "channel" ? "@channel" : `@${to}`;
}

function projectNames() {
  const names = new Set(sessions.map((s) => s.project));
  for (const name of Object.keys(channelSettings)) names.add(name);
  for (const entry of schedules.schedules) names.add(entry.project);
  return [...names].sort();
}

function fillScheduleForm() {
  const project = $("schedule-project");
  const chosen = project.value || editingSchedule?.project || projectNames()[0] || "";
  project.replaceChildren(...projectNames().map((name) => new Option(`#${name}`, name)));
  project.value = chosen;
  const to = $("schedule-to");
  const current = to.value;
  const def = channelSettings[chosen]?.default_recipient;
  to.replaceChildren(new Option(def ? `channel default (@${def})` : "channel default / mentions", ""), new Option("@channel (everyone)", "channel"));
  for (const member of channelMembers(chosen)) to.append(new Option(`@${member.name}`, member.name));
  if ([...to.options].some((o) => o.value === current)) to.value = current;
}

function renderSchedules() {
  const key = JSON.stringify([schedules, sessions.map((s) => [s.project, s.name])]);
  if (key === shownSchedules) return;
  shownSchedules = key;
  $("schedule-form").hidden = !schedules.available;
  $("schedules-note").textContent = !schedules.available
    ? "Scheduling uses crontab, which this system does not have."
    : schedules.problem ? `${schedules.problem}. Schedules are shown read-only.` : "";
  const body = $("schedules-body");
  body.replaceChildren();
  $("schedules").hidden = schedules.schedules.length === 0;
  $("schedules-empty").hidden = schedules.schedules.length > 0;
  for (const entry of schedules.schedules) {
    const row = document.createElement("tr");
    const when = cell(describeCron(entry.cron));
    when.title = entry.cron;
    if (!entry.installed) when.append(element("span", "state state--warn schedule__missing", " not in crontab"));
    const plain = entry.message.replace(/(\*\*|__|`|~~)/g, "").replace(/\s+/g, " ").trim();
    const preview = cell(plain.length > 90 ? `${plain.slice(0, 90)}...` : plain);
    preview.title = entry.message;
    row.append(when, cell(`#${entry.project}`), cell(scheduleTarget(entry.to)), preview);
    // What the last run did (REQ-025): posted, or skipped because the previous post is unread.
    const run = entry.last_run;
    const runText = run ? `${new Date(run.at * 1000).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" })}: ${run.summary}` : "not run yet";
    const runCell = run && /skipped|failed/.test(run.summary) ? stateCell(runText, "warn") : cell(runText);
    runCell.classList.add("schedule__run");
    row.append(runCell);
    const actions = document.createElement("td");
    row.dataset.qa = `schedule:${entry.id}`;
    for (const [label, handler, tone, qa] of [["Run now", () => runSchedule(entry), "quiet", "schedule-run"], ["Edit", () => openNavItem("cron", `schedule:${entry.id}`, () => editSchedule(entry)), "quiet", "schedule-edit"], ["Delete", () => deleteSchedule(entry), "danger", "schedule-delete"]]) {
      const button = element("button", `button button--${tone} schedule__action`, label);
      button.type = "button";
      button.dataset.qa = qa;
      button.addEventListener("click", handler);
      actions.append(button);
    }
    row.append(actions);
    body.append(row);
  }
  $("crontab-other").textContent = schedules.other_lines.length ? schedules.other_lines.join("\n") : "(none)";
  fillScheduleForm();
  renderCronNav();
}

async function refreshSchedules() {
  try {
    schedules = await call({ method: "schedules" });
    renderSchedules();
  } catch (error) {
    showError(error);
  }
}

function resetScheduleForm() {
  editingSchedule = null;
  $("schedule-cron").value = "";
  $("schedule-message").value = "";
  $("schedule-team").checked = false;
  $("schedule-save").textContent = "Add schedule";
  $("schedule-cancel").hidden = true;
}

function editSchedule(entry) {
  editingSchedule = entry;
  $("schedule-cron").value = entry.cron;
  $("schedule-project").value = entry.project;
  fillScheduleForm();
  $("schedule-to").value = entry.to ?? "";
  $("schedule-message").value = entry.message;
  $("schedule-team").checked = Boolean(entry.team_report);
  $("schedule-save").textContent = "Save schedule";
  $("schedule-cancel").hidden = false;
  $("schedule-cron").focus();
}

async function saveSchedule(event) {
  event.preventDefault();
  try {
    schedules = await call({
      method: "save_schedule",
      id: editingSchedule?.id ?? null,
      cron: $("schedule-cron").value.trim(),
      project: $("schedule-project").value,
      to: $("schedule-to").value || null,
      message: $("schedule-message").value,
      team_report: $("schedule-team").checked,
    });
    resetScheduleForm();
    navSelection.cron = "all";
    shownSchedules = "";
    renderSchedules();
    showError(null);
  } catch (error) {
    showError(error);
  }
}

async function deleteSchedule(entry) {
  if (!confirm(`Delete the schedule "${describeCron(entry.cron)}" to ${scheduleTarget(entry.to)} in #${entry.project}? Its crontab line is removed.`)) return;
  try {
    schedules = await call({ method: "delete_schedule", id: entry.id });
    if (editingSchedule?.id === entry.id) resetScheduleForm();
    shownSchedules = "";
    renderSchedules();
  } catch (error) {
    showError(error);
  }
}

async function runSchedule(entry) {
  try {
    const reply = await call({ method: "run_schedule", id: entry.id });
    $("schedules-note").textContent = `Posted to ${reply.receipts.length} session${reply.receipts.length === 1 ? "" : "s"} in #${entry.project}.`;
    await refresh();
  } catch (error) {
    showError(error);
  }
}

// Tabs: Chats, Cron, Peers, Roles; the choice survives a reload.
function showTab(name) {
  for (const button of document.querySelectorAll(".tabs__tab")) {
    const active = button.dataset.tab === name;
    button.classList.toggle("tabs__tab--active", active);
    button.setAttribute("aria-selected", String(active));
    // One Tab stop for the tab list: the selected tab; arrows move between tabs.
    button.tabIndex = active ? 0 : -1;
    if (active) button.scrollIntoView({ block: "nearest", inline: "nearest", container: "nearest" });
  }
  for (const panel of document.querySelectorAll(".tab")) panel.hidden = panel.id !== `tab-${name}`;
  // Every tab uses the whole window: a sidebar of the tab's items and a main pane.
  document.body.classList.add("app-full");
  renderNavFor(name);
  sessionStorage.setItem("session-bridge-tab", name);
  if (name === "peers") {
    refreshUsage();
    refreshPhones();
  }
  if (name === "projects") refreshProjects();
  if (name === "run") refreshRun();
}

// Arrow keys, Home and End move focus along the tab list; Enter or Space
// selects (each tab loads its data, so focus alone does not switch).
function moveTabFocus(event) {
  const tabs = [...document.querySelectorAll(".tabs__tab")];
  const at = tabs.indexOf(document.activeElement);
  if (at < 0) return;
  const to = { ArrowRight: at + 1, ArrowLeft: at - 1, Home: 0, End: tabs.length - 1 }[event.key];
  if (to === undefined) return;
  event.preventDefault();
  const next = tabs[(to + tabs.length) % tabs.length];
  for (const tab of tabs) tab.tabIndex = tab === next ? 0 : -1;
  next.focus();
  next.scrollIntoView({ block: "nearest", inline: "nearest", container: "nearest" });
}

// Projects on this laptop (REQ-030): sessions by directory, the master, start and close.
let projectViews = [];
let shownProjects = "";
let projectsBusy = false;

async function refreshProjects() {
  if (projectsBusy || $("tab-projects").hidden) return;
  projectsBusy = true;
  try {
    projectViews = (await call({ method: "projects" })).projects;
    renderProjects();
  } catch (error) {
    showError(error);
  } finally {
    projectsBusy = false;
  }
}

function renderProjects() {
  const byId = new Map(sessions.map((s) => [s.session_id, s]));
  const key = JSON.stringify([projectViews, roleBook, [...byId.keys()]]);
  if (key === shownProjects) return;
  // Not in the middle of an input-method composition: it ends first, then
  // the tab is rebuilt (compositionend calls back here).
  if (projectComposing) return;
  shownProjects = key;
  // Re-render even while the user is choosing in a project's form (a select
  // keeps focus after a choice, which used to freeze this tab), carrying the
  // choices and the focused field over to the new form.
  const drafts = new Map();
  for (const form of document.querySelectorAll("#project-cards form.project__spawn")) {
    const fields = Object.fromEntries([...form.querySelectorAll("[data-field]")].map((f) => [f.dataset.field, f.value]));
    const active = form.contains(document.activeElement) ? document.activeElement : null;
    const caret = active && "selectionStart" in active
      ? { start: active.selectionStart, end: active.selectionEnd, direction: active.selectionDirection, scroll: active.scrollTop }
      : null;
    drafts.set(form.dataset.project, { fields, focused: active?.dataset.field ?? null, caret });
  }
  const cards = $("project-cards");
  cards.replaceChildren();
  $("projects-empty").hidden = projectViews.length > 0;
  for (const project of projectViews) {
    const card = element("article", "project");
    card.dataset.project = project.project;
    const head = element("div", "project__head");
    // The star shows once the bridge reports it (ProjectView.starred).
    if ("starred" in project) head.append(starButton(project));
    head.append(element("h3", "project__title", `#${project.project}`), element("code", "project__dir", project.dir));
    const masterBox = element("label", "project__master", "Master ");
    const masterSelect = document.createElement("select");
    const members = project.sessions.map((id) => byId.get(id)).filter(Boolean);
    masterSelect.append(new Option(project.master && !project.master_chosen ? `Automatic: ${project.master}` : "Automatic", ""));
    for (const s of members) masterSelect.append(new Option(s.name, s.name));
    masterSelect.value = project.master_chosen ? project.master : "";
    masterSelect.addEventListener("change", () => setMaster(project.project, masterSelect.value || null));
    masterBox.append(masterSelect);
    head.append(masterBox);
    card.append(head);

    const table = element("table", "project__sessions");
    const body = document.createElement("tbody");
    for (const s of members) {
      const row = document.createElement("tr");
      row.dataset.qa = `project-session:${s.session_id}`;
      const name = cell("");
      name.append(element("span", "project__name", s.name));
      const role = roleBook.assignments[s.name];
      if (role) name.append(badge(role, "role"));
      if (s.name === project.master) name.append(badge("master", "master"));
      row.append(name, cell(s.agent), cell(s.status));
      const actions = document.createElement("td");
      const close = element("button", "button button--danger schedule__action", "Close");
      close.type = "button";
      close.dataset.qa = "project-close";
      close.addEventListener("click", () => closeFromProjects(s));
      actions.append(close);
      row.append(actions);
      body.append(row);
    }
    table.append(body);
    card.append(table);

    const form = element("form", "project__spawn");
    form.dataset.project = project.project;
    const agent = document.createElement("select");
    agent.dataset.field = "agent";
    agent.append(new Option("Claude", "claude"), new Option("Codex", "codex"));
    const roleSelect = document.createElement("select");
    roleSelect.dataset.field = "role";
    for (const r of roleBook.roles) roleSelect.append(new Option(r.name, r.name));
    const start = element("button", "button", "Start session");
    start.dataset.qa = "project-start";
    start.type = "submit";
    const agentLabel = element("label", "schedule-form__field", "Agent ");
    agentLabel.append(agent);
    const roleLabel = element("label", "schedule-form__field", "Role ");
    roleLabel.append(roleSelect);
    const note = document.createElement("textarea");
    note.dataset.field = "note";
    note.rows = 1;
    note.placeholder = "Your instructions (optional)";
    const noteLabel = element("label", "schedule-form__field project__note", "Instructions ");
    noteLabel.append(note);
    form.append(agentLabel, roleLabel, noteLabel, start);
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      if (await startInProject(project, agent.value, roleSelect.value, note.value.trim())) note.value = "";
    });
    card.append(form);
    cards.append(card);
    const draft = drafts.get(project.project);
    if (draft) {
      for (const field of [agent, roleSelect, note]) {
        const value = draft.fields[field.dataset.field];
        // A role that no longer exists is not restored.
        if (value !== undefined && (field.tagName !== "SELECT" || [...field.options].some((o) => o.value === value))) field.value = value;
      }
      const focused = draft.focused ? form.querySelector(`[data-field="${draft.focused}"]`) : null;
      if (focused) {
        focused.focus();
        // The caret and scroll stay where the user was typing.
        if (draft.caret && "setSelectionRange" in focused) {
          focused.setSelectionRange(draft.caret.start, draft.caret.end, draft.caret.direction ?? "none");
          focused.scrollTop = draft.caret.scroll;
        }
      }
    }
  }
  renderProjectsNav();
}

// An input-method composition in a project's form holds the tab's rebuild.
// It ends with compositionend, or, should a browser never send that, when
// focus leaves the form or after 30 s, so the tab cannot stay frozen.
let projectComposing = false;
let composingTimer = null;
function endProjectComposing() {
  if (!projectComposing) return;
  projectComposing = false;
  clearTimeout(composingTimer);
  renderProjects();
}
document.addEventListener("compositionstart", (event) => {
  if (!event.target.closest?.("#project-cards form")) return;
  projectComposing = true;
  clearTimeout(composingTimer);
  composingTimer = setTimeout(endProjectComposing, 30000);
}, true);
document.addEventListener("compositionend", endProjectComposing, true);
document.addEventListener("focusout", (event) => {
  // After focus has moved, so the rebuild does not pull it back.
  if (projectComposing && event.target.closest?.("#project-cards form")) setTimeout(endProjectComposing, 0);
}, true);

async function setMaster(project, name) {
  try {
    projectViews = (await call({ method: "project_master", project, name })).projects;
    shownProjects = "";
    renderProjects();
    $("projects-note").textContent = name ? `${name} is the master of #${project}.` : `#${project} chooses its master automatically.`;
    showError(null);
  } catch (error) {
    showError(error);
  }
}

async function startInProject(project, agent, role, note) {
  if (!role) {
    showError(new Error("Add a role on the Roles tab first."));
    return false;
  }
  try {
    const reply = await call({ method: "spawn", project: project.dir, agent, role, note: note || null });
    $("projects-note").textContent = `Starting ${reply.name} (${reply.agent}, ${reply.role}) in #${reply.project}.`;
    showError(null);
    document.activeElement?.blur?.();
    await refreshRoles();
    shownProjects = "";
    await refreshSessions();
    await refreshProjects();
    return true;
  } catch (error) {
    showError(error);
    return false;
  }
}

async function closeFromProjects(session) {
  if (!confirm(`Close ${session.name}? Its process ends; work it has not saved is lost.`)) return;
  try {
    const reply = await call({ method: "close", session_id: session.session_id });
    $("projects-note").textContent = `Closed ${reply.name}.`;
    showError(null);
    await refreshSessions();
    shownProjects = "";
    await refreshProjects();
  } catch (error) {
    showError(error);
  }
}

// Roles (REQ-026): names and instructions; assigning sends them to a session.
let editingRole = null;
let shownRoles = "";

function renderRoles() {
  const key = JSON.stringify([roleBook, sessions.map((s) => [s.session_id, s.name, s.project, s.host])]);
  if (key === shownRoles) return;
  shownRoles = key;
  const body = $("roles-body");
  body.replaceChildren();
  $("roles").hidden = roleBook.roles.length === 0;
  $("roles-empty").hidden = roleBook.roles.length > 0;
  for (const role of roleBook.roles) {
    const holders = Object.entries(roleBook.assignments).filter(([, r]) => r === role.name).map(([name]) => name);
    const row = document.createElement("tr");
    const name = cell("");
    name.append(badge(role.name, "role"));
    // Where the role lives (REQ-037): its Markdown file, editable anywhere.
    const path = roleBook.files?.[role.name]?.path;
    if (path) {
      const file = element("span", "roles__path", path.split("/").pop());
      file.title = path;
      name.append(file);
    }
    const plain = role.prompt.replace(/(\*\*|__|`|~~)/g, "").replace(/\s+/g, " ").trim();
    const prompt = cell(plain.length > 110 ? `${plain.slice(0, 110)}...` : plain);
    prompt.title = role.prompt;
    row.append(name, prompt, cell(holders.length ? holders.join(", ") : "nobody yet"));
    const actions = document.createElement("td");
    row.dataset.qa = `role:${role.name}`;
    for (const [label, handler, tone, qa] of [["Edit", () => openNavItem("roles", `role:${role.name}`, () => editRole(role)), "quiet", "role-edit"], ["Delete", () => deleteRole(role), "danger", "role-delete"]]) {
      const button = element("button", `button button--${tone} schedule__action`, label);
      button.type = "button";
      button.dataset.qa = qa;
      button.addEventListener("click", handler);
      actions.append(button);
    }
    row.append(actions);
    body.append(row);
  }
  $("roles-folder").textContent = roleBook.folder ? `Each role is a Markdown file in ${roleBook.folder}. Edit one in any editor and the bridge picks up the change within a few seconds; sessions holding the role get the new text only when you send it again.` : "";
  $("roles-open-folder").hidden = !roleBook.folder;
  const skipped = roleBook.not_loaded ?? [];
  $("roles-not-loaded").hidden = skipped.length === 0;
  $("roles-not-loaded-list").replaceChildren(...skipped.map((n) => element("li", "", `${n.file}: ${n.reason}`)));
  renderRolesNav();
  const sessionSelect = $("assign-session");
  const chosen = sessionSelect.value;
  sessionSelect.replaceChildren(...sessions.map((session) => {
    const role = roleBook.assignments[session.name];
    const text = `${session.name} (${session.project}) - ${session.host ?? selfName}${role ? ` - now ${role}` : ""}`;
    return new Option(text, session.session_id);
  }));
  if ([...sessionSelect.options].some((o) => o.value === chosen)) sessionSelect.value = chosen;
  const roleSelect = $("assign-role");
  const pick = roleSelect.value;
  roleSelect.replaceChildren(new Option("No role", ""), ...roleBook.roles.map((r) => new Option(r.name, r.name)));
  if ([...roleSelect.options].some((o) => o.value === pick)) roleSelect.value = pick;
}

async function refreshRoles() {
  try {
    roleBook = await call({ method: "roles" });
    renderRoles();
  } catch (error) {
    showError(error);
  }
}

function resetRoleForm() {
  editingRole = null;
  $("role-name").value = "";
  $("role-prompt").value = "";
  $("role-save").textContent = "Add role";
  $("role-cancel").hidden = true;
  $("role-file").textContent = "";
  $("roles-note").textContent = "";
}

function editRole(role) {
  // The file's version as read: a save after an edit elsewhere is refused.
  editingRole = { ...role, version: roleBook.files?.[role.name]?.version ?? null };
  $("role-name").value = role.name;
  $("role-prompt").value = role.prompt;
  $("role-save").textContent = "Save role";
  $("role-cancel").hidden = false;
  $("role-file").textContent = roleBook.files?.[role.name]?.path ?? "";
  $("roles-note").textContent = "";
  $("role-name").focus();
}

function afterRoles(reply) {
  roleBook = reply;
  shownRoles = "";
  shownSidebar = "";
  shownThread = "";
  renderRoles();
  renderChat();
  showError(null);
}

async function saveRole(event) {
  event.preventDefault();
  try {
    afterRoles(await call({ method: "save_role", name: $("role-name").value.trim(), prompt: $("role-prompt").value, previous: editingRole?.name ?? null, version: editingRole?.version ?? null }));
    resetRoleForm();
    openNavItem("roles", "all");
  } catch (error) {
    // Kept in the tab, with the text typed: the periodic refresh clears the banner.
    $("roles-note").textContent = `Not saved: ${error.message}`;
    showError(error);
  }
}

async function deleteRole(role) {
  if (!confirm(`Delete the role ${role.name}? Sessions that hold it lose it; nothing is sent to them.`)) return;
  try {
    afterRoles(await call({ method: "delete_role", name: role.name }));
    if (editingRole?.name === role.name) resetRoleForm();
  } catch (error) {
    showError(error);
  }
}

async function assignRole(event) {
  event.preventDefault();
  const sessionId = $("assign-session").value;
  const role = $("assign-role").value || null;
  if (!sessionId) return;
  try {
    afterRoles(await call({ method: "assign_role", session_id: sessionId, role }));
    const name = sessions.find((s) => s.session_id === sessionId)?.name ?? "the session";
    $("roles-note").textContent = role ? `Sent the ${role} instructions to ${name}.` : `${name} has no role now.`;
  } catch (error) {
    showError(error);
  }
}

// Remote hosts over SSH (REQ-027).
let shownRemotes = "";
let shownRemoteList = [];

async function refreshRemotes() {
  try {
    const reply = await call({ method: "remotes" });
    const key = JSON.stringify(reply.remotes);
    if (key === shownRemotes) return;
    shownRemotes = key;
    shownRemoteList = reply.remotes;
    renderPeersNav();
    const body = $("remotes-body");
    body.replaceChildren();
    $("remotes").hidden = reply.remotes.length === 0;
    $("remotes-empty").hidden = reply.remotes.length > 0;
    for (const remote of reply.remotes) {
      const row = document.createElement("tr");
      row.dataset.qa = `remote:${remote.host}`;
      const tone = remote.state === "connected" ? "ok" : remote.state.startsWith("down") ? "bad" : "warn";
      row.append(cell(remote.host, "mono"), stateCell(remote.state, tone), cell(`here ${remote.local_port}, there ${remote.remote_port}, back ${remote.back_port}`, "mono"));
      const actions = document.createElement("td");
      for (const [label, handler, style, qa] of [["Pair", () => startPairing(remote.host), "quiet", "remote-pair"], ["Remove", () => removeRemote(remote.host), "danger", "remote-remove"]]) {
        const button = element("button", `button button--${style} schedule__action`, label);
        button.type = "button";
        button.dataset.qa = qa;
        button.addEventListener("click", handler);
        actions.append(button);
      }
      row.append(actions);
      body.append(row);
    }
  } catch (error) {
    showError(error);
  }
}

async function addRemote(event) {
  event.preventDefault();
  const host = $("remote-host").value.trim();
  const port = $("remote-port").value.trim();
  if (!host) return;
  try {
    await call({ method: "add_remote", host, port: port ? Number(port) : null });
    $("remote-host").value = "";
    $("remote-port").value = "";
    $("remotes-note").textContent = `Connecting to ${host}; pair it once the tunnel shows connected.`;
    shownRemotes = "";
    await refreshRemotes();
    showError(null);
  } catch (error) {
    showError(error);
  }
}

async function removeRemote(host) {
  if (!confirm(`Remove ${host}? Its ssh connection closes; a pairing with its bridge stays until you unpair it.`)) return;
  try {
    await call({ method: "remove_remote", host });
    shownRemotes = "";
    await refreshRemotes();
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
  $("composer").addEventListener("submit", sendMessage);
  $("default-select").addEventListener("change", changeDefault);
  $("spawn-open").addEventListener("click", openSpawnForm);
  $("spawn-cancel").addEventListener("click", () => { $("spawn-form").hidden = true; });
  $("spawn-form").addEventListener("submit", startSession);
  for (const id of ["spawn-role", "spawn-short"]) $(id).addEventListener("input", renderSpawnPreview);
  $("close-session").addEventListener("click", closeSession);
  $("composer-kickoff").addEventListener("click", () => insertShortcut(KICKOFF));
  $("composer-rebalance").addEventListener("click", () => insertShortcut(REBALANCE));
  $("send-role").addEventListener("click", sendRole);
  for (const button of document.querySelectorAll(".tabs__tab")) button.addEventListener("click", () => showTab(button.dataset.tab));
  document.querySelector(".tabs").addEventListener("keydown", moveTabFocus);
  // Leaving the tab list without choosing: Tab comes back to the selected tab.
  document.querySelector(".tabs").addEventListener("focusout", (event) => {
    if (event.currentTarget.contains(event.relatedTarget)) return;
    for (const tab of document.querySelectorAll(".tabs__tab")) tab.tabIndex = tab.getAttribute("aria-selected") === "true" ? 0 : -1;
  });
  $("projects-search").addEventListener("input", renderProjectsNav);
  showTab(sessionStorage.getItem("session-bridge-tab") || "chats");
  for (const id of ["group-by", "peer-filter", "sort-by"]) {
    $(id).addEventListener("change", () => { shownSidebar = ""; renderSidebar(); });
  }
  $("role-form").addEventListener("submit", saveRole);
  $("role-cancel").addEventListener("click", () => openNavItem("roles", "all", resetRoleForm));
  $("roles-open-folder").addEventListener("click", async () => {
    try {
      afterRoles(await call({ method: "open_roles_folder" }));
    } catch (error) {
      showError(error);
    }
  });
  $("assign-form").addEventListener("submit", assignRole);
  $("remote-form").addEventListener("submit", addRemote);
  refreshRemotes();
  setInterval(refreshRemotes, 5000);
  refreshRoles();
  // Role files can change in another editor (REQ-037).
  setInterval(refreshRoles, 5000);
  $("schedule-form").addEventListener("submit", saveSchedule);
  $("schedule-cancel").addEventListener("click", () => openNavItem("cron", "all", resetScheduleForm));
  $("schedule-project").addEventListener("change", fillScheduleForm);
  $("composer-text").addEventListener("keydown", (event) => {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) sendMessage(event);
  });
  refresh();
  refreshSessions();
  setInterval(refreshUsage, 5000);
  setInterval(refreshProjects, 5000);
  refreshWaiting();
  setInterval(refreshWaiting, 5000);
  setInterval(refresh, 2000);
  setInterval(refreshSessions, 5000);
  refreshSchedules();
  setInterval(refreshSchedules, 15000);
});

// Run tab (REQ-034): how sessions the user starts run.
let runEditing = false;
// The form takes input only once the saved settings are in it: a field
// typed in before would keep a default, and Save would write it.
let runLoaded = false;

function setRunEnabled(enabled) {
  for (const control of $("run-form").elements) control.disabled = !enabled;
}

function renderRunWarnings() {
  for (const agent of ["claude", "codex"]) {
    const bypass = $(`run-${agent}-approvals`).value === "bypass";
    $(`run-${agent}-approvals`).classList.toggle("run-agent__danger", bypass);
    $(`run-${agent}-warning`).hidden = !bypass;
  }
}

function renderRun() {
  renderRunNav();
  if (!runEditing || !runLoaded) {
    $("run-terminal").value = spawnSettings.terminal ?? "terminal";
    for (const agent of ["claude", "codex"]) {
      $(`run-${agent}-approvals`).value = spawnSettings[agent]?.approvals ?? "ask";
      $(`run-${agent}-flags`).value = spawnSettings[agent]?.flags ?? "";
    }
  }
  for (const agent of ["claude", "codex"]) {
    $(`run-${agent}-preview`).textContent = spawnSettings.preview?.[agent] ?? "";
  }
  renderRunWarnings();
}

// The first time the tab opens with settings never imported, they are
// filled from the user's shell aliases once.
let autoImportTried = false;

async function refreshRun() {
  try {
    spawnSettings = await call({ method: "spawn_settings" });
    renderRun();
    if (!runLoaded) {
      runLoaded = true;
      setRunEnabled(true);
    }
    showError(null);
    if (spawnSettings.imported === false && !autoImportTried) {
      autoImportTried = true;
      await importAliases(true);
    }
  } catch (error) {
    showError(error);
  }
}

async function importAliases(automatic = false) {
  try {
    spawnSettings = await call({ method: "import_aliases" });
    runEditing = false;
    renderRun();
    runNote(automatic ? "Filled in from your shell aliases. Change and save as you like." : "Imported from your shell aliases and saved.", false);
    showError(null);
  } catch (error) {
    runNote(`Not imported: ${error.message}`, true);
    showError(error);
  }
}

async function saveRun(event) {
  event.preventDefault();
  try {
    spawnSettings = await call({
      method: "save_spawn_settings",
      terminal: $("run-terminal").value,
      claude: { approvals: $("run-claude-approvals").value, flags: $("run-claude-flags").value.trim() },
      codex: { approvals: $("run-codex-approvals").value, flags: $("run-codex-flags").value.trim() },
    });
    runEditing = false;
    renderRun();
    runNote("Saved. Sessions you start from now on run this way.", false);
    showError(null);
  } catch (error) {
    // Kept in the tab: the page's periodic refresh clears the error banner.
    runNote(`Not saved: ${error.message}`, true);
    showError(error);
  }
}

function runNote(text, bad) {
  $("run-note").textContent = text;
  $("run-note").classList.toggle("run-note--bad", bad);
}

document.addEventListener("DOMContentLoaded", () => {
  setRunEnabled(false);
  $("run-form").addEventListener("submit", saveRun);
  $("run-import").addEventListener("click", () => importAliases(false));
  $("run-form").addEventListener("input", () => { runEditing = true; runNote("Not saved yet.", false); });
  for (const agent of ["claude", "codex"]) $(`run-${agent}-approvals`).addEventListener("change", renderRunWarnings);
});

// Every non-chat tab: a sidebar of the tab's items (`#<tab>-nav`) and a main
// pane showing the selected one. Panes are the elements with `data-pane`
// inside the tab; choosing an item shows its pane and may open it for editing.
const navSelection = { cron: "all", peers: "self", projects: "all", run: "all", roles: "all" };

function showPane(tab, pane) {
  for (const el of document.querySelectorAll(`#tab-${tab} [data-pane]`)) {
    // A pane element may belong to several panes ("all form").
    el.classList.toggle("pane--off", pane !== "all-panes" && !el.dataset.pane.split(" ").includes(pane));
  }
}

// groups: [{ heading, items: [{ key, label, sub, pane, open }] }]
function renderNav(tab, groups) {
  const nav = $(`${tab}-nav`);
  if (!nav) return;
  const keys = groups.flatMap((g) => g.items.map((i) => i.key));
  if (!keys.includes(navSelection[tab])) navSelection[tab] = keys[0];
  nav.replaceChildren();
  for (const group of groups) {
    if (group.heading && group.items.length) nav.append(element("h2", "chat__heading", group.heading));
    const ul = element("ul", "split__list");
    for (const item of group.items) {
      const li = element("li");
      const button = element("button", `split__item${item.key === navSelection[tab] ? " split__item--active" : ""}`);
      button.type = "button";
      if (item.key === navSelection[tab]) button.setAttribute("aria-current", "true");
      button.dataset.qa = `nav:${tab}:${item.key}`;
      button.append(element("span", "split__item-name", item.label));
      if (item.sub) button.append(element("span", "split__item-sub", item.sub));
      button.addEventListener("click", () => {
        navSelection[tab] = item.key;
        item.open?.();
        renderNavFor(tab);
      });
      li.append(button);
      ul.append(li);
    }
    nav.append(ul);
  }
  const chosen = groups.flatMap((g) => g.items).find((i) => i.key === navSelection[tab]);
  if (chosen) showPane(tab, chosen.pane);
}

// What a click on the sidebar item `key` does: select it, open it, show its
// pane. A table row's Edit goes through here, since its form is another pane.
function openNavItem(tab, key, open) {
  navSelection[tab] = key;
  open?.();
  renderNavFor(tab);
}

function renderNavFor(tab) {
  ({ cron: renderCronNav, peers: renderPeersNav, projects: renderProjectsNav, run: renderRunNav, roles: renderRolesNav })[tab]?.();
}

function renderCronNav() {
  const items = schedules.schedules.map((entry) => ({
    key: `schedule:${entry.id}`,
    label: describeCron(entry.cron),
    sub: `#${entry.project}, ${scheduleTarget(entry.to)}`,
    pane: "form",
    open: () => editSchedule(entry),
  }));
  renderNav("cron", [
    { items: [{ key: "all", label: "All schedules", sub: `${items.length} scheduled`, pane: "all" }] },
    { heading: "Schedules", items },
    { items: [
      { key: "new", label: "New schedule", pane: "form", open: resetScheduleForm },
      { key: "crontab", label: "Your other crontab lines", sub: "read-only", pane: "crontab" },
    ] },
  ]);
}

function renderPeersNav() {
  renderNav("peers", [
    { items: [{ key: "self", label: "This bridge", sub: selfName, pane: "self" }] },
    { heading: "Laptops", items: [
      { key: "hosts", label: "Laptops on this network", sub: `${shownHosts.length} found`, pane: "hosts" },
      ...shownHosts.map((h) => ({ key: `host:${h.host}`, label: h.host, sub: h.paired ? (h.online ? "paired, online" : "paired, offline") : "not paired", pane: "hosts" })),
    ] },
    { heading: "Remote hosts", items: [
      { key: "remotes", label: "Remote hosts over SSH", sub: `${shownRemoteList.length} configured`, pane: "remotes" },
      ...shownRemoteList.map((r) => ({ key: `remote:${r.host}`, label: r.host, sub: r.state, pane: "remotes" })),
    ] },
    { heading: "Phones", items: [
      { key: "phones", label: "Phone access", sub: phones ? (phones.enabled ? (phones.listening ? "on" : "on, not listening") : "off") : "", pane: "phones" },
      ...(phones?.devices ?? []).map((d) => ({ key: `phone:${d.id}`, label: d.name, sub: `key ${d.key}`, pane: "phones" })),
    ] },
  ]);
}

// The search box filters the sidebar and the cards by project name or folder.
function projectMatches(project, query) {
  return !query || project.project.toLowerCase().includes(query) || project.dir.toLowerCase().includes(query);
}

function renderProjectsNav() {
  const query = $("projects-search").value.trim().toLowerCase();
  const shown = projectViews.filter((p) => projectMatches(p, query));
  const count = query ? `${shown.length} of ${projectViews.length} shown` : `${projectViews.length} here`;
  renderNav("projects", [
    { items: [{ key: "all", label: "All projects", sub: count, pane: "all-panes" }] },
    { heading: "Projects", items: shown.map((p) => ({ key: `project:${p.project}`, label: `${p.starred ? "\u2605 " : ""}#${p.project}`, sub: p.dir, pane: "all-panes" })) },
  ]);
  // A chosen project shows only its card.
  const chosen = navSelection.projects.startsWith("project:") ? navSelection.projects.slice(8) : null;
  const byName = new Map(projectViews.map((p) => [p.project, p]));
  for (const card of document.querySelectorAll("#project-cards .project")) {
    card.classList.toggle("pane--off", chosen !== null && card.dataset.project !== chosen);
    const project = byName.get(card.dataset.project);
    card.classList.toggle("project--filtered", !project || !projectMatches(project, query));
  }
  const none = query !== "" && shown.length === 0 && projectViews.length > 0;
  $("projects-no-match").hidden = !none;
  $("projects-no-match").textContent = none ? `No project matches "${$("projects-search").value.trim()}".` : "";
}

function starButton(project) {
  const star = element("button", "project__star", project.starred ? "\u2605" : "\u2606");
  star.type = "button";
  star.dataset.qa = "project-star";
  star.setAttribute("aria-pressed", String(project.starred === true));
  star.setAttribute("aria-label", `Star #${project.project}`);
  star.title = project.starred ? "Starred: kept at the top. Click to unstar." : "Star to keep it at the top";
  star.addEventListener("click", () => setStar(project, !project.starred));
  return star;
}

async function setStar(project, starred) {
  try {
    projectViews = (await call({ method: "project_star", dir: project.dir, starred })).projects;
    shownProjects = "";
    renderProjects();
    showError(null);
  } catch (error) {
    showError(error);
  }
}

function renderRunNav() {
  renderNav("run", [
    { items: [{ key: "all", label: "All settings", pane: "all-panes" }] },
    { heading: "Settings", items: [
      { key: "terminal", label: "Terminal app", sub: spawnSettings.terminal === "iterm" ? "iTerm2" : "Terminal", pane: "terminal" },
      { key: "claude", label: "Claude", sub: spawnSettings.claude?.approvals ?? "ask", pane: "claude" },
      { key: "codex", label: "Codex", sub: spawnSettings.codex?.approvals ?? "ask", pane: "codex" },
    ] },
  ]);
}

function renderRolesNav() {
  renderNav("roles", [
    { items: [{ key: "all", label: "All roles", sub: `${roleBook.roles.length} roles`, pane: "all" }] },
    { heading: "Roles", items: roleBook.roles.map((role) => {
      const holders = Object.values(roleBook.assignments).filter((r) => r === role.name).length;
      return { key: `role:${role.name}`, label: role.name, sub: holders ? `held by ${holders}` : "nobody yet", pane: "form", open: () => editRole(role) };
    }) },
    { items: [
      { key: "new", label: "New role", pane: "form", open: resetRoleForm },
      { key: "assign", label: "Assign a role", pane: "assign" },
    ] },
  ]);
}

// Phone access (Peers tab): the HTTPS listener for the phone app, its
// trust profile, pairing with a one-time offer, and the paired phones.
let phones = null;
let phonesBusy = false;
let shownPhones = "";
// The LAN address whose links the pane shows; the default route's unless the user picks another.
let phoneAddress = null;

// A QR matrix (one string per row, "1" dark) as SVG with a 4-module margin.
function qrSvg(rows) {
  const ns = "http://www.w3.org/2000/svg";
  const size = rows.length + 8;
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", `0 0 ${size} ${size}`);
  svg.setAttribute("shape-rendering", "crispEdges");
  svg.setAttribute("role", "img");
  const back = document.createElementNS(ns, "rect");
  for (const [k, v] of [["width", size], ["height", size], ["fill", "#fff"]]) back.setAttribute(k, v);
  svg.append(back);
  rows.forEach((row, y) => {
    [...row].forEach((bit, x) => {
      if (bit !== "1") return;
      const r = document.createElementNS(ns, "rect");
      for (const [k, v] of [["x", x + 4], ["y", y + 4], ["width", 1], ["height", 1], ["fill", "#000"]]) r.setAttribute(k, v);
      svg.append(r);
    });
  });
  return svg;
}

function since(seconds) {
  return seconds ? new Date(seconds * 1000).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }) : "never";
}

function offerStateText(offer) {
  const s = offer.state.state;
  if (s === "waiting") return `Waiting for a phone. The code expires in ${offer.expires_in} s.`;
  if (s === "scanned") return "A phone is asking to pair.";
  if (s === "paired") return "Paired.";
  if (s === "declined") return "You declined that phone. Start again to pair another one.";
  if (s === "expired") return "The code expired. Start again to pair a phone.";
  if (s === "failed") return "A wrong code was typed; that code is used up. Start again.";
  return "";
}

function renderPhones() {
  if (!phones) return;
  const addresses = phones.addresses ?? (phones.address ? [phones.address] : []);
  if (!addresses.includes(phoneAddress)) phoneAddress = addresses[0] ?? null;
  const key = JSON.stringify([phones, phoneAddress]);
  if (key === shownPhones) return;
  shownPhones = key;
  $("phones-toggle").textContent = phones.enabled ? "Turn phone access off" : "Turn phone access on";
  $("phones-toggle").dataset.enabled = String(phones.enabled);
  if (document.activeElement !== $("phones-port")) $("phones-port").value = phones.port ?? "";
  $("phones-status").textContent = !phones.enabled ? "Phone access is off."
    : phones.problem ? `Not listening: ${phones.problem}`
    : phones.listening ? `Listening on https://${phoneAddress ?? phones.host}:${phones.port}.`
    : "Starting...";
  const ready = phones.enabled && phones.listening;
  // Several addresses (Wi-Fi and Ethernet, a VPN): the user picks the phone's network.
  $("phones-addresses").hidden = !ready || addresses.length < 2;
  $("phones-address-picks").replaceChildren(...(ready && addresses.length > 1 ? addresses.map((address) => {
    const pick = element("button", `button ${address === phoneAddress ? "" : "button--quiet"}`.trim(), address);
    pick.type = "button";
    pick.setAttribute("aria-pressed", String(address === phoneAddress));
    pick.addEventListener("click", () => { phoneAddress = address; renderPhones(); });
    return pick;
  }) : []));
  // The links for the chosen address, falling back to the first ones.
  const linkFor = (links, url, qr) => links?.find((l) => l.address === phoneAddress) ?? { url, qr };
  $("phones-step1").hidden = !ready;
  $("phones-step2").hidden = !ready;
  if (ready) {
    const profile = linkFor(phones.profile_links, phones.profile_url, phones.profile_qr);
    $("phones-profile-qr").replaceChildren(...(profile.qr ? [qrSvg(profile.qr)] : []));
    // A link only for an http(s) address; its text is the address itself.
    const link = $("phones-profile-link");
    const url = profile.url ?? "";
    link.textContent = url || "(no network address)";
    if (/^https?:\/\//.test(url)) link.href = url;
    else link.removeAttribute("href");
    $("phones-fingerprint").textContent = phones.fingerprint_short ?? "";
    $("phones-fingerprint-full").textContent = phones.fingerprint ? `Full fingerprint: ${phones.fingerprint}` : "";
  }
  const offer = phones.offer;
  $("phones-offer-box").hidden = !ready || !offer;
  if (ready && offer) {
    const open = offer.state.state === "waiting" || offer.state.state === "scanned";
    const pairing = linkFor(offer.links, offer.url, offer.qr);
    $("phones-offer-qr").replaceChildren(...(open && pairing.qr ? [qrSvg(pairing.qr)] : []));
    $("phones-offer-qr").hidden = !open;
    $("phones-offer-code").textContent = open ? offer.code : "";
    // Once the offer has ended, only its outcome is shown.
    $("phones-offer-hint").hidden = !open;
    $("phones-offer-code").hidden = !open;
    $("phones-offer-state").textContent = offerStateText(offer);
    const scanned = offer.state.state === "scanned";
    $("phones-confirm").hidden = !scanned;
    // The name is the phone's own claim; the key is what to compare.
    if (scanned) $("phones-confirm-text").textContent = `Pair the phone with key ${offer.state.key}? It calls itself "${offer.state.device_name}". Accept only if the phone you are holding shows this key.`;
  }
  const devices = phones.devices ?? [];
  $("phones-empty").hidden = devices.length > 0;
  $("phones-devices").hidden = devices.length === 0;
  const pairedNow = offer?.state.state === "paired" ? offer.state.device_id : null;
  $("phones-devices-body").replaceChildren(...devices.map((d) => {
    const row = document.createElement("tr");
    if (d.id === pairedNow) row.className = "phones__new";
    row.append(cell(d.name), cell(d.key, "mono"), cell(since(d.paired_at)), cell(d.last_seen ? `${since(d.last_seen)}${d.last_address ? `, ${d.last_address}` : ""}` : "never"), cell(String(d.sessions ?? 0)));
    const actions = document.createElement("td");
    row.dataset.qa = `phone:${d.id}`;
    const unpair = element("button", "button button--danger schedule__action", "Unpair");
    unpair.dataset.qa = "phone-unpair";
    unpair.type = "button";
    unpair.addEventListener("click", () => unpairPhone(d));
    actions.append(unpair);
    row.append(actions);
    return row;
  }));
  $("phones-hint").textContent = `Up to ${phones.max_devices ?? 5} phones. Phones reach this bridge on the local network over HTTPS with this bridge's own certificate. A paired phone writes as you, never as an approval. Unpair a phone you no longer use.`;
  renderPeersNav();
}

async function phonesCall(body, done) {
  try {
    phones = (await call(body)).phones;
    renderPhones();
    if (done) $("phones-note").textContent = done;
    showError(null);
  } catch (error) {
    $("phones-note").textContent = error.message;
    showError(error);
  }
}

async function refreshPhones() {
  if (phonesBusy || $("tab-peers").hidden) return;
  phonesBusy = true;
  try {
    phones = (await call({ method: "phones" })).phones;
    renderPhones();
  } catch (error) {
    showError(error);
  } finally {
    phonesBusy = false;
  }
}

function togglePhones(event) {
  event.preventDefault();
  const enabled = $("phones-toggle").dataset.enabled !== "true";
  const port = Number($("phones-port").value.trim());
  const body = { method: "phone_access", enabled };
  if (enabled && port) body.port = port;
  phonesCall(body, enabled ? "Phone access on." : "Phone access off.");
}

function unpairPhone(device) {
  if (!confirm(`Unpair ${device.name}? It can no longer reach this bridge until you pair it again.`)) return;
  phonesCall({ method: "phone_unpair", device_id: device.id }, `${device.name} unpaired.`);
}

function confirmPhone(accept) {
  const offer = phones?.offer;
  if (!offer) return;
  phonesCall({ method: "phone_confirm", id: offer.id, accept }, accept ? "Phone paired." : "Phone declined.");
}

document.addEventListener("DOMContentLoaded", () => {
  $("phones-switch").addEventListener("submit", togglePhones);
  $("phones-offer").addEventListener("click", () => phonesCall({ method: "phone_offer" }, "Scan the code with the phone."));
  $("phones-accept").addEventListener("click", () => confirmPhone(true));
  $("phones-decline").addEventListener("click", () => confirmPhone(false));
  // Every 6 s on the Peers tab; every 2 s while an offer waits for a phone.
  let tick = 0;
  setInterval(() => {
    tick += 1;
    const open = ["waiting", "scanned"].includes(phones?.offer?.state.state);
    if (open || tick % 3 === 0) refreshPhones();
  }, 2000);
});
