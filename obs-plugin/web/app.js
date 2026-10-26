// LiMBOCHAT — panel OBS.
//
// Panel jest „cienkim klientem”: cały stan (kanały, logowanie, emotki)
// trzyma wtyczka. Tu tylko rysujemy zdarzenia z WebSocketu i wysyłamy
// komendy. Treść z czatu jest niezaufana — ZAWSZE wstawiamy ją przez
// textContent / właściwości elementów, nigdy przez innerHTML.
"use strict";

const $ = (id) => document.getElementById(id);
const MAX_DOM_MESSAGES = 400;
const RATE_WINDOW_MS = 10_000;
const TIMEOUTS = [["1 min", 60], ["10 min", 600], ["1 h", 3600], ["24 h", 86400], ["7 dni", 604800]];
const CHIP_COLORS = { broadcaster: ["LIVE", "#c84b5a"], moderator: ["MOD", "#3f9e6e"], vip: ["VIP", "#b45cc0"], subscriber: ["SUB", "#8b6cf0"], founder: ["SUB", "#8b6cf0"], staff: ["STAFF", "#555566"] };
const ICONS = {
  trash: '<svg viewBox="0 0 24 24"><path d="M4 7h16M10 11v6M14 11v6M5 7l1 12a2 2 0 0 0 2 2h8a2 2 0 0 0 2-2l1-12M9 7V4h6v3"/></svg>',
  clock: '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="8.5"/><path d="M12 7.5V12l3 2"/></svg>',
  ban: '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="8.5"/><path d="m6 6 12 12"/></svg>',
  check: '<svg viewBox="0 0 24 24"><path d="m5 12.5 4.5 4.5L19 7.5"/></svg>',
  copy: '<svg viewBox="0 0 24 24"><rect x="8" y="8" width="12" height="12" rx="2.5"/><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"/></svg>',
  user: '<svg viewBox="0 0 24 24"><circle cx="12" cy="8" r="4"/><path d="M4 20c1.5-4 4.5-6 8-6s6.5 2 8 6"/></svg>',
  link: '<svg viewBox="0 0 24 24"><path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"/></svg>',
  x: '<svg viewBox="0 0 24 24"><path d="m6 6 12 12M18 6 6 18"/></svg>',
  logout: '<svg viewBox="0 0 24 24"><path d="M15 4h3a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-3M10 16l-4-4 4-4M6 12h10"/></svg>',
};

const state = {
  ws: null,
  auth: { state: "restoring" },
  avatar: null,
  settings: { animations: true, timestamps: true, mod_buttons: true },
  globalBadges: {},
  globalEmotes: 0,
  channels: new Map(),
  order: [],
  active: null,
  demo: false,
  lobbyLog: [],
  loginDeadline: 0,
  completion: null,
  jarvis: null,
};

// ── Połączenie z wtyczką ────────────────────────────────────────────────

let retryDelay = 300;
let offlineTimer = 0;
function connect() {
  const ws = new WebSocket(`ws://${location.host}/ws`);
  state.ws = ws;
  ws.onopen = () => { retryDelay = 300; clearTimeout(offlineTimer); $("offline").hidden = true; };
  ws.onmessage = (e) => { try { handle(JSON.parse(e.data)); } catch (err) { console.error(err); } };
  ws.onclose = () => {
    // Krótka przerwa (np. aktualizacja silnika) nie zasłania czatu.
    clearTimeout(offlineTimer);
    offlineTimer = setTimeout(() => { $("offline").hidden = false; }, state.update && state.update.state === "applying" ? 12000 : 2500);
    setTimeout(connect, retryDelay);
    retryDelay = Math.min(retryDelay * 2, 2000);
  };
}

function send(msg) {
  if (state.ws && state.ws.readyState === WebSocket.OPEN) state.ws.send(JSON.stringify(msg));
}

// ── Zdarzenia z wtyczki ─────────────────────────────────────────────────

function handle(msg) {
  switch (msg.t) {
    case "snapshot": return applySnapshot(msg);
    case "message": return addMessage(msg.m);
    case "alert_update": return onAlertUpdate(msg);
    case "history": return replaceHistory(msg.channel, msg.messages);
    case "deleted": return markDeleted(msg.channel, (m) => m.dataset.id === msg.id);
    case "user_cleared": {
      markDeleted(msg.channel, (m) => m.dataset.user === msg.user_id);
      const text = msg.timeout == null ? `${msg.login} został zbanowany` : `${msg.login} otrzymał timeout na ${duration(msg.timeout)}`;
      return system(msg.channel, text, false);
    }
    case "chat_cleared":
      markDeleted(msg.channel, () => true);
      return system(msg.channel, "Czat został wyczyszczony przez moderatora", false);
    case "connected": {
      const ch = state.channels.get(msg.channel);
      if (ch) { ch.connected = true; system(msg.channel, `Połączono z #${msg.channel}`, false); renderTabs(); renderComposer(); renderStatus(); }
      return;
    }
    case "channel_added": {
      if (!state.channels.has(msg.channel)) addChannel({ login: msg.channel, connected: state.demo, can_moderate: state.demo, emote_count: 0, badges: {}, messages: [] });
      return activate(msg.channel);
    }
    case "channel_removed": return removeChannel(msg.channel);
    case "auth": return setAuth(msg);
    case "avatar": state.avatar = msg.url; return renderAccount();
    case "mod_status": {
      const ch = state.channels.get(msg.channel);
      if (!ch) return;
      ch.canModerate = msg.can_moderate;
      if (!msg.can_moderate) system(msg.channel, "Nie masz uprawnień moderatora na tym kanale — możesz pisać, ale nie moderować.", false);
      return renderStatus();
    }
    case "badges":
      if (msg.channel == null) state.globalBadges = msg.map;
      else if (state.channels.has(msg.channel)) state.channels.get(msg.channel).badges = msg.map;
      return;
    case "emotes_changed":
      if (msg.channel == null) { state.globalEmotes = msg.count; state.channels.forEach((c) => (c.emotes = null)); }
      else if (state.channels.has(msg.channel)) { const c = state.channels.get(msg.channel); c.emoteCount = msg.count; c.emotes = null; }
      return renderStatus();
    case "emote_list": {
      const ch = state.channels.get(msg.channel);
      if (ch) { ch.emotes = msg.emotes; if (!$("picker").hidden) renderPicker(); }
      return;
    }
    case "notice": return system(msg.channel, msg.text, msg.error);
    case "settings": state.settings = msg; return applySettings();
    case "jarvis": return onJarvis(msg.ev);
    case "update": state.update = msg; return renderUpdate();
    case "obs": state.obs = msg; return renderObs();
    case "summary": return showSummary(msg);
    case "summaries": return renderSummaryList(msg.list);
    case "changelog": return showChangelog(msg.entries, false);
    case "memory": return onMemory(msg);
  }
}

function applySnapshot(s) {
  // Silnik się zaktualizował — nowa wersja ma nowy panel: przeładuj stronę.
  if (state.version && s.version !== state.version) return location.reload();
  state.version = s.version;
  state.update = s.update;
  state.demo = s.demo;
  state.settings = s.settings;
  state.globalBadges = s.global_badges;
  state.globalEmotes = s.global_emotes;
  state.avatar = s.avatar;
  // Najpierw logowanie — od niego zależy podświetlanie wzmianek w historii.
  state.auth = s.auth;
  $("chats").replaceChildren();
  state.channels.clear();
  state.order = [];
  for (const c of s.channels) addChannel(c);
  const wanted = s.settings.active && state.channels.has(s.settings.active) ? s.settings.active : state.order[0];
  state.active = null;
  if (wanted) activate(wanted, false);
  setAuth(s.auth);
  applySettings();
  state.jarvis = s.jarvis;
  renderJarvisSettings();
  renderJarvisButton();
  renderAll();
  renderUpdate();
  announceUpdate();
  state.obs = s.obs;
  state.memoryCount = s.memory_count;
  renderObs();
  renderMemorySettings();
  if (s.changelog && s.changelog.length) showChangelog(s.changelog, true);
}

// ── Aktualizacje ──
function announceUpdate() {
  const u = state.update;
  if (!u || !u.updated_from) return;
  const key = `limbochat-updated-${u.version}`;
  try {
    if (sessionStorage.getItem(key)) return;
    sessionStorage.setItem(key, "1");
  } catch { /* brak pamięci sesji — pokażemy raz na połączenie */ }
  system(null, `✨ LiMBOCHAT zaktualizowany: ${u.updated_from} → ${u.version}. OBS nie musiał się restartować.`);
}

function timeAgo(ms) {
  const min = Math.round((Date.now() - ms) / 60000);
  return min < 1 ? "przed chwilą" : min < 60 ? `${min} min temu` : `${Math.round(min / 60)} h temu`;
}

function renderUpdate() {
  const u = state.update;
  if (!u) return;
  const mode = (state.settings && state.settings.updates) || "auto";
  $("up-ver").textContent = `Wersja ${u.version}`;
  const labels = {
    disabled: "wyłączone", idle: "", checking: "sprawdzam…", up_to_date: "aktualna ✓",
    downloading: `pobieram ${u.available || ""}…`, ready: `gotowa ${u.available}`, applying: `instaluję ${u.available || ""}…`, error: "błąd",
  };
  $("up-state").textContent = labels[u.state] || "";
  $("up-state").className = `up-state ${u.state}`;
  let note = "";
  if (u.state === "disabled") note = "Ta kopia nie ma ustawionego repozytorium aktualizacji (patrz README: ustaw-aktualizacje.bat).";
  else if (u.state === "error") note = u.error || "";
  else if (u.state === "ready") note = mode === "auto" ? "Zainstaluje się sama, gdy LimboAI nic nie będzie robić." : "Kliknij „Zainstaluj teraz” — zajmie to 1–2 sekundy.";
  else if (u.checked_at) note = `Sprawdzono ${timeAgo(u.checked_at)} · ${u.repo}`;
  if (u.notes && (u.state === "ready" || u.state === "downloading")) note += `\nCo nowego: ${u.notes}`;
  $("up-note").textContent = note;
  $("up-check").disabled = !u.enabled || ["checking", "downloading", "applying"].includes(u.state);
  $("up-apply").hidden = u.state !== "ready";
  for (const b of $("up-mode").children) b.classList.toggle("on", b.dataset.mode === mode);
  $("up-mode").classList.toggle("dim", !u.enabled);
  const pill = $("update-pill");
  const showPill = u.state === "applying" || (u.state === "ready" && mode === "ask");
  pill.hidden = !showPill;
  pill.textContent = u.state === "applying" ? `⟳ Aktualizuję do ${u.available}…` : `✨ Nowa wersja ${u.available} — zainstaluj`;
  pill.disabled = u.state === "applying";
}

// ── Kanały i zakładki ───────────────────────────────────────────────────

function addChannel(c) {
  const el = document.createElement("div");
  el.className = "chat";
  el.hidden = true;
  const ch = {
    login: c.login, connected: c.connected, canModerate: c.can_moderate, emoteCount: c.emote_count,
    badges: c.badges || {}, el, unread: 0, mentioned: false, unseen: 0, atBottom: true, emotes: null, recent: [], userScrollUntil: 0,
  };
  el.addEventListener("scroll", () => onScroll(ch));
  // Tylko ruch użytkownika może wstrzymać przewijanie (nie doładowane obrazki).
  const intent = () => { ch.userScrollUntil = Date.now() + 400; };
  for (const ev of ["wheel", "pointerdown", "touchstart", "keydown"]) el.addEventListener(ev, intent, { passive: true });
  // Emotka doładowała się i wiadomość urosła — zostań na dole.
  el.addEventListener("load", () => { if (ch.atBottom) el.scrollTop = el.scrollHeight; }, true);
  $("chats").appendChild(el);
  state.channels.set(c.login, ch);
  state.order.push(c.login);
  for (const m of c.messages) appendMessage(ch, m, false);
  scrollToBottom(ch);
  renderAll();
}

function removeChannel(login) {
  const ch = state.channels.get(login);
  if (!ch) return;
  ch.el.remove();
  state.channels.delete(login);
  const index = state.order.indexOf(login);
  state.order.splice(index, 1);
  if (state.active === login) {
    state.active = null;
    const next = state.order[Math.max(0, index - 1)];
    if (next) activate(next);
  }
  renderAll();
}

function activate(login, notify = true) {
  const ch = state.channels.get(login);
  if (!ch) return;
  if (state.active && state.channels.has(state.active)) state.channels.get(state.active).el.hidden = true;
  state.active = login;
  ch.el.hidden = false;
  ch.unread = 0;
  ch.mentioned = false;
  if (ch.atBottom) scrollToBottom(ch);
  closePopovers();
  if (notify) send({ c: "activate", channel: login });
  renderAll();
}

function activeChannel() {
  return state.active ? state.channels.get(state.active) : null;
}

function renderTabs() {
  const tabs = $("tabs");
  const indicator = $("tab-indicator");
  tabs.replaceChildren(indicator);
  for (const login of state.order) {
    const ch = state.channels.get(login);
    const tab = document.createElement("button");
    tab.className = "tab";
    tab.classList.toggle("active", login === state.active);
    tab.classList.toggle("connected", ch.connected);
    tab.classList.toggle("unread", ch.unread > 0);
    tab.classList.toggle("mention", ch.mentioned);
    tab.dataset.login = login;
    const dot = document.createElement("i");
    dot.className = "dot";
    const name = document.createElement("span");
    name.textContent = `#${login}`;
    tab.append(dot, name);
    if (ch.unread > 0 && login !== state.active) {
      const badge = document.createElement("span");
      badge.className = "badge";
      badge.textContent = ch.unread > 99 ? "99+" : String(ch.unread);
      tab.append(badge);
    }
    const close = document.createElement("span");
    close.className = "close";
    close.title = "Zamknij";
    close.innerHTML = ICONS.x;
    close.addEventListener("click", (e) => { e.stopPropagation(); send({ c: "part", channel: login }); });
    tab.append(close);
    tab.addEventListener("click", () => activate(login));
    tab.addEventListener("auxclick", (e) => { if (e.button === 1) send({ c: "part", channel: login }); });
    tabs.append(tab);
  }
  moveIndicator();
}

