// The panel's page: the bench's processes, their console, and the test buttons.
// Text from the processes is only ever set with textContent.
"use strict";

const token = document.querySelector('meta[name="panel-token"]').content;
let selected = null;
let since = 0;
let knownServers = "";

async function api(method, path, body) {
  const options = { method, headers: { "X-Panel-Token": token } };
  if (body !== undefined) {
    options.headers["Content-Type"] = "application/json";
    options.body = JSON.stringify(body);
  }
  const response = await fetch(path, options);
  const data = await response.json();
  if (!response.ok) throw new Error(data.error || response.statusText);
  return data;
}

function say(text, isError) {
  const message = document.getElementById("message");
  message.textContent = text;
  message.className = isError ? "error" : "muted";
}

function el(tag, attrs, text) {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) node.setAttribute(k, v);
  if (text !== undefined) node.textContent = text;
  return node;
}

function spark(history, index, max) {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", "spark");
  svg.setAttribute("viewBox", "0 0 150 40");
  svg.setAttribute("preserveAspectRatio", "none");
  if (history.length > 1) {
    const top = Math.max(max, ...history.map((h) => h[index])) || 1;
    const points = history.map((h, i) =>
      `${(i / (history.length - 1)) * 150},${40 - (h[index] / top) * 38}`).join(" ");
    const line = document.createElementNS("http://www.w3.org/2000/svg", "polyline");
    line.setAttribute("points", points);
    line.setAttribute("fill", "none");
    line.setAttribute("stroke", index === 1 ? "var(--accent)" : "var(--ok)");
    line.setAttribute("stroke-width", "1.5");
    svg.appendChild(line);
  }
  return svg;
}

function uptime(seconds) {
  if (seconds == null) return "";
  const h = Math.floor(seconds / 3600), m = Math.floor((seconds % 3600) / 60), s = seconds % 60;
  return h ? `${h} h ${m} min` : m ? `${m} min ${s} s` : `${s} s`;
}

async function action(name, verb) {
  try {
    await api("POST", "/api/action", { name, action: verb });
    say(`${verb}: ${name}`);
  } catch (e) {
    say(e.message, true);
  }
}

function card(p) {
  const div = el("div", { class: "card" + (p.name === selected ? " selected" : "") });
  const head = el("div", { class: "row" });
  const name = el("span", { class: "name" }, p.name);
  name.addEventListener("click", () => select(p.name));
  const state = p.state === "exited" && p.exitCode != null ? `exited (${p.exitCode})` : p.state;
  head.append(name, el("span", { class: "state " + p.state }, state));
  div.append(head);
  div.append(el("div", { class: "muted" },
    `${p.kind}, port ${p.port}` + (p.pid ? `, process ${p.pid}` : "") +
    (p.uptimeSeconds != null ? `, up ${uptime(p.uptimeSeconds)}` : "")));
  const numbers = el("div", { class: "numbers" });
  numbers.append(
    el("span", {}, p.cpu == null ? "CPU -" : `CPU ${p.cpu.toFixed(1)} %`),
    el("span", {}, p.memoryMb == null ? "RAM -" : `RAM ${p.memoryMb.toFixed(0)} MB`));
  div.append(numbers, spark(p.history, 1, 100), spark(p.history, 2, 256));
  const buttons = el("div", { class: "buttons" });
  for (const verb of ["start", "stop", "restart"]) {
    const b = el("button", {}, verb[0].toUpperCase() + verb.slice(1));
    b.addEventListener("click", () => action(p.name, verb));
    buttons.append(b);
  }
  const consoleButton = el("button", {}, "Console");
  consoleButton.addEventListener("click", () => select(p.name));
  buttons.append(consoleButton);
  div.append(buttons);
  return div;
}

function select(name) {
  if (selected === name) return;
  selected = name;
  since = 0;
  document.getElementById("console").textContent = "";
  document.getElementById("console-name").textContent = name;
}

function sendButtons(processes) {
  const servers = processes.filter((p) => p.kind === "server").map((p) => p.name);
  const key = servers.join(",");
  if (key === knownServers) return;
  knownServers = key;
  const span = document.getElementById("send-buttons");
  span.textContent = "";
  for (const server of servers) {
    const b = el("button", {}, `Send to ${server}`);
    b.addEventListener("click", () => test("/api/test/send", {
      player: document.getElementById("player").value.trim(), target: server }));
    span.append(b);
  }
}

async function test(path, body) {
  const result = document.getElementById("result");
  result.textContent = "...";
  try {
    const data = await api("POST", path, body || {});
    result.textContent = data.lines.length ? data.lines.join("\n") : "(no answer within a second)";
  } catch (e) {
    result.textContent = e.message;
  }
}

async function refresh() {
  try {
    const state = await api("GET", "/api/state");
    if (selected === null && state.processes.length) select(state.processes[0].name);
    const cards = document.getElementById("cards");
    cards.textContent = "";
    for (const p of state.processes) cards.append(card(p));
    sendButtons(state.processes);
    if (selected !== null) {
      const log = await api("GET", `/api/log?name=${encodeURIComponent(selected)}&since=${since}`);
      const pre = document.getElementById("console");
      const atBottom = pre.scrollTop + pre.clientHeight >= pre.scrollHeight - 4;
      for (const line of log.lines) pre.append(line.text + "\n");
      since = log.next;
      const lines = pre.textContent.split("\n");
      if (lines.length > 2000) pre.textContent = lines.slice(-2000).join("\n");
      if (atBottom) pre.scrollTop = pre.scrollHeight;
    }
  } catch (e) {
    say(`the panel does not answer: ${e.message}`, true);
  }
}

document.getElementById("start-all").addEventListener("click", () => action("all", "start"));
document.getElementById("stop-all").addEventListener("click", () => action("all", "stop"));
document.getElementById("players").addEventListener("click", () => test("/api/test/players"));
document.getElementById("command-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const input = document.getElementById("command");
  const line = input.value;
  if (!selected || !line.trim()) return;
  input.value = "";
  try {
    await api("POST", "/api/command", { name: selected, line });
  } catch (e) {
    say(e.message, true);
  }
});

refresh();
setInterval(refresh, 1000);
