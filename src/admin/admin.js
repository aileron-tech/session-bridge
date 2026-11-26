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

// Chat: one conversation per pair of parties, like direct messages. The
// user's conversations are listed per session; conversations between agents
// are read-only.
let sessions = [];
let unavailable = [];
let mail = { user: null, messages: [] };
let selected = null; // { kind: "dm", address } | { kind: "pair", key } | { kind: "channel", project }
let shownSidebar = "";
let shownThread = "";
let shownSelection = "";
let sessionsBusy = false;
let channelSettings = {};

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
  return element("span", `avatar avatar--${key === "you" ? "you" : avatarTone(key)}`, initials(label));
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
  const channelList = [...channels.values()].sort((a, b) => b.last - a.last || a.project.localeCompare(b.project));
  return { user, dms, agents, channels: channelList };
}

function sessionLine(session) {
  return [session.project, session.host ?? "this laptop", session.agent, session.status].join(" - ");
}

function sidebarItem(label, sub, tone, active, onClick, time, mark) {
  const item = element("li");
  const button = element("button", `chat__item${active ? " chat__item--active" : ""}`);
  button.type = "button";
  button.append(mark ? element("span", "avatar avatar--channel", mark) : avatar(label, tone));
  const text = element("span", "chat__item-text");
  text.append(element("span", "chat__item-name", label), element("span", "chat__item-sub", sub));
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

function renderSidebar() {
  const { dms, agents, channels } = conversations();
  const known = parties();
  const key = JSON.stringify([[...dms].map(([a, e]) => [a, e.last]), agents.map((a) => [a.key, a.last]), channels.map((c) => [c.project, c.last]), sessions.map((s) => [s.session_id, s.status]), unavailable, selected]);
  if (key === shownSidebar) return;
  shownSidebar = key;
  $("sessions-note").hidden = unavailable.length === 0;
  $("sessions-note").textContent = `Not answering: ${unavailable.join(", ")}`;

  // One section per project: its channel, then its sessions to message directly.
  const projects = new Map();
  const project = (name) => {
    if (!projects.has(name)) projects.set(name, { name, channel: null, people: [], last: 0 });
    return projects.get(name);
  };
  for (const channel of channels) {
    const entry = project(channel.project);
    entry.channel = channel;
    entry.last = Math.max(entry.last, channel.last);
  }
  const people = new Map();
  for (const session of sessions) people.set(session.session_id, dms.get(session.session_id)?.last ?? 0);
  for (const [other, entry] of dms) people.set(other, Math.max(people.get(other) ?? 0, entry.last));
  for (const [address, last] of people) {
    const entry = project(projectOf(address, known));
    entry.people.push({ address, last });
    entry.last = Math.max(entry.last, last);
  }
  const container = $("projects");
  container.replaceChildren();
  $("dm-empty").hidden = projects.size > 0;
  const ordered = [...projects.values()].sort((a, b) => b.last - a.last || a.name.localeCompare(b.name));
  for (const entry of ordered) {
    container.append(element("h2", "chat__heading", entry.name));
    const items = [];
    if (entry.channel) {
      const live = channelMembers(entry.name).length;
      const active = selected?.kind === "channel" && selected.project === entry.name;
      const def = channelSettings[entry.name]?.default_recipient;
      const sub = `${live} live session${live === 1 ? "" : "s"}${def ? `, default @${def}` : ""}`;
      items.push(sidebarItem(`#${entry.name}`, sub, `#${entry.name}`, active, () => select({ kind: "channel", project: entry.name }), entry.channel.last || null, "#"));
    }
    entry.people.sort((a, b) => b.last - a.last || (known.get(a.address)?.label ?? "").localeCompare(known.get(b.address)?.label ?? ""));
    for (const person of entry.people) {
      const party = known.get(person.address) ?? { label: person.address, live: false };
      const sub = party.session ? sessionLine(party.session) : "not live";
      const active = selected?.kind === "dm" && selected.address === person.address;
      items.push(sidebarItem(party.label, sub, person.address, active, () => select({ kind: "dm", address: person.address }), person.last || null));
    }
    container.append(list(items));
  }

  // Conversations between agents, grouped by the agent that started each.
  const groups = new Map();
  for (const entry of agents) {
    const first = entry.messages.slice().sort((a, b) => a.created_at - b.created_at)[0];
    const source = address(first.sender);
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
      const label = known.get(other)?.label ?? other;
      const active = selected?.kind === "pair" && selected.key === entry.key;
      return sidebarItem(`to ${label}`, `${entry.messages.length} message${entry.messages.length === 1 ? "" : "s"}`, other, active, () => select({ kind: "pair", key: entry.key }), entry.last);
    });
    agentGroups.append(list(items));
  }
}

function select(next) {
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
  if (key === shownThread) return;
  shownThread = key;
  const switched = JSON.stringify(selected) !== shownSelection;
  shownSelection = JSON.stringify(selected);

  $("thread-title").textContent = title;
  $("thread-subtitle").textContent = subtitle;
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
      head.append(element("time", "msg__time", when.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })));
    }
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
  const names = project ? ["channel", ...channelMembers(project).map((m) => m.name)] : [];
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
  if (!selected) {
    const { dms } = conversations();
    const latest = [...dms.entries()].sort((a, b) => b[1].last - a[1].last)[0];
    if (latest) selected = { kind: "dm", address: latest[0] };
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
    unavailable = reply.unavailable_hosts ?? [];
    renderChat();
  } catch (error) {
    showError(error);
  } finally {
    sessionsBusy = false;
  }
}

async function sendMessage(event) {
  event.preventDefault();
  if (selected?.kind !== "dm" && selected?.kind !== "channel") return;
  const text = $("composer-text").value;
  if (!text.trim()) return;
  try {
    let sent;
    if (selected.kind === "channel") {
      const reply = await call({ method: "channel_send", project: selected.project, message: text });
      sent = `Posted to ${reply.receipts.length} session${reply.receipts.length === 1 ? "" : "s"}`
        + (reply.failed.length ? `; not reached: ${reply.failed.join("; ")}` : "");
    } else {
      const reply = await call({ method: "user_send", session_id: selected.address, message: text });
      sent = `Sent: ${reply.receipt.delivery.state}`;
    }
    $("composer-text").value = "";
    $("composer-hint").textContent = `${sent}. Your words, never an approval. Cmd+Enter sends.`;
    showError(null);
    shownThread = "";
    await refresh();
    $("thread").scrollTop = $("thread").scrollHeight;
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
  $("composer-text").addEventListener("keydown", (event) => {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) sendMessage(event);
  });
  refresh();
  refreshSessions();
  setInterval(refresh, 2000);
  setInterval(refreshSessions, 5000);
});