/** Pigułka aktywnej zakładki płynnie przesuwa się i rozciąga (transition w CSS). */
function moveIndicator() {
  const indicator = $("tab-indicator");
  const active = $("tabs").querySelector(".tab.active");
  if (!active) { indicator.style.opacity = "0"; return; }
  indicator.style.opacity = "1";
  indicator.style.left = `${active.offsetLeft}px`;
  indicator.style.width = `${active.offsetWidth}px`;
  active.scrollIntoView({ block: "nearest", inline: "nearest" });
}

// ── Wiadomości ──────────────────────────────────────────────────────────

function myLogin() {
  return state.auth.state === "logged_in" ? state.auth.login.toLowerCase() : null;
}

function mentionsMe(m) {
  const me = myLogin();
  if (!me || m.login === me) return false;
  return m.raw.split(/[^\p{L}\p{N}_]+/u).some((w) => w.toLowerCase() === me);
}

function addMessage(m) {
  const ch = state.channels.get(m.channel);
  if (!ch) return;
  ch.recent.push(Date.now());
  const isActive = m.channel === state.active;
  if (!isActive) {
    ch.unread++;
    if (mentionsMe(m)) ch.mentioned = true;
    renderTabs();
  } else if (!ch.atBottom) {
    ch.unseen++;
    renderJump();
  }
  appendMessage(ch, m, true);
}

function appendMessage(ch, m, live) {
  const stick = ch.atBottom;
  ch.el.appendChild(renderMessage(ch, m, live));
  trim(ch);
  if (stick) scrollToBottom(ch);
}

function trim(ch) {
  // Gdy czytasz historię, nie usuwamy starszych wiadomości (tekst by „uciekał”).
  const limit = ch.atBottom ? MAX_DOM_MESSAGES : MAX_DOM_MESSAGES * 2;
  while (ch.el.childElementCount > limit) ch.el.firstElementChild.remove();
}

function replaceHistory(login, messages) {
  const ch = state.channels.get(login);
  if (!ch) return;
  const systemLines = [...ch.el.querySelectorAll(".msg.system")];
  ch.el.replaceChildren();
  for (const m of messages) ch.el.appendChild(renderMessage(ch, m, false));
  for (const s of systemLines.slice(-5)) ch.el.appendChild(s);
  if (ch.atBottom) scrollToBottom(ch);
}

function renderMessage(ch, m, live) {
  if (m.alert) return renderAlert(ch, m, live);
  const row = document.createElement("div");
  row.className = "msg";
  if (!live) row.style.animation = "none";
  row.dataset.id = m.id;
  row.dataset.user = m.user_id;
  row._msg = m;
  if (m.deleted) row.classList.add("deleted");
  if (m.action) row.classList.add("action");
  if (mentionsMe(m)) row.classList.add("mention");

  const time = document.createElement("span");
  time.className = "time";
  time.textContent = new Date(m.ts).toLocaleTimeString("pl-PL", { hour: "2-digit", minute: "2-digit" });
  row.append(time);

  for (const [set, version] of m.badges) {
    const badge = ch.badges[`${set}/${version}`] || state.globalBadges[`${set}/${version}`];
    if (badge) {
      const img = document.createElement("img");
      img.className = "badge-img";
      img.src = badge.url;
      img.title = badge.title;
      row.append(img);
    } else if (CHIP_COLORS[set]) {
      const chip = document.createElement("span");
      chip.className = "chip";
      chip.textContent = CHIP_COLORS[set][0];
      chip.style.background = CHIP_COLORS[set][1];
      chip.title = set;
      row.append(chip);
    }
  }

  const color = readableColor(m.color, m.login);
  const name = document.createElement("span");
  name.className = "uname";
  name.textContent = m.name;
  name.style.color = color;
  name.addEventListener("click", (e) => openUserMenu(e, ch, m, row));
  name.addEventListener("contextmenu", (e) => { e.preventDefault(); openUserMenu(e, ch, m, row); });
  row.append(name);

  if (!m.action) {
    const colon = document.createElement("span");
    colon.className = "colon";
    colon.textContent = ":";
    row.append(colon);
  }

  const me = myLogin();
  for (const seg of m.segs) {
    if (seg.k === "text") {
      const t = document.createElement("span");
      t.className = "text";
      t.textContent = ` ${seg.v} `;
      if (m.action) t.style.color = color;
      row.append(t);
    } else if (seg.k === "emote") {
      row.append(renderEmote(seg.e, seg.ov));
    } else if (seg.k === "mention") {
      const p = document.createElement("span");
      p.className = "mention-pill";
      if (me && seg.v.replace(/^@/, "").toLowerCase() === me) p.classList.add("me");
      p.textContent = seg.v;
      row.append(" ", p, " ");
    } else if (seg.k === "link") {
      const a = document.createElement("a");
      const href = /^https?:\/\//i.test(seg.v) ? seg.v : `https://${seg.v}`;
      a.textContent = seg.v;
      a.href = href;
      a.addEventListener("click", (e) => { e.preventDefault(); send({ c: "open", url: href }); });
      row.append(" ", a, " ");
    }
  }

  if (ch.canModerate || state.demo) row.append(renderModButtons(ch, m));
  appendMedia(ch, m, row);
  return row;
}

// ── Obrazki z kappa.lol prosto na czacie ────────────────────────────────

function kappaUrl(v) {
  try {
    const u = new URL(/^https?:\/\//i.test(v) ? v : `https://${v}`);
    const host = u.hostname.toLowerCase();
    if (host !== "kappa.lol" && !host.endsWith(".kappa.lol")) return null;
    u.protocol = "https:";
    return u.href;
  } catch {
    return null;
  }
}

const TRUSTED = ["broadcaster", "moderator", "vip", "subscriber", "founder"];
function mediaAllowed(m) {
  const mode = (state.settings && state.settings.media_previews) || "all";
  if (mode === "off") return false;
  if (mode === "all") return true;
  return m.login === myLogin() || m.badges.some(([set]) => TRUSTED.includes(set));
}

function appendMedia(ch, m, row) {
  if (!mediaAllowed(m)) return;
  const urls = m.segs.filter((s) => s.k === "link").map((s) => kappaUrl(s.v)).filter(Boolean).slice(0, 2);
  for (const url of urls) {
    const box = document.createElement("div");
    box.className = "media loading";
    const src = `/media?u=${encodeURIComponent(url)}`;
    const done = () => {
      box.classList.remove("loading");
      if (ch.atBottom) scrollToBottom(ch);
    };
    const img = document.createElement("img");
    img.alt = "";
    img.src = src;
    img.onload = done;
    // To nie obrazek? Może krótkie wideo (mp4/webm). Jak nie — chowamy podgląd.
    img.onerror = () => {
      const video = document.createElement("video");
      Object.assign(video, { src, muted: true, loop: true, autoplay: true, playsInline: true });
      video.onloadeddata = done;
      video.onerror = () => box.remove();
      img.replaceWith(video);
    };
    box.append(img);
    box.title = url;
    box.addEventListener("click", (e) => { e.stopPropagation(); openLightbox(box.firstElementChild, url); });
    row.append(box);
  }
}

function openLightbox(el, url) {
  const lb = $("lightbox");
  const media = el.cloneNode();
  if (media.tagName === "VIDEO") Object.assign(media, { muted: true, loop: true, autoplay: true, controls: true });
  $("lb-media").replaceChildren(media);
  $("lb-open").onclick = (e) => { e.stopPropagation(); send({ c: "open", url }); };
  lb.hidden = false;
}
function closeLightbox() {
  $("lightbox").hidden = true;
  $("lb-media").replaceChildren();
}

// ── Alerty: bitsy, suby, gifty, raidy, follow, nagrody ─────────────────

function plural(n, one, few, many) {
  if (n === 1) return one;
  const d = n % 10, t = n % 100;
  return d >= 2 && d <= 4 && (t < 12 || t > 14) ? few : many;
}

function bitsColor(n) {
  return n >= 10000 ? "#f43021" : n >= 5000 ? "#0099fe" : n >= 1000 ? "#1db2a5" : n >= 100 ? "#a35cff" : "#b5b3c2";
}

const ALERT_ICONS = {
  gem: '<svg viewBox="0 0 24 24"><path d="M12 2 3 9l9 13 9-13z" fill="currentColor"/><path d="M3 9h18M12 2 8 9l4 13 4-13z" fill="none" stroke="rgba(255,255,255,.55)" stroke-width="1.1"/></svg>',
  star: '<svg viewBox="0 0 24 24"><path d="m12 2 2.9 6.3 6.9.8-5.1 4.7 1.4 6.8L12 17.2 5.9 20.6l1.4-6.8L2.2 9.1l6.9-.8z" fill="currentColor"/></svg>',
  gift: '<svg viewBox="0 0 24 24"><path d="M3 10h18v4H3zM5 14h14v8H5z" fill="currentColor"/><path d="M12 10v12" stroke="rgba(0,0,0,.35)" stroke-width="2"/><path d="M12 10c-2-4-6-5-6-2s4 2 6 2zm0 0c2-4 6-5 6-2s-4 2-6 2z" fill="none" stroke="currentColor" stroke-width="2"/></svg>',
  rocket: '<svg viewBox="0 0 24 24"><path d="M14 3c4 0 7 3 7 7l-7 7-4-4-4-4z" fill="currentColor"/><circle cx="15.5" cy="8.5" r="1.8" fill="rgba(0,0,0,.4)"/><path d="M6 13l-3 1 2 2zm5 5-1 3-2-2zM7 17c-1 1-3 3-4 4 1-1 3-3 4-4z" fill="currentColor"/></svg>',
  heart: '<svg viewBox="0 0 24 24"><path d="M12 21s-8-5.2-8-11a4.5 4.5 0 0 1 8-2.8A4.5 4.5 0 0 1 20 10c0 5.8-8 11-8 11z" fill="currentColor"/></svg>',
  points: '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="9" fill="none" stroke="currentColor" stroke-width="3"/><path d="M12 7v5h5" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round"/></svg>',
  crown: '<svg viewBox="0 0 24 24"><path d="M3 8l4.5 4L12 5l4.5 7L21 8l-2 11H5z" fill="currentColor"/><rect x="5" y="19.5" width="14" height="2" rx="1" fill="currentColor"/></svg>',
  flame: '<svg viewBox="0 0 24 24"><path d="M12 2c1 4 6 6 6 12a6 6 0 0 1-12 0c0-3 1.5-5 3-6.5.3 2 1.2 3 2.5 3.5C11 8 10.5 5 12 2z" fill="currentColor"/></svg>',
  wave: '<svg viewBox="0 0 24 24"><path d="M8 13V5.5a1.5 1.5 0 0 1 3 0V11m0-6.5a1.5 1.5 0 0 1 3 0V11m0-5a1.5 1.5 0 0 1 3 0v8a7 7 0 0 1-12.5 4.3L2.6 15a1.5 1.5 0 0 1 2.3-1.9L8 16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>',
};

function alertLook(a) {
  switch (a.kind) {
    case "bits": return { icon: "gem", color: bitsColor(a.amount), big: a.amount >= 1000, label: "Bitsy" };
    case "bits_badge": return { icon: "gem", color: bitsColor(a.amount), big: true, label: "Odznaka bitsów" };
    case "sub": case "resub": return { icon: "star", color: a.tier === "3" ? "#ff9d2e" : "#f5c451", big: a.tier === "3", label: a.tier === "Prime" ? "Prime" : `Sub · Tier ${a.tier || 1}` };
    case "gift": return { icon: "gift", color: "#ff6fb5", big: false, label: "Gift" };
    case "upgrade": return { icon: "star", color: "#ff6fb5", big: false, label: "Sub" };
    case "mass_gift": return { icon: "gift", color: "#ff6fb5", big: a.amount >= 5, rainbow: true, label: "Gift-bomba" };
    case "raid": return { icon: "rocket", color: "#ff8a3d", big: a.amount >= 10, label: "Raid" };
    case "follow": return { icon: "heart", color: "#ff5c8a", big: false, label: "Follow" };
    case "watch_streak": return { icon: "flame", color: "#ff7a2f", big: a.amount >= 50, label: "Seria oglądania" };
    case "redemption": return { icon: "points", color: "#31d0c6", big: a.amount >= 10000, label: "Nagroda" };
    case "vip": return { icon: "crown", color: "#ffc83a", big: true, label: (a.target || "").startsWith("Szef Kosmi") ? "Twórca LiMBOCHAT" : "Gość specjalny" };
    default: return { icon: "wave", color: "#b9a6f7", big: false, label: "Nowy" };
  }
}

function b(text) {
  const el = document.createElement("b");
  el.textContent = text;
  return el;
}

/// Nagłówek po polsku; `counter` = element z liczbą do animacji.
function alertTitle(m, a) {
  const f = document.createElement("span");
  const name = a.anonymous ? "Anonim" : m.name;
  const count = (n) => {
    const s = document.createElement("span");
    s.className = "count";
    s.dataset.to = String(n);
    s.textContent = n.toLocaleString("pl-PL");
    return s;
  };
  const add = (...parts) => f.append(...parts.map((p) => (typeof p === "string" ? document.createTextNode(p) : p)));
  switch (a.kind) {
    case "bits": add(b(name), " wysyła ", count(a.amount), ` ${plural(a.amount, "bit", "bity", "bitsów")}!`); break;
    case "bits_badge": add(b(name), " zdobywa odznakę ", count(a.amount), " bitsów!"); break;
    case "sub": add(b(name), a.tier === "Prime" ? " subskrybuje z Prime!" : " subskrybuje kanał!"); break;
    case "resub": add(b(name), " subskrybuje już ", count(a.amount), ` ${plural(a.amount, "miesiąc", "miesiące", "miesięcy")}!`); break;
    case "gift": add(b(name), " daje suba dla ", b(a.target || "?"), "!"); break;
    case "upgrade": add(b(name), a.target ? " zostaje z subem od " : " zostaje z subem po gifcie", a.target ? b(a.target) : "", "!"); break;
    case "mass_gift": add(b(name), " rozdaje ", count(a.amount), ` ${plural(a.amount, "suba", "suby", "subów")}!`); break;
    case "raid": add(b(name), " wpada z raidem i ", count(a.amount), ` ${a.amount === 1 ? "widzem" : "widzami"}!`); break;
    case "follow": add(b(name), " obserwuje kanał!"); break;
    case "watch_streak": add(b(name), " ogląda już ", count(a.amount), ` ${plural(a.amount, "stream", "streamy", "streamów")} z rzędu!`); break;
    case "redemption": add(b(name), " odbiera nagrodę ", b(a.target || "nagroda")); break;
    case "vip": add("👑 ", b(a.target || name), "!"); break;
    default: add(b(name), " jest tu pierwszy raz — przywitaj się!");
  }
  return f;
}

function renderAlert(ch, m, live) {
  const a = m.alert;
  const look = alertLook(a);
  const settings = (state.settings && state.settings.alerts) || {};
  const row = document.createElement("div");
  row.className = `msg alert kind-${a.kind}`;
  row.dataset.id = m.id;
  row.dataset.user = m.user_id;
  row._msg = m;
  row.style.setProperty("--k", look.color);
  if (look.big) row.classList.add("big");
  if (look.rainbow) row.classList.add("rainbow");
  if (settings.enabled === false) row.classList.add("compact");
  if (m.deleted) row.classList.add("deleted");
  const fresh = live && settings.enabled !== false && !document.body.classList.contains("no-anim");
  if (fresh) row.classList.add("fresh"); else row.style.animation = "none";

  const icon = document.createElement("div");
  icon.className = "alert-icon";
  icon.innerHTML = ALERT_ICONS[look.icon];
  if (a.avatar) {
    const av = document.createElement("img");
    av.className = "alert-avatar";
    av.src = a.avatar;
    av.alt = "";
    icon.append(av);
  }

  const body = document.createElement("div");
  body.className = "alert-body";
  const head = document.createElement("div");
  head.className = "alert-head";
  const tag = document.createElement("span");
  tag.className = "alert-tag";
  tag.textContent = look.label + (a.test ? " · test" : "");
  const time = document.createElement("span");
  time.className = "time";
  time.textContent = new Date(m.ts).toLocaleTimeString("pl-PL", { hour: "2-digit", minute: "2-digit" });
  head.append(tag, time);
  if (a.kind === "redemption" && a.amount) {
    const cost = document.createElement("span");
    cost.className = "alert-cost";
    cost.textContent = `${a.amount.toLocaleString("pl-PL")} pkt`;
    head.append(cost);
  }
  const title = document.createElement("div");
  title.className = "alert-title";
  title.append(alertTitle(m, a));
  body.append(head, title);

  if (a.kind === "mass_gift" && a.target) {
    const total = document.createElement("div");
    total.className = "alert-sub";
    total.textContent = `Łącznie na kanale: ${Number(a.target).toLocaleString("pl-PL")}`;
    body.append(total);
  }
  const extra = document.createElement("div");
  extra.className = "alert-extra";
  if (a.extra) extra.textContent = a.kind === "redemption" || a.kind === "vip" ? `„${a.extra}”` : `Dla: ${a.extra}`;
  body.append(extra);

  if (m.segs.length) {
    const text = document.createElement("div");
    text.className = "alert-text";
    for (const seg of m.segs) {
      if (seg.k === "emote") text.append(renderEmote(seg.e, seg.ov));
      else {
        const t = document.createElement("span");
        t.textContent = ` ${seg.v} `;
        text.append(t);
      }
    }
    body.append(text);
  }

  const rhyme = document.createElement("div");
  rhyme.className = "alert-rhyme";
  body.append(rhyme);
  renderRhyme(rhyme, a, false);

  row.append(icon, body);
  if (fresh) {
    requestAnimationFrame(() => celebrate(row, look));
    if (settings.sound) playAlertSound(look.big);
  }
  return row;
}

function renderRhyme(box, a, animate) {
  box.classList.toggle("pending", !!a.rhyme_pending);
  if (a.rhyme_pending) {
    box.textContent = "LimboAI układa rymowankę…";
    return;
  }
  if (!a.rhyme) {
    box.hidden = true;
    return;
  }
  box.hidden = false;
  if (!animate || document.body.classList.contains("no-anim")) {
    box.textContent = a.rhyme;
    return;
  }
  let i = 0;
  box.textContent = "";
  const timer = setInterval(() => {
    i = Math.min(a.rhyme.length, i + 2);
    box.textContent = a.rhyme.slice(0, i);
    if (i >= a.rhyme.length) clearInterval(timer);
  }, 30);
}

function onAlertUpdate(msg) {
  const ch = state.channels.get(msg.channel);
  if (!ch) return;
  const row = ch.el.querySelector(`.msg.alert[data-id="${CSS.escape(msg.id)}"]`);
  if (!row) return;
  const stick = ch.atBottom;
  row._msg.alert = msg.alert;
  const extra = row.querySelector(".alert-extra");
  if (extra && msg.alert.extra) extra.textContent = msg.alert.kind === "redemption" ? `„${msg.alert.extra}”` : `Dla: ${msg.alert.extra}`;
  renderRhyme(row.querySelector(".alert-rhyme"), msg.alert, true);
  if (stick) scrollToBottom(ch);
}

/// Wybuch cząsteczek, liczenie w górę, konfetti przy gift-bombach.
function celebrate(row, look) {
  for (const el of row.querySelectorAll(".count")) {
    const to = Number(el.dataset.to);
    const start = performance.now();
    const dur = Math.min(1600, 500 + Math.log10(to + 1) * 280);
    const tick = (now) => {
      const p = Math.min(1, (now - start) / dur);
      const eased = 1 - Math.pow(1 - p, 3);
      el.textContent = Math.round(to * eased).toLocaleString("pl-PL");
      if (p < 1) requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  }
  const burst = document.createElement("div");
  burst.className = "burst";
  const n = look.big ? 30 : 16;
  const colors = look.rainbow ? ["#ff6fb5", "#ffd84d", "#5ee1a1", "#6cc6ff", "#b48cff"] : [look.color, "#ffffff", look.color];
  for (let i = 0; i < n; i++) {
    const p = document.createElement("i");
    const angle = (Math.PI * 2 * i) / n + Math.random() * 0.5;
    const dist = (look.big ? 70 : 46) + Math.random() * (look.big ? 70 : 40);
    p.style.setProperty("--dx", `${Math.cos(angle) * dist}px`);
    p.style.setProperty("--dy", `${Math.sin(angle) * dist * 0.7}px`);
    p.style.setProperty("--r", `${Math.random() * 540 - 270}deg`);
    p.style.setProperty("--d", `${0.7 + Math.random() * 0.6}s`);
    p.style.background = colors[i % colors.length];
    if (i % 3 === 0) p.className = "dot";
    burst.append(p);
  }
  row.append(burst);
  setTimeout(() => burst.remove(), 1600);
  if (look.rainbow || look.big) {
    const rain = document.createElement("div");
    rain.className = "confetti";
    for (let i = 0; i < 26; i++) {
      const c = document.createElement("i");
      c.style.left = `${Math.random() * 100}%`;
      c.style.setProperty("--d", `${1.1 + Math.random() * 1.2}s`);
      c.style.setProperty("--delay", `${Math.random() * 0.6}s`);
      c.style.setProperty("--r", `${Math.random() * 720}deg`);
      c.style.background = colors[i % colors.length];
      rain.append(c);
    }
    row.append(rain);
    setTimeout(() => rain.remove(), 3200);
  }
}

let audioCtx = null;
function playAlertSound(big) {
  try {
    audioCtx = audioCtx || new AudioContext();
    const notes = big ? [523, 659, 784, 1047, 1319] : [659, 880, 1175];
    notes.forEach((f, i) => {
      const o = audioCtx.createOscillator();
      const g = audioCtx.createGain();
      o.type = "triangle";
      o.frequency.value = f;
      const t = audioCtx.currentTime + i * 0.08;
      g.gain.setValueAtTime(0, t);
      g.gain.linearRampToValueAtTime(0.12, t + 0.02);
      g.gain.exponentialRampToValueAtTime(0.0001, t + 0.35);
      o.connect(g).connect(audioCtx.destination);
      o.start(t);
      o.stop(t + 0.4);
    });
  } catch { /* przeglądarka bez dźwięku */ }
}

// ── Ustawienia alertów i obrazków ──
const ALERT_SWITCHES = { "al-enabled": "enabled", "al-rhymes": "rhymes", "al-voice": "rhymes_voice", "al-chat": "rhymes_chat", "al-follows": "follows", "al-redemptions": "redemptions", "al-new": "new_chatters", "al-sound": "sound" };
function renderGreetings() {
  const g = (state.settings && state.settings.greetings) || {};
  $("gr-creator").checked = g.creator !== false;
  $("gr-voice").checked = g.voice !== false;
  $("gr-overlay").checked = g.overlay !== false;
  if (document.activeElement !== $("gr-custom")) $("gr-custom").value = (g.custom || []).map(([n, t]) => `${n}: ${t}`).join("\n");
}
function saveGreetings() {
  const custom = $("gr-custom").value
    .split("\n")
    .map((l) => l.split(":"))
    .filter((p) => p.length >= 2)
    .map(([n, ...t]) => [n.trim().replace(/^@/, "").toLowerCase(), t.join(":").trim()])
    .filter(([n, t]) => n && t);
  const g = { creator: $("gr-creator").checked, voice: $("gr-voice").checked, overlay: $("gr-overlay").checked, custom };
  state.settings.greetings = g;
  send({ c: "greetings", ...g });
}
for (const id of ["gr-creator", "gr-voice", "gr-overlay"]) $(id).addEventListener("change", saveGreetings);
$("gr-custom").addEventListener("change", saveGreetings);

function renderAlertSettings() {
  renderGreetings();
  const a = (state.settings && state.settings.alerts) || {};
  for (const [id, key] of Object.entries(ALERT_SWITCHES)) $(id).checked = !!a[key];
  for (const id of ["al-voice", "al-chat"]) $(id).closest("label").classList.toggle("dim", !a.rhymes);
  const mode = (state.settings && state.settings.media_previews) || "all";
  for (const btn of $("media-mode").children) btn.classList.toggle("on", btn.dataset.mode === mode);
}
for (const [id, key] of Object.entries(ALERT_SWITCHES)) {
  $(id).addEventListener("change", (e) => {
    const a = { ...(state.settings.alerts || {}), [key]: e.target.checked };
    state.settings.alerts = a;
    renderAlertSettings();
    send({ c: "alerts", ...a });
  });
}
for (const btn of $("media-mode").children) {
  btn.addEventListener("click", () => {
    state.settings.media_previews = btn.dataset.mode;
    renderAlertSettings();
    send({ c: "media_previews", mode: btn.dataset.mode });
  });
}
$("al-test").addEventListener("click", () => {
  $("settings").hidden = true;
  send({ c: "alert_test", kind: $("al-test-kind").value });
});
$("lightbox").addEventListener("click", closeLightbox);
document.addEventListener("keydown", (e) => { if (e.key === "Escape" && !$("lightbox").hidden) closeLightbox(); });

function renderEmote(e, overlays) {
  const wrap = document.createElement("span");
  wrap.className = "emote";
  const img = document.createElement("img");
  img.src = e.url;
  img.alt = e.name;
  img.loading = "lazy";
  wrap.append(img);
  for (const o of overlays) {
    const ov = document.createElement("img");
    ov.className = "ov";
    ov.src = o.url;
    ov.alt = o.name;
    wrap.append(ov);
  }
  wrap.addEventListener("mouseenter", () => showTip(wrap, e));
  wrap.addEventListener("mouseleave", hideTip);
  return wrap;
}

function renderModButtons(ch, m) {
  const box = document.createElement("span");
  box.className = "mod";
  const button = (icon, title, onClick, extra = "") => {
    const b = document.createElement("button");
    b.innerHTML = ICONS[icon];
    b.title = title;
    if (extra) b.className = extra;
    b.addEventListener("click", (e) => { e.stopPropagation(); onClick(e); });
    box.append(b);
    return b;
  };
  button("trash", "Usuń wiadomość", () => send({ c: "delete", channel: ch.login, message_id: m.id }));
  button("clock", "Timeout 10 min", () => send({ c: "timeout", channel: ch.login, user_id: m.user_id, login: m.login, seconds: 600 }));
  // Ban jednym kliknięciem tylko z Shiftem — ochrona przed misclickiem.
  button("ban", "Ban (przytrzymaj Shift)", (e) => {
    if (e.shiftKey) send({ c: "ban", channel: ch.login, user_id: m.user_id, login: m.login });
  }, "ban");
  return box;
}

/** Po zmianie konta przelicz podświetlenia wzmianek w już narysowanych wiadomościach. */
function refreshMentions() {
  const me = myLogin();
  for (const row of document.querySelectorAll(".msg:not(.system)")) {
    if (!row._msg) continue;
    row.classList.toggle("mention", mentionsMe(row._msg));
    for (const pill of row.querySelectorAll(".mention-pill")) {
      pill.classList.toggle("me", !!me && pill.textContent.replace(/^@/, "").toLowerCase() === me);
    }
  }
}

function system(channel, text, error) {
  const ch = channel ? state.channels.get(channel) : activeChannel();
  if (!ch) {
    state.lobbyLog.push({ text, error });
    state.lobbyLog = state.lobbyLog.slice(-5);
    return renderLobby();
  }
  const row = document.createElement("div");
  row.className = "msg system";
  if (error) row.classList.add("error");
  row.textContent = text;
  const stick = ch.atBottom;
  ch.el.appendChild(row);
  trim(ch);
  if (stick) scrollToBottom(ch);
}

function markDeleted(channel, predicate) {
  const ch = state.channels.get(channel);
  if (!ch) return;
  for (const row of ch.el.querySelectorAll(".msg:not(.system)")) if (predicate(row)) row.classList.add("deleted");
}

// ── Przewijanie ─────────────────────────────────────────────────────────

function scrollToBottom(ch) {
  ch.el.scrollTop = ch.el.scrollHeight;
  ch.atBottom = true;
  ch.unseen = 0;
}

function onScroll(ch) {
  const gap = ch.el.scrollHeight - ch.el.scrollTop - ch.el.clientHeight;
  if (gap < 6) ch.atBottom = true;
  else if (Date.now() < ch.userScrollUntil) ch.atBottom = false;
  else if (ch.atBottom) { ch.el.scrollTop = ch.el.scrollHeight; return; }
  if (ch.atBottom) ch.unseen = 0;
  if (ch.login === state.active) { renderJump(); renderStatus(); }
}

function renderJump() {
  const ch = activeChannel();
  const jump = $("jump");
  if (!ch || ch.atBottom) { jump.hidden = true; return; }
  jump.hidden = false;
  const n = ch.unseen;
  jump.textContent = n === 0 ? "↓  Najnowsze" : n === 1 ? "↓  1 nowa wiadomość" : n < 5 ? `↓  ${n} nowe wiadomości` : `↓  ${n} nowych wiadomości`;
}

// ── Menu użytkownika (moderacja) ────────────────────────────────────────

function openUserMenu(event, ch, m, row) {
  event.stopPropagation();
  const menu = $("menu");
  menu.replaceChildren();
  const head = document.createElement("div");
  head.className = "head";
  const name = document.createElement("b");
  name.textContent = m.name;
  name.style.color = readableColor(m.color, m.login);
  const id = document.createElement("div");
  id.className = "tag";
  id.textContent = `ID ${m.user_id}`;
  head.append(name, id);
  menu.append(head);
  // Pamięć LimboAI o tej osobie (dociąga się z silnika).
  const mem = document.createElement("div");
  mem.className = "mem";
  mem.id = "mem-box";
  mem.dataset.login = m.login;
  mem.dataset.name = m.name;
  mem.textContent = "🧠 …";
  mem.addEventListener("click", (e) => e.stopPropagation());
  menu.append(mem, sep());
  send({ c: "memory_get", login: m.login });

  const item = (icon, label, onClick, cls = "") => {
    const b = document.createElement("button");
    b.className = `item ${cls}`;
    b.innerHTML = ICONS[icon];
    b.append(label);
    b.addEventListener("click", () => { onClick(); closePopovers(); });
    menu.append(b);
    return b;
  };

  if (ch.canModerate || state.demo) {
    const del = item("trash", "Usuń wiadomość", () => send({ c: "delete", channel: ch.login, message_id: m.id }));
    del.disabled = row.classList.contains("deleted");
    const label = document.createElement("div");
    label.className = "item";
    label.innerHTML = ICONS.clock;
    label.append("Timeout");
    label.style.pointerEvents = "none";
    menu.append(label);
    const chips = document.createElement("div");
    chips.className = "chips";
    for (const [text, seconds] of TIMEOUTS) {
      const c = document.createElement("button");
      c.textContent = text;
      c.addEventListener("click", () => { send({ c: "timeout", channel: ch.login, user_id: m.user_id, login: m.login, seconds }); closePopovers(); });
      chips.append(c);
    }
    menu.append(chips);
    item("ban", "Ban", () => send({ c: "ban", channel: ch.login, user_id: m.user_id, login: m.login }), "danger");
    item("check", "Unban / zdejmij timeout", () => send({ c: "unban", channel: ch.login, user_id: m.user_id, login: m.login }));
  } else {
    const note = document.createElement("div");
    note.className = "note";
    note.textContent = "Zaloguj się jako moderator tego kanału, aby moderować.";
    menu.append(note);
  }
  menu.append(sep());
  item("copy", "Kopiuj wiadomość", () => copy(m.raw));
  item("user", "Kopiuj nick", () => copy(m.login));
  item("link", "Otwórz profil", () => send({ c: "open", url: `https://www.twitch.tv/${m.login}` }));

  menu.hidden = false;
  menu._at = { x: event.clientX, y: event.clientY };
  placeMenu();
}

function placeMenu() {
  const menu = $("menu");
  const { innerWidth: w, innerHeight: h } = window;
  const rect = menu.getBoundingClientRect();
  menu.style.left = `${Math.max(8, Math.min(menu._at.x, w - rect.width - 8))}px`;
  menu.style.top = `${Math.max(8, Math.min(menu._at.y, h - rect.height - 8))}px`;
}

// ── Pamięć o widzach ──
function fmtDate(ms) {
  return ms ? new Date(ms).toLocaleDateString("pl-PL", { day: "2-digit", month: "2-digit", year: "numeric" }) : "";
}

function onMemory(msg) {
  state.memoryCount = msg.count;
  renderMemorySettings();
  const box = $("mem-box");
  if (!box || $("menu").hidden || box.dataset.login !== msg.login) return;
  box.replaceChildren();
  const v = msg.viewer;
  const title = document.createElement("div");
  title.className = "mem-title";
  title.textContent = "🧠 Pamięć LimboAI";
  box.append(title);
  if (v && v.messages) {
    const stats = document.createElement("div");
    stats.className = "mem-stats";
    const parts = [`${v.messages.toLocaleString("pl-PL")} wiad.`];
    if (v.streams) parts.push(`${v.streams} ${plural(v.streams, "stream", "streamy", "streamów")}`);
    if (v.first_seen) parts.push(`od ${fmtDate(v.first_seen)}`);
    if (v.bits) parts.push(`💎 ${v.bits.toLocaleString("pl-PL")}`);
    if (v.sub_months) parts.push(`⭐ ${v.sub_months} mies.`);
    if (v.gifted) parts.push(`🎁 ${v.gifted}`);
    stats.textContent = parts.join(" · ");
    box.append(stats);
  }
  const list = document.createElement("div");
  list.className = "mem-notes";
  (v ? v.notes : []).forEach((n, i) => {
    const row = document.createElement("div");
    row.className = "mem-note";
    const t = document.createElement("span");
    t.textContent = n.text;
    t.title = `${n.source === "ai" ? "LimboAI z czatu" : n.source === "voice" ? "głosem" : "dopisane w panelu"} · ${fmtDate(n.at)}`;
    const x = document.createElement("button");
    x.textContent = "×";
    x.title = "Zapomnij";
    x.addEventListener("click", () => send({ c: "memory_forget", login: msg.login, index: i }));
    if (n.source === "ai") row.classList.add("ai");
    row.append(t, x);
    list.append(row);
  });
  if (!v || !v.notes.length) {
    const empty = document.createElement("div");
    empty.className = "mem-empty";
    empty.textContent = "Nic jeszcze nie wiem o tej osobie.";
    list.append(empty);
  }
  const input = document.createElement("input");
  input.className = "mem-input";
  input.placeholder = "Dopisz, np. „lubi koty” ↵";
  input.maxLength = 140;
  input.addEventListener("keydown", (e) => {
    e.stopPropagation();
    if (e.key === "Enter" && input.value.trim()) {
      send({ c: "memory_add", login: msg.login, name: box.dataset.name, text: input.value.trim() });
      input.value = "";
    }
    if (e.key === "Escape") closePopovers();
  });
  box.append(list, input);
  placeMenu();
}

function renderMemorySettings() {
  const s = state.settings || {};
  const mem = s.memory || {};
  $("mem-enabled").checked = mem.enabled !== false;
  $("mem-learn").checked = mem.learn !== false;
  $("mem-learn").closest("label").classList.toggle("dim", mem.enabled === false);
  const n = state.memoryCount || 0;
  $("mem-count").textContent = `Pamiętam ${n} ${plural(n, "osobę", "osoby", "osób")}.`;
  const sum = s.summary || {};
  $("sum-enabled").checked = sum.enabled !== false;
  $("sum-voice").checked = sum.voice !== false;
}

// ── OBS ──
function renderObs() {
  const o = state.obs || {};
  const cfg = (state.settings && state.settings.obs) || { enabled: true, port: 4455 };
  $("obs-enabled").checked = cfg.enabled !== false;
  if (document.activeElement !== $("obs-port")) $("obs-port").value = cfg.port || 4455;
  const st = $("obs-status");
  st.className = `obs-status ${o.connected ? "on" : cfg.enabled === false ? "" : "err"}`;
  if (cfg.enabled === false) st.textContent = "Wyłączone.";
  else if (o.connected) {
    st.textContent = `Połączono z OBS${o.version ? ` (WebSocket ${o.version})` : ""} · scena: ${o.current_scene || "?"}${o.streaming ? " · 🔴 na żywo" : ""}${o.recording ? " · ⏺ nagrywanie" : ""}`;
  } else st.textContent = o.error || "Łączenie z OBS…";
  const badge = $("obs-pass-state");
  badge.textContent = o.has_password ? "✓ zapisane" : "brak";
  badge.className = `key-state ${o.has_password ? "ok" : "missing"}`;
  $("obs-pass-remove").hidden = !o.has_password;
}
function saveObs() {
  const cfg = { enabled: $("obs-enabled").checked, port: Number($("obs-port").value) || 4455 };
  state.settings.obs = cfg;
  send({ c: "obs_settings", ...cfg });
  renderObs();
}
$("obs-enabled").addEventListener("change", saveObs);
$("obs-port").addEventListener("change", saveObs);
$("obs-pass-save").addEventListener("click", () => {
  const v = $("obs-pass").value.trim();
  if (!v) return $("obs-pass").focus();
  send({ c: "obs_password", password: v });
  $("obs-pass").value = "";
});
$("obs-pass").addEventListener("keydown", (e) => { if (e.key === "Enter") $("obs-pass-save").click(); });
$("obs-pass-remove").addEventListener("click", (e) => { e.preventDefault(); send({ c: "obs_password", password: null }); });
for (const id of ["mem-enabled", "mem-learn"]) {
  $(id).addEventListener("change", () => {
    const c = { enabled: $("mem-enabled").checked, learn: $("mem-learn").checked };
    state.settings.memory = c;
    send({ c: "memory_settings", ...c });
    renderMemorySettings();
  });
}
$("mem-clear").addEventListener("click", () => {
  const b = $("mem-clear");
  if (!b.dataset.sure) {
    b.dataset.sure = "1";
    b.textContent = "Na pewno? Kliknij jeszcze raz";
    setTimeout(() => { delete b.dataset.sure; b.textContent = "Wyczyść pamięć"; }, 4000);
    return;
  }
  delete b.dataset.sure;
  b.textContent = "Wyczyść pamięć";
  send({ c: "memory_clear" });
});
for (const id of ["sum-enabled", "sum-voice"]) {
  $(id).addEventListener("change", () => {
    const c = { enabled: $("sum-enabled").checked, voice: $("sum-voice").checked };
    state.settings.summary = c;
    send({ c: "summary_settings", ...c });
  });
}
$("sum-now").addEventListener("click", () => { $("settings").hidden = true; send({ c: "summary_now" }); });
$("sum-history").addEventListener("click", () => send({ c: "summary_history" }));
$("up-changelog").addEventListener("click", () => send({ c: "changelog" }));

// ── Okno (podsumowanie streama, co nowego) ──
function openModal(build, kind) {
  const card = $("modal-card");
  card.replaceChildren();
  card.dataset.kind = kind;
  build(card);
  $("modal").hidden = false;
  card.scrollTop = 0;
}
function closeModal() { $("modal").hidden = true; }
$("modal").addEventListener("click", (e) => { if (e.target.id === "modal") closeModal(); });
document.addEventListener("keydown", (e) => { if (e.key === "Escape" && !$("modal").hidden) closeModal(); });

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function richText(s) {
  // **pogrubienie** z CHANGELOG.md, reszta jako tekst.
  const span = el("span");
  s.split(/(\*\*[^*]+\*\*)/).forEach((part) => {
    if (/^\*\*[^*]+\*\*$/.test(part)) span.append(el("b", "", part.slice(2, -2)));
    else if (part) span.append(document.createTextNode(part));
  });
  return span;
}

function showChangelog(entries, afterUpdate) {
  if (!entries || !entries.length) return;
  if (afterUpdate) {
    const key = `limbochat-changelog-${state.version}`;
    try {
      if (localStorage.getItem(key)) return;
      localStorage.setItem(key, "1");
    } catch { /* bez pamięci przeglądarki pokażemy raz na sesję */ }
  }
  openModal((card) => {
    const head = el("div", "modal-head");
    head.append(el("div", "modal-title", afterUpdate ? `✨ Zaktualizowano do ${state.version}` : "✨ Co nowego"));
    const x = el("button", "modal-x", "×");
    x.addEventListener("click", closeModal);
    head.append(x);
    card.append(head);
    if (afterUpdate) card.append(el("p", "hint", "Oto co się zmieniło — OBS nie musiał się restartować."));
    for (const e of entries) {
      const sec = el("div", "cl-section");
      sec.append(el("div", "cl-version", e.version === "Następna wersja" ? "NAJNOWSZE" : `WERSJA ${e.version}`));
      const ul = el("ul", "cl-list");
      for (const item of e.items) {
        const li = el("li");
        li.append(richText(item));
        ul.append(li);
      }
      sec.append(ul);
      card.append(sec);
    }
    const ok = el("button", "btn-accent", "Super!");
    ok.addEventListener("click", closeModal);
    card.append(ok);
  }, "changelog");
}

function fmtMinutes(m) {
  const h = Math.floor(m / 60), r = m % 60;
  return h ? `${h} h ${r} min` : `${r} min`;
}

function sparkline(points, { area = true, color = "var(--k)" } = {}) {
  const w = 320, h = 70, pad = 4;
  const max = Math.max(1, ...points);
  const step = points.length > 1 ? (w - pad * 2) / (points.length - 1) : 0;
  const xy = points.map((v, i) => [pad + i * step, h - pad - (v / max) * (h - pad * 2)]);
  const line = xy.map(([x, y], i) => `${i ? "L" : "M"}${x.toFixed(1)},${y.toFixed(1)}`).join(" ");
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", `0 0 ${w} ${h}`);
  svg.setAttribute("class", "spark");
  svg.setAttribute("preserveAspectRatio", "none");
  svg.style.setProperty("--k", color);
  if (area && xy.length > 1) {
    const a = document.createElementNS(ns, "path");
    a.setAttribute("d", `${line} L${xy[xy.length - 1][0]},${h} L${xy[0][0]},${h} Z`);
    a.setAttribute("class", "spark-area");
    svg.append(a);
  }
  const p = document.createElementNS(ns, "path");
  p.setAttribute("d", line);
  p.setAttribute("class", "spark-line");
  svg.append(p);
  return svg;
}

function bars(values) {
  const box = el("div", "bars");
  const max = Math.max(1, ...values);
  for (const v of values) {
    const b = el("i");
    b.style.height = `${Math.max(3, (v / max) * 100)}%`;
    b.title = `${v} wiadomości`;
    box.append(b);
  }
  return box;
}

function showSummary(s) {
  const card = $("modal-card");
  // Dopisany tekst LimboAI do już otwartego podsumowania — tylko odśwież tekst.
  if (!$("modal").hidden && card.dataset.kind === "summary" && card.dataset.id === String(s.id)) {
    renderSummaryText(card.querySelector(".sum-ai"), s, true);
    if (s.title && card.querySelector(".sum-sub")) card.querySelector(".sum-sub").textContent = summarySubtitle(s);
    return;
  }
  openModal((card) => {
    card.dataset.id = String(s.id);
    const head = el("div", "modal-head");
    const title = el("div", "modal-title", "📊 Podsumowanie streama");
    const x = el("button", "modal-x", "×");
    x.addEventListener("click", closeModal);
    head.append(title, x);
    card.append(head, el("div", "sum-sub", summarySubtitle(s)));

    const tiles = el("div", "sum-tiles");
    const tile = (icon, label, value, extra) => {
      const t = el("div", "sum-tile");
      t.append(el("div", "sum-icon", icon), el("div", "sum-value", value), el("div", "sum-label", label));
      if (extra) t.append(el("div", "sum-extra", extra));
      tiles.append(t);
    };
    tile("⏱", "czas streama", fmtMinutes(s.minutes));
    tile("💬", "wiadomości", s.messages.toLocaleString("pl-PL"), `${s.chatters} ${plural(s.chatters, "osoba", "osoby", "osób")} na czacie`);
    if (s.peak_viewers) tile("👀", "widzów max", s.peak_viewers.toLocaleString("pl-PL"), `średnio ${s.avg_viewers}`);
    if (s.new_chatters) tile("👋", plural(s.new_chatters, "nowa osoba", "nowe osoby", "nowych osób"), s.new_chatters);
    if (s.bits) tile("💎", plural(s.bits, "bits", "bitsy", "bitsów"), s.bits.toLocaleString("pl-PL"));
    if (s.subs || s.resubs) tile("⭐", plural(s.subs + s.resubs, "sub", "suby", "subów"), s.subs + s.resubs, s.resubs ? `w tym ${s.resubs} ${plural(s.resubs, "resub", "resuby", "resubów")}` : "");
    if (s.gifts) tile("🎁", plural(s.gifts, "gift", "gifty", "giftów"), s.gifts);
    if (s.follows) tile("❤️", "follow", s.follows);
    if (s.redemptions) tile("🎯", plural(s.redemptions, "nagroda", "nagrody", "nagród"), s.redemptions);
    if (s.raids.length) tile("🚀", plural(s.raids.length, "raid", "raidy", "raidów"), s.raids.length, s.raids.map(([n, v]) => `${n} (${v})`).join(", "));
    card.append(tiles);

    if (s.viewers.length > 1) {
      const sec = el("div", "sum-section");
      sec.append(el("div", "sum-h", "Widzowie"), sparkline(s.viewers.map(([, v]) => v), { color: "#9be36d" }));
      card.append(sec);
    }
    if (s.messages_per_5min.some((v) => v)) {
      const sec = el("div", "sum-section");
      sec.append(el("div", "sum-h", "Aktywność czatu (co 5 minut)"), bars(s.messages_per_5min));
      card.append(sec);
    }
    if (s.top_chatters.length) {
      const sec = el("div", "sum-section");
      sec.append(el("div", "sum-h", "Najaktywniejsi na czacie"));
      const max = s.top_chatters[0][1] || 1;
      s.top_chatters.forEach(([name, count], i) => {
        const row = el("div", "sum-rank");
        row.append(el("span", "medal", ["🥇", "🥈", "🥉", "4.", "5."][i]), el("span", "rank-name", name));
        const bar = el("span", "rank-bar");
        bar.style.setProperty("--w", `${(count / max) * 100}%`);
        row.append(bar, el("span", "rank-count", count));
        sec.append(row);
      });
      card.append(sec);
    }
    if (s.top_supporters.length || s.top_emotes.length) {
      const sec = el("div", "sum-section sum-two");
      if (s.top_supporters.length) {
        const a = el("div");
        a.append(el("div", "sum-h", "Największe wsparcie"));
        for (const [n] of s.top_supporters) a.append(el("div", "sum-chip", `💜 ${n}`));
        sec.append(a);
      }
      if (s.top_emotes.length) {
        const e = el("div");
        e.append(el("div", "sum-h", "Ulubione emotki"));
        for (const [n, c] of s.top_emotes) e.append(el("div", "sum-chip", `${n} × ${c}`));
        sec.append(e);
      }
      card.append(sec);
    }
    const ai = el("div", "sum-ai");
    card.append(ai);
    renderSummaryText(ai, s, false);
    if (s.clips.length) {
      const sec = el("div", "sum-section");
      sec.append(el("div", "sum-h", "Klipy"));
      for (const url of s.clips) {
        const a = el("a", "sum-clip", url);
        a.href = url;
        a.addEventListener("click", (e) => { e.preventDefault(); send({ c: "open", url }); });
        sec.append(a);
      }
      card.append(sec);
    }
    const actions = el("div", "row-inline sum-actions");
    const copyBtn = el("button", "btn-ghost small", "Kopiuj tekst");
    copyBtn.addEventListener("click", () => copy(summaryAsText(s)));
    const ok = el("button", "btn-accent small", "Zamknij");
    ok.addEventListener("click", closeModal);
    actions.append(copyBtn, ok);
    card.append(actions);
  }, "summary");
  card.dataset.id = String(s.id);
  card._summary = s;
}

function summarySubtitle(s) {
  const d = new Date(s.started_at);
  const when = `${d.toLocaleDateString("pl-PL", { weekday: "long", day: "numeric", month: "long" })}, ${d.toLocaleTimeString("pl-PL", { hour: "2-digit", minute: "2-digit" })}`;
  return [when, s.category, s.title && `„${s.title}”`].filter(Boolean).join(" · ");
}

function renderSummaryText(box, s, animate) {
  if (!box) return;
  $("modal-card")._summary = s;
  box.classList.toggle("pending", !!s.ai_pending);
  if (s.ai_pending) {
    box.textContent = "LimboAI pisze podsumowanie…";
    return;
  }
  if (!s.ai_text) {
    box.hidden = true;
    return;
  }
  box.hidden = false;
  if (!animate || document.body.classList.contains("no-anim")) {
    box.textContent = s.ai_text;
    return;
  }
  let i = 0;
  box.textContent = "";
  const t = setInterval(() => {
    i = Math.min(s.ai_text.length, i + 3);
    box.textContent = s.ai_text.slice(0, i);
    if (i >= s.ai_text.length) clearInterval(t);
  }, 20);
}

function summaryAsText(s) {
  const lines = [`Podsumowanie streama — ${summarySubtitle(s)}`, `Czas: ${fmtMinutes(s.minutes)}, wiadomości: ${s.messages}, osób na czacie: ${s.chatters}`];
  if (s.peak_viewers) lines.push(`Widzowie: max ${s.peak_viewers}, średnio ${s.avg_viewers}`);
  if (s.top_chatters.length) lines.push(`Najaktywniejsi: ${s.top_chatters.map(([n, c]) => `${n} (${c})`).join(", ")}`);
  const ai = ($("modal-card")._summary || s).ai_text;
  if (ai) lines.push("", ai);
  return lines.join("\n");
}

function renderSummaryList(list) {
  const box = $("sum-list");
  box.hidden = false;
  box.replaceChildren();
  if (!list.length) {
    box.append(el("div", "hint", "Brak zapisanych podsumowań."));
    return;
  }
  for (const s of list) {
    const b = el("button", "sum-item");
    b.append(el("span", "", new Date(s.started_at).toLocaleDateString("pl-PL", { day: "2-digit", month: "2-digit" })), el("span", "", `${fmtMinutes(s.minutes)} · ${s.messages} wiad.${s.peak_viewers ? ` · ${s.peak_viewers} widzów` : ""}`));
    b.addEventListener("click", () => { $("settings").hidden = true; showSummary(s); });
    box.append(b);
  }
}

function sep() {
  const s = document.createElement("div");
  s.className = "sep";
  return s;
}

// ── Konto i logowanie ───────────────────────────────────────────────────

function setAuth(auth) {
  const wasPending = state.auth.state === "pending";
  state.auth = auth;
  if (auth.state === "pending") state.loginDeadline = Date.now() + auth.expires_in * 1000;
  if (auth.state === "logged_in" && wasPending) system(null, `Zalogowano jako ${auth.login}`, false);
  if (auth.state === "logged_out") state.avatar = null;
  refreshMentions();
  renderAccount();
  renderLogin();
  renderComposer();
  renderStatus();
  renderLobby();
}

function renderAccount() {
  const box = $("account");
  box.replaceChildren();
  const a = state.auth;
  if (a.state === "restoring") {
    const s = document.createElement("div");
    s.className = "spinner";
    s.style.cssText = "width:20px;height:20px;margin:0;border-width:2px";
    box.append(s);
  } else if (a.state === "logged_out") {
    const b = document.createElement("button");
    b.className = "btn-accent";
    b.textContent = "Zaloguj";
    b.addEventListener("click", () => send({ c: "login" }));
    box.append(b);
  } else if (a.state === "pending") {
    const t = document.createElement("span");
    t.className = "muted";
    t.textContent = "Logowanie…";
    box.append(t);
  } else {
    const chip = document.createElement("button");
    chip.className = "avatar-chip";
    const av = document.createElement("span");
    av.className = "avatar";
    if (state.avatar) {
      const img = document.createElement("img");
      img.src = state.avatar;
      av.append(img);
    } else {
      const initial = document.createElement("span");
      initial.className = "initial";
      initial.textContent = a.login.charAt(0).toUpperCase();
      av.append(initial);
    }
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = a.login;
    chip.append(av, name);
    chip.addEventListener("click", (e) => openAccountMenu(e));
    box.append(chip);
  }
}

function openAccountMenu(event) {
  event.stopPropagation();
  const menu = $("menu");
  menu.replaceChildren();
  const head = document.createElement("div");
  head.className = "head";
  const b = document.createElement("b");
  b.textContent = state.auth.login;
  const id = document.createElement("div");
  id.className = "tag";
  id.textContent = `ID ${state.auth.user_id}`;
  head.append(b, id);
  const out = document.createElement("button");
  out.className = "item";
  out.innerHTML = ICONS.logout;
  out.append("Wyloguj");
  out.addEventListener("click", () => { send({ c: "logout" }); closePopovers(); });
  menu.append(head, sep(), out);
  menu.hidden = false;
  const rect = menu.getBoundingClientRect();
  menu.style.left = `${window.innerWidth - rect.width - 8}px`;
  menu.style.top = "52px";
}

function renderLogin() {
  const pending = state.auth.state === "pending";
  $("login-overlay").hidden = !pending;
  if (pending) {
    $("login-code").textContent = state.auth.code;
    tickLogin();
  }
}

function tickLogin() {
  if (state.auth.state !== "pending") return;
  const left = Math.max(0, Math.round((state.loginDeadline - Date.now()) / 1000));
  $("login-time").textContent = `kod wygaśnie za ${Math.floor(left / 60)}:${String(left % 60).padStart(2, "0")}`;
  $("login-bar").style.width = `${Math.min(100, (left / 1800) * 100)}%`;
}

// ── Pole wiadomości, picker, podpowiedzi ────────────────────────────────

function renderComposer() {
  const ch = activeChannel();
  const loggedIn = state.auth.state === "logged_in";
  const enabled = !!ch && loggedIn && ch.connected;
  const input = $("input");
  input.disabled = !enabled;
  $("emote-btn").disabled = !enabled;
  input.placeholder = !ch ? "Otwórz kanał, aby pisać" : !loggedIn ? "Zaloguj się, aby pisać na czacie" : !ch.connected ? "Łączenie z kanałem…" : `Napisz na #${ch.login}…  (Tab — emotki)`;
  $("composer").hidden = !ch;
  updateCounter();
}

function updateCounter() {
  const n = $("input").value.length;
  const counter = $("counter");
  counter.hidden = n < 400;
  counter.textContent = `${n}/500`;
  counter.classList.toggle("full", n >= 500);
  $("send").disabled = $("input").disabled || !$("input").value.trim();
}

function submit() {
  const ch = activeChannel();
  const text = $("input").value.trim();
  if (!ch || !text || $("input").disabled) return;
  send({ c: "send", channel: ch.login, text });
  $("input").value = "";
  state.completion = null;
  updateCounter();
}

function ensureEmotes(ch) {
  if (ch && ch.emotes == null) { ch.emotes = []; send({ c: "emotes", channel: ch.login }); }
}

function togglePicker() {
  const picker = $("picker");
  const open = picker.hidden;
  closePopovers();
  if (!open) return;
  const ch = activeChannel();
  ensureEmotes(ch);
  picker.hidden = false;
  $("emote-btn").classList.add("open");
  $("picker-search").value = "";
  renderPicker();
  $("picker-search").focus();
}

function renderPicker() {
  const ch = activeChannel();
  const grid = $("picker-grid");
  grid.replaceChildren();
  const filter = $("picker-search").value.toLowerCase();
  const list = (ch && ch.emotes ? ch.emotes : []).filter((e) => !filter || e.name.toLowerCase().includes(filter));
  if (!list.length) {
    const empty = document.createElement("div");
    empty.className = "picker-empty";
    empty.textContent = ch && ch.emotes && ch.emotes.length ? "Nic nie pasuje do wyszukiwania." : "Brak emotek 7TV — trwa ładowanie albo kanał nie ma zestawu.";
    grid.append(empty);
    return;
  }
  for (const e of list) {
    const b = document.createElement("button");
    b.title = e.name;
    const img = document.createElement("img");
    img.src = e.url;
    img.alt = e.name;
    img.loading = "lazy";
    b.append(img);
    b.addEventListener("click", () => insertEmote(e.name));
    grid.append(b);
  }
}

function insertEmote(name) {
  const input = $("input");
  let v = input.value;
  if (v && !v.endsWith(" ")) v += " ";
  input.value = `${v}${name} `;
  input.focus();
  input.setSelectionRange(input.value.length, input.value.length);
  state.completion = null;
  updateCounter();
}

/** Tab: uzupełnia ostatnie słowo (emotki kanału → globalne → nicki); kolejne Taby przełączają. */
function complete() {
  const ch = activeChannel();
  if (!ch) return;
  ensureEmotes(ch);
  const input = $("input");
  const c = state.completion;
  if (c && input.value === c.applied && c.candidates.length) {
    c.index = (c.index + 1) % c.candidates.length;
    input.value = c.applied = `${c.base}${c.candidates[c.index]} `;
    return;
  }
  const split = input.value.lastIndexOf(" ") + 1;
  const base = input.value.slice(0, split);
  const word = input.value.slice(split);
  if (!word) return;
  const lower = word.toLowerCase();
  const chatters = [...new Set([...ch.el.querySelectorAll(".uname")].reverse().map((n) => n.textContent))];
  let candidates;
  if (lower.startsWith("@")) {
    candidates = chatters.filter((n) => n.toLowerCase().startsWith(lower.slice(1))).map((n) => `@${n}`);
  } else {
    candidates = (ch.emotes || []).map((e) => e.name).filter((n) => n.toLowerCase().startsWith(lower));
    candidates.push(...chatters.filter((n) => n.toLowerCase().startsWith(lower)));
  }
  candidates = [...new Set(candidates)].slice(0, 50);
  if (!candidates.length) return;
  input.value = `${base}${candidates[0]} `;
  state.completion = { base, candidates, index: 0, applied: input.value };
  updateCounter();
}

// ── Pasek statusu ───────────────────────────────────────────────────────

// Elementy paska tworzymy raz — przebudowa co sekundę restartowałaby animację słupków.
const statusEl = (() => {
  const make = (tag, cls) => { const e = document.createElement(tag); if (cls) e.className = cls; return e; };
  const el = {
    live: make("i", "live"), name: make("span"), bars: make("span", "bars"), perMin: make("span"),
    role: make("span", "role"), spacer: make("span", "spacer"), paused: make("span"), emotes: make("span"), empty: make("span"),
  };
  for (let i = 0; i < 4; i++) el.bars.append(document.createElement("i"));
  el.bars.title = "Aktywność czatu";
  el.paused.textContent = "⏸ pauza";
  el.paused.style.color = "var(--violet-soft)";
  el.emotes.title = "Emotki 7TV: globalne · kanałowe";
  el.empty.textContent = "brak otwartych kanałów";
  $("status").append(el.empty, el.live, el.name, el.bars, el.perMin, el.role, el.spacer, el.paused, el.emotes);
  return el;
})();

function renderStatus() {
  const el = statusEl;
  const ch = activeChannel();
  const show = (node, visible) => { node.hidden = !visible; };
  show(el.empty, !ch);
  for (const k of ["live", "name", "spacer", "emotes"]) show(el[k], !!ch);
  if (!ch) { for (const k of ["bars", "perMin", "role", "paused"]) show(el[k], false); return; }

  el.live.className = ch.connected ? "live" : "live wait";
  el.name.textContent = ch.connected ? `#${ch.login}` : `łączenie z #${ch.login}…`;
  el.name.style.color = ch.connected ? "" : "var(--warn)";

  const now = Date.now();
  ch.recent = ch.recent.filter((t) => now - t < RATE_WINDOW_MS);
  const rate = ch.recent.length / (RATE_WINDOW_MS / 1000);
  // Słupki tańczą szybciej i wyżej, gdy czat przyspiesza.
  const energy = Math.min(1, Math.max(0.12, rate / 4));
  el.bars.style.setProperty("--speed", `${Math.max(0.35, 1.6 - rate * 0.12).toFixed(2)}s`);
  el.bars.style.setProperty("--min", (0.2 + energy * 0.15).toFixed(2));
  el.bars.style.setProperty("--max", (0.35 + energy * 0.65).toFixed(2));
  el.perMin.textContent = `${Math.round(rate * 60)}/min`;
  show(el.bars, ch.connected);
  show(el.perMin, ch.connected);

  const showRole = ch.connected && state.auth.state === "logged_in";
  show(el.role, showRole);
  el.role.className = ch.canModerate ? "role mod" : "role viewer";
  el.role.textContent = ch.canModerate ? "moderator" : "widz";
  show(el.paused, !ch.atBottom);
  el.emotes.textContent = `7TV ${state.globalEmotes} · ${ch.emoteCount}`;
}

// ── Ekran powitalny, ustawienia, drobiazgi ──────────────────────────────

function renderLobby() {
  const empty = state.order.length === 0;
  $("lobby").hidden = !empty;
  $("lobby-login").hidden = state.auth.state !== "logged_out";
  const log = $("lobby-log");
  log.replaceChildren();
  for (const { text, error } of state.lobbyLog) {
    const p = document.createElement("div");
    p.textContent = text;
    if (error) p.className = "error";
    log.append(p);
  }
}

// ── Kolory motywu ──────────────────────────────────────────────────────

const DEFAULT_ACCENT = "#8b6cf0";
const ACCENTS = [
  ["#8b6cf0", "Fiolet"], ["#f0619e", "Róż"], ["#f0505a", "Czerwień"], ["#f08a3c", "Pomarańcz"], ["#e8b84a", "Złoto"],
  ["#3fcf8e", "Zieleń"], ["#33c4d6", "Turkus"], ["#4f8cf0", "Niebieski"], ["#c9cfdc", "Srebro"],
];
const ACCENT_VARS = ["--a", "--a-soft", "--a-deep", "--a-alt", "--a-pale", "--a-btn-top", "--a-btn-bot", "--a-blob1", "--a-blob2", "--a-blob3", "--tint0", "--tint1", "--tint2", "--tint3", "--bg", "--logo-filter"];

function hexToHsl(hex) {
  const v = parseInt(hex.slice(1), 16);
  const [r, g, b] = [(v >> 16) & 255, (v >> 8) & 255, v & 255].map((c) => c / 255);
  const max = Math.max(r, g, b), min = Math.min(r, g, b), l = (max + min) / 2;
  if (max === min) return [0, 0, l];
  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  const h = max === r ? ((g - b) / d + (g < b ? 6 : 0)) : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
  return [h * 60, s, l];
}

function hslTriplet(h, s, l) {
  h = ((h % 360) + 360) % 360;
  s = Math.min(1, Math.max(0, s));
  l = Math.min(1, Math.max(0, l));
  const k = (n) => (n + h / 30) % 12;
  const a = s * Math.min(l, 1 - l);
  const f = (n) => l - a * Math.max(-1, Math.min(k(n) - 3, 9 - k(n), 1));
  return [f(0), f(8), f(4)].map((x) => Math.round(x * 255)).join(" ");
}

/** Cała paleta panelu (przyciski, szkło, zorza, orb, logo) z jednego koloru. */
function applyAccent(hex) {
  const root = document.documentElement.style;
  if (!/^#[0-9a-f]{6}$/i.test(hex || "") || hex.toLowerCase() === DEFAULT_ACCENT) {
    for (const v of ACCENT_VARS) root.removeProperty(v); // dopracowany ręcznie fiolet z CSS
    renderSwatches(DEFAULT_ACCENT);
    return;
  }
  const [h, s, l] = hexToHsl(hex);
  const set = (name, hh, ss, ll) => root.setProperty(name, hslTriplet(hh, ss, ll));
  set("--a", h, s, l);
  set("--a-soft", h, s * 0.85, Math.min(0.85, l + 0.22));
  set("--a-deep", h, s * 0.6, Math.max(0.18, l - 0.27));
  set("--a-alt", h + 35, s, Math.min(0.8, l + 0.08));
  set("--a-pale", h, s * 0.8, 0.9);
  set("--a-btn-top", h, s, Math.min(0.85, l + 0.06));
  set("--a-btn-bot", h, s * 0.6, Math.max(0.2, l - 0.12));
  set("--a-blob1", h, s * 0.6, 0.5);
  set("--a-blob2", h, s * 0.65, 0.37);
  set("--a-blob3", h + 34, s * 0.6, 0.4);
  const ts = Math.min(0.37, s * 0.4);
  set("--tint0", h, ts, 0.075);
  set("--tint1", h, ts, 0.09);
  set("--tint2", h, ts, 0.11);
  set("--tint3", h, ts, 0.12);
  root.setProperty("--bg", `rgb(${hslTriplet(h, ts * 0.8, 0.04)})`);
  // Fiolet w logo ma odcień ~268° — obracamy go do nowego akcentu.
  root.setProperty("--logo-filter", `hue-rotate(${Math.round(h - 268)}deg) saturate(${s < 0.25 ? 0.15 : 1})`);
  renderSwatches(hex);
}

function renderSwatches(current) {
  const box = $("swatches");
  if (!box) return;
  box.replaceChildren();
  const cur = (current || DEFAULT_ACCENT).toLowerCase();
  let preset = false;
  for (const [hex, name] of ACCENTS) {
    const b = document.createElement("button");
    b.className = "swatch";
    b.style.setProperty("--c", hex);
    b.title = name;
    if (hex === cur) { b.classList.add("active"); preset = true; }
    b.addEventListener("click", () => saveAccent(hex));
    box.append(b);
  }
  const custom = document.createElement("label");
  custom.className = "swatch custom";
  custom.title = "Własny kolor";
  if (!preset) { custom.classList.add("active"); custom.style.setProperty("--c", cur); }
  const input = document.createElement("input");
  input.type = "color";
  input.value = cur;
  input.addEventListener("input", () => applyAccent(input.value)); // podgląd na żywo
  input.addEventListener("change", () => saveAccent(input.value));
  custom.append(input);
  box.append(custom);
}

function saveAccent(hex) {
  applyAccent(hex);
  send({ c: "theme", accent: hex });
}

function applySettings() {
  applyAccent(state.settings.accent);
  document.body.classList.toggle("no-anim", !state.settings.animations);
  document.body.classList.toggle("no-time", !state.settings.timestamps);
  document.body.classList.toggle("no-mod", !state.settings.mod_buttons);
  $("set-anim").checked = state.settings.animations;
  $("set-time").checked = state.settings.timestamps;
  $("set-mod").checked = state.settings.mod_buttons;
  renderOverlay();
  renderUpdate();
  renderAlertSettings();
  renderObs();
  renderMemorySettings();
}

function saveSettings() {
  send({ c: "settings", animations: $("set-anim").checked, timestamps: $("set-time").checked, mod_buttons: $("set-mod").checked });
}

function renderAll() {
  renderTabs();
  renderComposer();
  renderStatus();
  renderJump();
  renderLobby();
}

function closePopovers() {
  $("picker").hidden = true;
  $("settings").hidden = true;
  $("menu").hidden = true;
  $("emote-btn").classList.remove("open");
  hideTip();
}

function openAddTab() {
  $("add-form").classList.add("open");
  $("add-btn").hidden = true;
  $("add-input").focus();
}

function closeAddTab() {
  $("add-form").classList.remove("open");
  $("add-btn").hidden = false;
  $("add-input").value = "";
}

function showTip(anchor, e) {
  const tip = $("tip");
  tip.replaceChildren();
  const img = document.createElement("img");
  img.src = e.url;
  const name = document.createElement("b");
  name.textContent = e.name;
  const src = document.createElement("div");
  src.className = "tag";
  src.textContent = `${e.src === "twitch" ? "Twitch" : "7TV"}${e.zw ? " · zero-width" : ""}`;
  tip.append(img, name, src);
  tip.hidden = false;
  const r = anchor.getBoundingClientRect();
  const t = tip.getBoundingClientRect();
  tip.style.left = `${Math.max(8, Math.min(r.left + r.width / 2 - t.width / 2, innerWidth - t.width - 8))}px`;
  tip.style.top = `${r.top - t.height - 8 < 8 ? r.bottom + 8 : r.top - t.height - 8}px`;
}

function hideTip() {
  $("tip").hidden = true;
}

function copy(text) {
  if (navigator.clipboard && window.isSecureContext) {
    navigator.clipboard.writeText(text).catch(() => legacyCopy(text));
  } else legacyCopy(text);
}

function legacyCopy(text) {
  const ta = document.createElement("textarea");
  ta.value = text;
  document.body.append(ta);
  ta.select();
  document.execCommand("copy");
  ta.remove();
}

function duration(s) {
  if (s < 60) return `${s} s`;
  if (s < 3600) return `${Math.floor(s / 60)} min`;
  if (s < 86400) return `${Math.floor(s / 3600)} h`;
  return `${Math.floor(s / 86400)} dni`;
}

/** Za ciemne kolory nicków rozjaśniamy; brak koloru → stały kolor z hasha loginu. */
function readableColor(hex, login) {
  const fallback = ["#ff9e80", "#8cb4ff", "#7dd3a8", "#f0c97a", "#e89a6e", "#7fc8d0", "#f29cc8", "#b9a6f7"];
  if (!hex) {
    let h = 0;
    for (const c of login) h = (h * 31 + c.charCodeAt(0)) >>> 0;
    return fallback[h % fallback.length];
  }
  const v = parseInt(hex.slice(1), 16);
  let [r, g, b] = [(v >> 16) & 255, (v >> 8) & 255, v & 255];
  const lum = (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;
  if (lum < 0.4) [r, g, b] = [r, g, b].map((c) => Math.round(c + (255 - c) * 0.5));
  return `rgb(${r}, ${g}, ${b})`;
}

// ── Obsługa interakcji ──────────────────────────────────────────────────

$("add-btn").addEventListener("click", openAddTab);
$("lobby-join").addEventListener("click", openAddTab);
$("lobby-login").addEventListener("click", () => send({ c: "login" }));
$("add-form").addEventListener("submit", (e) => {
  e.preventDefault();
  const name = $("add-input").value.trim();
  if (name) send({ c: "join", channel: name });
  closeAddTab();
});
$("add-input").addEventListener("keydown", (e) => { if (e.key === "Escape") closeAddTab(); });
$("add-input").addEventListener("blur", () => setTimeout(() => { if (!$("add-input").value) closeAddTab(); }, 150));

$("input").addEventListener("keydown", (e) => {
  if (e.key === "Enter") { e.preventDefault(); submit(); }
  else if (e.key === "Tab") { e.preventDefault(); complete(); }
});
$("input").addEventListener("input", () => { state.completion = null; updateCounter(); });
$("send").addEventListener("click", submit);
$("emote-btn").addEventListener("click", (e) => { e.stopPropagation(); togglePicker(); });
$("picker-search").addEventListener("input", renderPicker);
$("picker").addEventListener("click", (e) => e.stopPropagation());
$("settings-btn").addEventListener("click", (e) => {
  e.stopPropagation();
  const open = $("settings").hidden;
  closePopovers();
  $("settings").hidden = !open;
  if (open) {
    send({ c: "jarvis_refresh" }); // odśwież listę mikrofonów i stan kluczy
    $("ov-url").textContent = `http://${location.host || "127.0.0.1:47250"}/overlay`;
    renderOverlay();
  }
});
$("settings").addEventListener("click", (e) => {
  e.stopPropagation();
  if (e.target.id === "settings") $("settings").hidden = true;
});
$("settings-close").addEventListener("click", () => { $("settings").hidden = true; });
for (const id of ["set-anim", "set-time", "set-mod"]) $(id).addEventListener("change", saveSettings);
$("menu").addEventListener("click", (e) => e.stopPropagation());
document.addEventListener("click", closePopovers);
document.addEventListener("keydown", (e) => { if (e.key === "Escape") closePopovers(); });
$("jump").addEventListener("click", () => { const ch = activeChannel(); if (ch) { scrollToBottom(ch); renderJump(); renderStatus(); } });
$("login-copy").addEventListener("click", () => copy(state.auth.code || ""));
$("login-open").addEventListener("click", () => send({ c: "open", url: state.auth.uri }));
$("login-cancel").addEventListener("click", () => send({ c: "cancel_login" }));
window.addEventListener("resize", moveIndicator);

// Ban jednym kliknięciem „uzbraja się” przy wciśniętym Shift.
const arm = (on) => document.querySelectorAll(".mod .ban").forEach((b) => b.classList.toggle("armed", on));
document.addEventListener("keydown", (e) => { if (e.key === "Shift") arm(true); });
document.addEventListener("keyup", (e) => { if (e.key === "Shift") arm(false); });
window.addEventListener("blur", () => arm(false));

// ── Jarvis ──────────────────────────────────────────────────────────────

const MODIFIERS = ["Ctrl", "Alt", "Shift"];
const jv = { state: "idle", hideTimer: null, typeTimer: null };
let hotkeyRec = null;

function onJarvis(ev) {
  switch (ev.e) {
    case "status": state.jarvis = ev; renderJarvisSettings(); return renderJarvisButton();
    case "state": return setJarvisState(ev.state);
    case "level":
      if (enroll.active) $("enroll-level").style.width = `${Math.min(100, ev.v * 100).toFixed(0)}%`;
      return $("orb").style.setProperty("--level", Math.min(1, ev.v).toFixed(3));
    case "transcript": $("jv-transcript").textContent = ev.text; return;
    case "action": return addJarvisAction(ev);
    case "reply": return typeReply(ev.text);
    case "error": return showJarvisError(ev.text);
    case "reminder": return showReminder(ev.text);
    case "enroll": return onEnroll(ev);
    case "models": return onModels(ev);
    case "vip": return; // karta przychodzi jako alert na czacie
  }
}

function showReminder(text) {
  showJarvis();
  const el = $("jarvis");
  el.dataset.state = "speaking";
  $("jv-state").textContent = "Przypomnienie";
  $("jv-transcript").textContent = "";
  $("jv-actions").replaceChildren();
  typeReply(text);
  setTimeout(() => { if (jv.state === "idle") { el.dataset.state = "idle"; hideJarvisSoon(6000); } }, 400);
}

// ── Nauka „Hej Limbo” ──
const enroll = { active: false, step: 0 };

function onEnroll(ev) {
  enroll.step = ev.step;
  const box = $("enroll");
  box.hidden = false;
  box.dataset.state = ev.state;
  enroll.active = ev.state === "listening" || ev.state === "ok" || ev.state === "retry";
  [...$("enroll-dots").children].forEach((d, i) => {
    const n = i + 1;
    d.className = n < ev.step || (n === ev.step && (ev.state === "ok" || ev.state === "done")) ? "done" : n === ev.step && enroll.active ? "now" : "";
  });
  $("enroll-text").textContent = ev.state === "done" ? ev.text : `${ev.step}/${ev.total} · ${ev.text}`;
  if (ev.state === "done") {
    // Po nauce od razu włączamy nasłuch.
    if (state.jarvis && !state.jarvis.config.wake_word) saveJarvis({ wake_word: true });
    setTimeout(() => { if (!enroll.active) box.hidden = true; }, 6000);
  }
  if (ev.state === "cancelled") setTimeout(() => { if (!enroll.active) box.hidden = true; }, 1500);
  renderWake();
}

function renderWake() {
  const j = state.jarvis;
  if (!j) return;
  const c = j.config;
  $("jv-wake").checked = c.wake_word;
  $("jv-wake").disabled = !j.wake_trained && !c.wake_word;
  $("jv-wake-sens").value = String(c.wake_sensitivity || 2);
  $("jv-wake-sens").hidden = !j.wake_trained;
  $("jv-mentions").checked = c.read_mentions;
  $("wake-train").hidden = enroll.active;
  $("wake-train").textContent = j.wake_trained ? "Naucz ponownie" : "Naucz mój głos";
  $("wake-train").disabled = !c.enabled;
  $("wake-cancel").hidden = !enroll.active;
  $("wake-reset").hidden = enroll.active || !j.wake_trained;
  const st = $("wake-status");
  let text, cls;
  if (enroll.active) [text, cls] = ["Nagrywanie wzorca…", "busy"];
  else if (j.wake_error) [text, cls] = [`Nasłuch wyłączony: ${j.wake_error}`, "err"];
  else if (!j.wake_trained) [text, cls] = ["Najpierw naucz LimboAI Twojego głosu (3 krótkie nagrania).", ""];
  else if (j.wake_listening) [text, cls] = ["Nasłuchuję „Hej Limbo”", "on"];
  else [text, cls] = ["Głos nauczony · nasłuch wyłączony", ""];
  st.textContent = text;
  st.className = `wake-status ${cls}`;
}

function showJarvis() {
  const el = $("jarvis");
  clearTimeout(jv.hideTimer);
  el.classList.remove("leaving");
  el.hidden = false;
}

function hideJarvis() {
  const el = $("jarvis");
  if (el.hidden) return;
  el.classList.add("leaving");
  setTimeout(() => { el.hidden = true; el.classList.remove("leaving"); }, 300);
}

function hideJarvisSoon(ms) {
  clearTimeout(jv.hideTimer);
  jv.hideTimer = setTimeout(() => {
    if ($("jarvis").matches(":hover")) return hideJarvisSoon(1500);
    hideJarvis();
  }, ms);
}

function setJarvisState(s) {
  const el = $("jarvis");
  jv.state = s;
  renderJarvisButton();
  if (s === "idle") {
    $("orb").style.setProperty("--level", "0");
    if (el.dataset.state !== "error") el.dataset.state = "idle";
    $("jv-state").textContent = el.dataset.state === "error" ? "Problem" : "";
    if (!el.hidden) hideJarvisSoon(el.dataset.state === "error" ? 6000 : 7000);
    return;
  }
  el.dataset.state = s;
  $("jv-state").textContent = { listening: "Słucham", thinking: "Myślę", speaking: "Mówię" }[s] || "";
  if (s === "listening") {
    clearInterval(jv.typeTimer);
    $("jv-transcript").textContent = "";
    $("jv-reply").textContent = "";
    $("jv-actions").replaceChildren();
    $("orb").style.setProperty("--level", "0");
  }
  showJarvis();
}

function addJarvisAction(a) {
  const chip = document.createElement(a.url ? "button" : "span");
  chip.className = "jv-action";
  if (!a.ok) chip.classList.add("fail");
  chip.textContent = a.text;
  if (a.url) {
    chip.classList.add("link");
    chip.title = a.url;
    chip.addEventListener("click", () => send({ c: "open", url: a.url }));
  }
  $("jv-actions").append(chip);
  showJarvis();
}

/** Odpowiedź pojawia się litera po literze, jak w filmie. */
function typeReply(text) {
  const box = $("jv-reply");
  clearInterval(jv.typeTimer);
  showJarvis();
  if (!state.settings.animations) { box.textContent = text; return; }
  let i = 0;
  const caret = document.createElement("span");
  caret.className = "caret";
  jv.typeTimer = setInterval(() => {
    i = Math.min(text.length, i + 2);
    box.textContent = text.slice(0, i);
    if (i < text.length) box.append(caret);
    else clearInterval(jv.typeTimer);
  }, 22);
}

function showJarvisError(text) {
  showJarvis();
  $("jarvis").dataset.state = "error";
  $("jv-state").textContent = "Problem";
  typeReply(text);
  hideJarvisSoon(6000);
}

function renderJarvisButton() {
  const btn = $("jarvis-btn");
  const j = state.jarvis;
  const enabled = !!j && j.config.enabled;
  btn.hidden = !j;
  btn.disabled = !enabled;
  btn.style.opacity = enabled ? "" : ".4";
  btn.classList.toggle("active", jv.state !== "idle");
  btn.classList.toggle("wake-on", !!j && j.config.enabled && !!j.wake_listening);
  btn.title = j ? `LimboAI (${j.config.hotkey.join(" + ")})` : "LimboAI";
}

function option(value, label) {
  const o = document.createElement("option");
  o.value = value;
  o.textContent = label;
  return o;
}

function renderKeys(keys, placeholder) {
  const box = $("jv-keys");
  box.replaceChildren();
  for (const k of keys) {
    const kbd = document.createElement("kbd");
    kbd.textContent = k;
    box.append(kbd);
  }
  if (placeholder) {
    const p = document.createElement("span");
    p.className = "placeholder";
    p.textContent = placeholder;
    box.append(p);
  }
}

function renderJarvisSettings() {
  const j = state.jarvis;
  if (!j) return;
  const c = j.config;
  $("jv-enabled").checked = c.enabled;
  $("jv-voice").checked = c.voice;
  if (!hotkeyRec) renderKeys(c.hotkey);
  const hold = c.talk_mode === "hold";
  for (const b of $("jv-talk").children) b.classList.toggle("on", b.dataset.mode === (hold ? "hold" : "toggle"));
  $("jv-hotkey-note").textContent = !j.hotkey_supported
    ? "Skrót globalny działa tylko w Windows — tutaj użyj przycisku LimboAI w nagłówku."
    : hold
      ? "Trzymaj przycisk, gdy mówisz — puść, a LimboAI wykona polecenie. Działa też w grze."
      : "Naciśnij, powiedz polecenie, naciśnij ponownie (albo po prostu zamilknij). Działa też w grze.";
  renderBrain();

  const mic = $("jv-mic");
  mic.replaceChildren(option("", "Domyślny mikrofon systemu"), ...j.mics.map((m) => option(m, m)));
  if (c.mic && !j.mics.includes(c.mic)) mic.append(option(c.mic, `${c.mic} (niepodłączony)`));
  mic.value = c.mic || "";

  const model = $("jv-model");
  model.replaceChildren(...j.models.map(([id, label]) => option(id, label)));
  model.value = c.model;

  for (const field of document.querySelectorAll(".key-field")) {
    const has = { gemini: j.gemini_key, groq: j.groq_key, llm: j.llm_key }[field.dataset.provider];
    const badge = field.querySelector(".key-state");
    badge.textContent = has ? "✓ zapisany" : "brak";
    badge.className = `key-state ${has ? "ok" : "missing"}`;
    field.querySelector(".remove").hidden = !has;
    field.querySelector("input").placeholder = has ? "wklej nowy klucz, aby zmienić…" : "wklej klucz API…";
  }
  $("jv-voice-note").hidden = j.polish_voice !== false;
  $("jv-test").disabled = !c.enabled;
  renderWake();
}

function saveJarvis(patch = {}) {
  if (!state.jarvis) return;
  const c = { ...state.jarvis.config, ...patch };
  c.mic = c.mic || null;
  state.jarvis.config = c; // od razu w panelu, serwer i tak odeśle stan
  send({ c: "jarvis_settings", ...c });
}

// ── Mózg: Gemini albo własny model (OpenAI, OpenRouter, Groq, Ollama, LM Studio…) ──
function renderBrain() {
  const j = state.jarvis;
  const c = j.config;
  const custom = c.brain === "custom";
  for (const b of $("jv-brain").children) b.classList.toggle("on", b.dataset.brain === (custom ? "custom" : "gemini"));
  $("gemini-box").hidden = custom;
  $("llm-box").hidden = !custom;
  $("gemini-hint").textContent = custom
    ? "Opcjonalny — zapasowe rozpoznawanie mowy, gdy nie masz klucza Groq."
    : "Mózg LimboAI.";
  const sel = $("llm-preset");
  if (sel.options.length !== j.llm_presets.length) sel.replaceChildren(...j.llm_presets.map((p) => option(p.id, p.name)));
  sel.value = c.llm_preset;
  const preset = j.llm_presets.find((p) => p.id === c.llm_preset) || {};
  $("llm-url-field").hidden = c.llm_preset !== "custom";
  if (document.activeElement !== $("llm-url")) $("llm-url").value = c.llm_url || "";
  if (document.activeElement !== $("llm-model")) $("llm-model").value = c.llm_model || "";
  $("llm-model").placeholder = preset.model ? `np. ${preset.model}` : "nazwa modelu";
  const local = c.llm_preset === "ollama" || c.llm_preset === "lmstudio";
  $("llm-key-hint").textContent = local
    ? "Model lokalny — klucz niepotrzebny."
    : c.llm_preset === "groq" ? "Bez osobnego klucza użyję klucza Groq z rozpoznawania mowy."
    : c.llm_preset === "custom" ? "Jeśli Twój serwer wymaga klucza." : "Wymagany.";
  const keyField = document.querySelector('.key-field[data-provider="llm"]');
  keyField.classList.toggle("dim", local);
  if (!$("llm-note").dataset.sticky) {
    $("llm-note").textContent = {
      ollama: "Zainstaluj Ollamę, pobierz model z narzędziami (np. ollama pull qwen2.5:7b) i zostaw ją włączoną.",
      lmstudio: "W LM Studio włącz serwer (Developer → Start Server) i załaduj model z obsługą narzędzi.",
      openrouter: "Setki modeli pod jednym kluczem — wybierz taki z obsługą narzędzi (tools).",
    }[c.llm_preset] || "Model musi obsługiwać wywoływanie narzędzi (function calling).";
  }
}

function setLlmNote(text, error) {
  const note = $("llm-note");
  note.textContent = text;
  note.classList.toggle("warn", !!error);
  note.dataset.sticky = "1";
  clearTimeout(setLlmNote.t);
  setLlmNote.t = setTimeout(() => { delete note.dataset.sticky; note.classList.remove("warn"); if (state.jarvis) renderBrain(); }, 9000);
}

function onModels(ev) {
  $("llm-fetch").disabled = false;
  $("llm-fetch").textContent = "Pobierz listę";
  if (ev.error) return setLlmNote(ev.error, true);
  $("llm-models").replaceChildren(...ev.models.map((m) => option(m, m)));
  setLlmNote(`Znaleziono ${ev.models.length} modeli — kliknij pole „Model” i wybierz z listy.`, false);
}

// Nagrywanie skrótu: naciśnij kombinację, puść wszystkie klawisze → zapis.
function keyName(code) {
  if (/^Shift/.test(code)) return "Shift";
  if (/^Control/.test(code)) return "Ctrl";
  if (/^Alt/.test(code)) return "Alt";
  let m = code.match(/^Key([A-Z])$/);
  if (m) return m[1];
  m = code.match(/^Digit([0-9])$/);
  if (m) return m[1];
  m = code.match(/^F([0-9]{1,2})$/);
  if (m && +m[1] >= 1 && +m[1] <= 24) return `F${m[1]}`;
  m = code.match(/^Numpad([0-9])$/);
  if (m) return `Num${m[1]}`;
  if (["Pause", "ScrollLock", "Insert", "Home", "End", "PageUp", "PageDown"].includes(code)) return code;
  return null;
}

// Klawisze, które mogą działać same (jak w hotkey.rs): F1–F24, Num0–9, Pause…, przyciski myszy.
function worksAlone(k) {
  return /^(F([1-9]|1[0-9]|2[0-4])|Num[0-9]|Mysz[345]|Pause|ScrollLock|Insert|Home|End|PageUp|PageDown)$/.test(k);
}

function finishHotkeyRecord() {
  if (hotkeyRec.down.size || !hotkeyRec.keys.length) return;
  // Modyfikatory na początek: Ctrl, Alt, Shift, potem reszta w kolejności naciśnięcia.
  const keys = [...MODIFIERS.filter((m) => hotkeyRec.keys.includes(m)), ...hotkeyRec.keys.filter((k) => !MODIFIERS.includes(k))];
  const ok = keys.length === 1 ? worksAlone(keys[0]) : keys.some((k) => !MODIFIERS.includes(k));
  if (ok) {
    stopHotkeyRecord();
    saveJarvis({ hotkey: keys });
    renderKeys(keys);
  } else {
    hotkeyRec.keys = [];
    renderKeys([], keys.length === 1 ? "Sam ten klawisz przeszkadzałby w pisaniu — dodaj Ctrl/Shift albo użyj F13–F24…" : "Dodaj literę, cyfrę albo F1–F24…");
  }
}

// Boczne / środkowy przycisk myszy jako przycisk do mówienia.
const MOUSE = { 1: "Mysz3", 3: "Mysz4", 4: "Mysz5" };
document.addEventListener("mousedown", (e) => {
  if (!hotkeyRec || !MOUSE[e.button]) return;
  e.preventDefault();
  e.stopPropagation();
  hotkeyRec.down.add(`mouse${e.button}`);
  if (!hotkeyRec.keys.includes(MOUSE[e.button]) && hotkeyRec.keys.length < 4) hotkeyRec.keys.push(MOUSE[e.button]);
  renderKeys(hotkeyRec.keys);
}, true);
document.addEventListener("mouseup", (e) => {
  if (!hotkeyRec || !MOUSE[e.button]) return;
  e.preventDefault();
  e.stopPropagation();
  hotkeyRec.down.delete(`mouse${e.button}`);
  finishHotkeyRecord();
}, true);
// Przyciski „wstecz/dalej” myszy nie mogą przewijać historii panelu.
document.addEventListener("auxclick", (e) => { if (hotkeyRec) e.preventDefault(); }, true);

function startHotkeyRecord() {
  hotkeyRec = { keys: [], down: new Set() };
  $("jv-keys").classList.add("recording");
  renderKeys([], "Naciśnij klawisz, kombinację albo przycisk myszy…");
  $("jv-record").textContent = "Anuluj";
}

function stopHotkeyRecord() {
  hotkeyRec = null;
  $("jv-keys").classList.remove("recording");
  $("jv-record").textContent = "Zmień";
  if (state.jarvis) renderKeys(state.jarvis.config.hotkey);
}

document.addEventListener("keydown", (e) => {
  if (!hotkeyRec) return;
  e.preventDefault();
  e.stopPropagation();
  if (e.key === "Escape") return stopHotkeyRecord();
  const name = keyName(e.code);
  if (!name) return;
  hotkeyRec.down.add(e.code);
  if (!hotkeyRec.keys.includes(name) && hotkeyRec.keys.length < 4) hotkeyRec.keys.push(name);
  renderKeys(hotkeyRec.keys);
}, true);

document.addEventListener("keyup", (e) => {
  if (!hotkeyRec) return;
  e.preventDefault();
  e.stopPropagation();
  hotkeyRec.down.delete(e.code);
  finishHotkeyRecord();
}, true);

$("jv-record").addEventListener("click", () => (hotkeyRec ? stopHotkeyRecord() : startHotkeyRecord()));
$("jv-enabled").addEventListener("change", (e) => saveJarvis({ enabled: e.target.checked }));
$("jv-voice").addEventListener("change", (e) => saveJarvis({ voice: e.target.checked }));
$("jv-mic").addEventListener("change", (e) => saveJarvis({ mic: e.target.value || null }));
$("jv-model").addEventListener("change", (e) => saveJarvis({ model: e.target.value }));
for (const b of $("jv-talk").children) b.addEventListener("click", () => saveJarvis({ talk_mode: b.dataset.mode }));
for (const b of $("jv-brain").children) b.addEventListener("click", () => saveJarvis({ brain: b.dataset.brain }));
$("llm-preset").addEventListener("change", (e) => {
  const preset = state.jarvis.llm_presets.find((p) => p.id === e.target.value);
  $("llm-models").replaceChildren();
  // Przy zmianie dostawcy podpowiadamy jego typowy model.
  saveJarvis({ llm_preset: e.target.value, llm_model: preset && preset.model ? preset.model : "" });
});
$("llm-url").addEventListener("change", (e) => saveJarvis({ llm_url: e.target.value.trim() }));
$("llm-model").addEventListener("change", (e) => saveJarvis({ llm_model: e.target.value.trim() }));
$("llm-fetch").addEventListener("click", () => {
  saveJarvis({ llm_url: $("llm-url").value.trim(), llm_model: $("llm-model").value.trim() });
  $("llm-fetch").disabled = true;
  $("llm-fetch").textContent = "Pobieram…";
  send({ c: "jarvis_models" });
});

$("up-check").addEventListener("click", () => send({ c: "update_check" }));
$("up-apply").addEventListener("click", () => send({ c: "update_apply" }));
$("update-pill").addEventListener("click", () => send({ c: "update_apply" }));
for (const b of $("up-mode").children) b.addEventListener("click", () => send({ c: "update_mode", mode: b.dataset.mode }));

// ── Overlay na stream ──
function overlayCfg() {
  return { show_command: true, show_actions: true, position: "bottom-left", scale: 100, ...(state.settings && state.settings.overlay) };
}
function renderOverlay() {
  const o = overlayCfg();
  $("ov-cmd").checked = o.show_command;
  $("ov-act").checked = o.show_actions;
  $("ov-pos").value = o.position;
  $("ov-scale").value = String(o.scale);
  const accent = ((state.settings && state.settings.accent) || "#8b6cf0").replace("#", "");
  const src = `/overlay?demo=1&bg=1&pos=${o.position}&accent=${accent}&cmd=${o.show_command ? 1 : 0}&act=${o.show_actions ? 1 : 0}&scale=${Math.round(o.scale * 0.55)}`;
  // Podgląd przeładowujemy tylko przy zmianie i gdy ustawienia są otwarte.
  if (!$("settings").hidden && $("ov-frame").dataset.src !== src) {
    $("ov-frame").dataset.src = src;
    $("ov-frame").src = src;
  }
}
function saveOverlay(patch) {
  const o = { ...overlayCfg(), ...patch };
  state.settings.overlay = o;
  renderOverlay();
  send({ c: "overlay", ...o });
}
$("ov-cmd").addEventListener("change", (e) => saveOverlay({ show_command: e.target.checked }));
$("ov-act").addEventListener("change", (e) => saveOverlay({ show_actions: e.target.checked }));
$("ov-pos").addEventListener("change", (e) => saveOverlay({ position: e.target.value }));
$("ov-scale").addEventListener("change", (e) => saveOverlay({ scale: Number(e.target.value) }));
$("ov-copy").addEventListener("click", async () => {
  const url = $("ov-url").textContent;
  try {
    await navigator.clipboard.writeText(url);
  } catch {
    const r = document.createRange();
    r.selectNodeContents($("ov-url"));
    getSelection().removeAllRanges();
    getSelection().addRange(r);
    document.execCommand("copy");
  }
  $("ov-copy").textContent = "Skopiowano ✓";
  setTimeout(() => ($("ov-copy").textContent = "Kopiuj"), 1600);
});
$("jv-wake").addEventListener("change", (e) => saveJarvis({ wake_word: e.target.checked }));
$("jv-wake-sens").addEventListener("change", (e) => saveJarvis({ wake_sensitivity: Number(e.target.value) }));
$("jv-mentions").addEventListener("change", (e) => saveJarvis({ read_mentions: e.target.checked }));
$("wake-train").addEventListener("click", () => { enroll.active = true; renderWake(); send({ c: "jarvis_enroll", action: "start" }); });
$("wake-cancel").addEventListener("click", () => send({ c: "jarvis_enroll", action: "cancel" }));
$("wake-reset").addEventListener("click", () => { saveJarvis({ wake_word: false }); send({ c: "jarvis_enroll", action: "reset" }); });
for (const field of document.querySelectorAll(".key-field")) {
  const provider = field.dataset.provider;
  const input = field.querySelector("input");
  const save = () => {
    const key = input.value.trim();
    if (!key) return input.focus();
    send({ c: "jarvis_key", provider, key });
    input.value = "";
  };
  field.querySelector(".save").addEventListener("click", save);
  input.addEventListener("keydown", (e) => { if (e.key === "Enter") save(); });
  field.querySelector(".remove").addEventListener("click", (e) => { e.preventDefault(); send({ c: "jarvis_key", provider, key: null }); });
}
for (const link of document.querySelectorAll("[data-open]")) {
  link.addEventListener("click", (e) => { e.preventDefault(); send({ c: "open", url: link.dataset.open }); });
}
$("jarvis-btn").addEventListener("click", (e) => { e.stopPropagation(); send({ c: "jarvis_activate" }); });
$("jv-test").addEventListener("click", () => { $("settings").hidden = true; send({ c: "jarvis_activate" }); });
$("jv-close").addEventListener("click", () => { clearTimeout(jv.hideTimer); hideJarvis(); });

setInterval(() => { renderStatus(); tickLogin(); }, 1000);
connect();
