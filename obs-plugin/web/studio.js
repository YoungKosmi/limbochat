"use strict";
const $ = (id) => document.getElementById(id);
let ws,
  auth = {},
  channels = [],
  settings = {},
  studio = {},
  publicState = {},
  obs = {},
  bank = [],
  bankDirty = false,
  hasFrame = false,
  viewer = null,
  tab = "moments",
  initialized = false,
  engineVersion = null,
  refreshForms = false,
  visionKey = "";
const titles = {
  moments: "Momenty i powtórki",
  polls: "Ankiety i predykcje",
  quiz: "Teleturniej Limbo",
  boss: "Czat kontra boss",
  vision: "Limbo widzi grę",
  viewer: "Karta widza",
  profiles: "Profile streama",
  questions: "Baza pytań",
};
const params = new URLSearchParams(location.search);
function element(tag, text, className) {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  if (className) node.className = className;
  return node;
}
function notify(text, error = false) {
  $("toast").hidden = false;
  $("toast").textContent = text;
  $("toast").classList.toggle("error", error);
}
function send(data) {
  if (ws?.readyState !== WebSocket.OPEN) {
    notify("Brak połączenia z silnikiem LiMBOCHAT.", true);
    return false;
  }
  ws.send(JSON.stringify(data));
  return true;
}
function command(action, data = {}) {
  return send({ c: "studio", command: { action, ...data } });
}
function number(id, min, max) {
  const value = Number($(id).value);
  if (!Number.isInteger(value) || value < min || value > max)
    throw Error(`Wpisz liczbę od ${min} do ${max}.`);
  return value;
}
function bind(id, fn) {
  $(id).addEventListener("click", () => {
    try {
      fn();
    } catch (error) {
      notify(error.message, true);
    }
  });
}
function button(text, fn, className = "") {
  const node = element("button", text, className);
  node.onclick = () => {
    try {
      fn();
    } catch (error) {
      notify(error.message, true);
    }
  };
  return node;
}
function empty(parent, text) {
  parent.replaceChildren(element("div", text, "empty"));
}
function switchTab(next) {
  tab = next;
  for (const node of document.querySelectorAll("[data-panel]"))
    node.hidden = node.dataset.panel !== tab;
  for (const node of document.querySelectorAll("[data-tab]"))
    node.classList.toggle("active", node.dataset.tab === tab);
  $("page-title").textContent = titles[tab];
  if (tab === "polls" && !studio.busy?.includes("ballots"))
    command("ballots_refresh");
}
for (const node of document.querySelectorAll("[data-tab]"))
  node.onclick = () => switchTab(node.dataset.tab);
function fillSelect(node, items, value, placeholder) {
  const previous = value ?? node.value;
  node.replaceChildren();
  if (placeholder) node.append(new Option(placeholder, ""));
  for (const item of items) node.append(new Option(item, item));
  if (items.includes(previous)) node.value = previous;
}
function updateChannels() {
  for (const node of document.querySelectorAll(".channel-select"))
    fillSelect(
      node,
      channels.map((c) => c.login),
      node.value || params.get("channel") || settings.active,
    );
  fillSelect(
    $("boss-victory-scene"),
    obs.scenes || [],
    undefined,
    "Bez zmiany sceny",
  );
}
function connect() {
  ws = new WebSocket(`ws://${location.host}/ws`);
  ws.onopen = () => {
    $("connection").textContent = "Silnik połączony";
    $("connection").classList.add("good");
  };
  ws.onclose = () => {
    $("connection").textContent = "Rozłączono";
    $("connection").classList.remove("good");
    setTimeout(connect, 1500);
  };
  ws.onmessage = (event) => {
    try {
      receive(JSON.parse(event.data));
    } catch (error) {
      notify(
        "Nie udało się odczytać odpowiedzi panelu: " + error.message,
        true,
      );
    }
  };
}
function receive(message) {
  switch (message.t) {
    case "snapshot":
      if (engineVersion && engineVersion !== message.version)
        return location.reload();
      engineVersion = message.version;
      $("demo-mode").hidden = !message.demo;
      auth = message.auth;
      channels = message.channels;
      settings = message.settings;
      obs = message.obs;
      $("version").textContent = message.version;
      $("account").textContent =
        auth.state === "logged_in"
          ? `Twitch: ${auth.login}`
          : "Twitch: niezalogowany";
      updateChannels();
      renderObs();
      applyStudio(message.studio || {});
      if (!initialized) {
        initialized = true;
        if (params.get("login")) {
          $("viewer-login").value = params.get("login");
          switchTab("viewer");
          loadViewer();
        }
      }
      break;
    case "settings":
      settings = message;
      break;
    case "auth":
      auth = message;
      $("account").textContent =
        auth.state === "logged_in"
          ? `Twitch: ${auth.login}`
          : "Twitch: niezalogowany";
      break;
    case "obs":
      obs = message;
      renderObs();
      updateChannels();
      break;
    case "message": {
      const c = channels.find((c) => c.login === message.m.channel);
      if (c) {
        c.messages.push(message.m);
        if (c.messages.length > 300) c.messages.shift();
      }
      break;
    }
    case "mod_status": {
      const c = channels.find((c) => c.login === message.channel);
      if (c) c.can_moderate = message.can_moderate;
      break;
    }
    case "channel_added":
      if (!channels.some((c) => c.login === message.channel)) {
        channels.push({
          login: message.channel,
          messages: [],
          can_moderate: false,
        });
        updateChannels();
      }
      break;
    case "channel_removed":
      channels = channels.filter((c) => c.login !== message.channel);
      updateChannels();
      break;
    case "notice":
      if (message.error) notify(message.text, true);
      break;
    case "studio":
      switch (message.event) {
        case "state":
          applyStudio(message.data);
          break;
        case "public":
          publicState = message.data;
          renderGames();
          break;
        case "viewer":
          renderViewer(message.data);
          break;
        case "sources": {
          const current = $("vision-source").value || studio.vision?.source;
          fillSelect(
            $("vision-source"),
            message.data,
            current,
            "Wybierz źródło",
          );
          break;
        }
        case "frame":
          $("vision-preview").src = message.data.image;
          $("vision-preview").hidden = false;
          $("vision-empty").hidden = true;
          hasFrame = true;
          updateBusy();
          break;
        case "vision":
          $("vision-answer").hidden = false;
          $("vision-answer").textContent = message.data.text;
          break;
        case "error":
          notify(message.data.text, true);
          break;
      }
      break;
  }
}
function renderObs() {
  $("obs-state").textContent = obs.connected
    ? `OBS: ${obs.current_scene || "połączony"}`
    : "OBS rozłączony";
  $("obs-state").classList.toggle("good", !!obs.connected);
}
function applyStudio(data) {
  const changed = visionKey !== JSON.stringify(data.vision);
  visionKey = JSON.stringify(data.vision);
  studio = data;
  publicState = data.public || {};
  if (!bankDirty) {
    bank = structuredClone(data.questions || []);
    renderQuestions();
  }
  if (changed || !initialized || refreshForms) {
    const cfg = data.vision || {};
    $("vision-enabled").checked = !!cfg.enabled;
    $("vision-speak").checked = !!cfg.speak;
    if (
      cfg.source &&
      !Array.from($("vision-source").options).some(
        (o) => o.value === cfg.source,
      )
    )
      $("vision-source").add(new Option(cfg.source, cfg.source));
    $("vision-source").value = cfg.source || "";
    hasFrame = false;
    $("vision-preview").hidden = true;
    $("vision-empty").hidden = false;
    $("replay-source").value = data.replay_source || "";
    refreshForms = false;
  }
  renderMoments();
  renderProfiles();
  renderBallots();
  renderGames();
  updateBusy();
}
function updateBusy() {
  const busy = studio.busy || [];
  $("replay-save").disabled = busy.includes("replay");
  $("vision-capture").disabled = busy.includes("capture");
  $("vision-analyze").disabled = !hasFrame || busy.includes("vision");
  $("vision-sources").disabled = busy.includes("sources");
  $("ballots-refresh").disabled = busy.includes("ballots");
  $("ballot-create").disabled = busy.includes("ballots");
}
function elapsed(seconds) {
  if (seconds == null) return "poza aktywnym streamem";
  const n = Math.max(0, Math.floor(seconds));
  return `${Math.floor(n / 3600)
    .toString()
    .padStart(2, "0")}:${Math.floor((n / 60) % 60)
    .toString()
    .padStart(2, "0")}:${(n % 60).toString().padStart(2, "0")}`;
}
function date(ms) {
  return new Date(ms).toLocaleString("pl-PL");
}
function copy(text) {
  navigator.clipboard.writeText(text).then(
    () => notify("Skopiowano."),
    () => notify("Nie udało się skopiować — zaznacz tekst ręcznie.", true),
  );
}
function renderMoments() {
  const list = $("moments-list");
  list.replaceChildren();
  if (!studio.moments?.length)
    return empty(list, "Pierwszy ciekawy moment czeka na swój znacznik.");
  for (const moment of studio.moments) {
    const card = element("article", undefined, "entry");
    card.append(
      element(
        "div",
        `${elapsed(moment.elapsed)} · #${moment.channel} · ${date(moment.at)}`,
        "meta",
      ),
      element("h3", moment.title || moment.label),
    );
    const actions = element("div", undefined, "actions");
    actions.append(
      button("Kopiuj do montażu", () =>
        copy(
          `${elapsed(moment.elapsed)} — ${moment.title || moment.label}\n${moment.chat.map((m) => `${m.name}: ${m.text}`).join("\n")}`,
        ),
      ),
      button("Zmień tytuł", () => {
        const title = prompt("Tytuł momentu", moment.title || moment.label);
        if (title !== null) command("moment_title", { id: moment.id, title });
      }),
      button("Usuń znacznik", () => {
        if (confirm("Usunąć znacznik? Plik nagrania zostanie zachowany."))
          command("moment_delete", { id: moment.id });
      }),
    );
    if (moment.replay) {
      actions.append(
        button(
          "▶ Pokaż powtórkę",
          () => command("replay_show", { id: moment.id }),
          "primary",
        ),
        button("Kopiuj ścieżkę", () => copy(moment.replay)),
      );
      card.append(element("p", moment.replay, "hint"));
    } else if (moment.replay_error)
      card.append(element("p", moment.replay_error, "hint"));
    const details = element("details"),
      summary = element("summary", `Fragment czatu (${moment.chat.length})`);
    details.append(summary);
    for (const message of moment.chat)
      details.append(
        element("p", `${message.name}: ${message.text}`, "note-message"),
      );
    card.append(actions, details);
    list.append(card);
  }
}
bind("moment-save", () =>
  command("moment", { label: $("moment-label").value, replay: false }),
);
bind("replay-save", () =>
  command("moment", { label: $("moment-label").value, replay: true }),
);
bind("replay-source-save", () =>
  command("replay_source", { source: $("replay-source").value }),
);
bind("replay-stop", () => command("replay_stop"));
bind("moments-copy", () =>
  copy(
    (studio.moments || [])
      .map((m) => `${elapsed(m.elapsed)} — ${m.title || m.label}`)
      .join("\n"),
  ),
);
function renderProfiles() {
  const list = $("profiles-list");
  list.replaceChildren();
  $("active-profile").textContent = studio.active_profile
    ? `Ostatnio wybrany: ${studio.active_profile}`
    : "Nie wybrano profilu.";
  if (!studio.profiles?.length)
    return empty(list, "Zapisz pierwszy profil z obecnych ustawień.");
  for (const name of studio.profiles) {
    const row = element("div", undefined, "entry");
    row.append(
      element("h3", name),
      button(
        "Zastosuj",
        () => {
          refreshForms = true;
          command("profile_apply", { name });
        },
        "primary",
      ),
      button("Usuń", () => {
        if (confirm(`Usunąć profil „${name}”?`))
          command("profile_delete", { name });
      }),
    );
    list.append(row);
  }
}
bind("profile-save", () => {
  const name = $("profile-name").value.trim();
  if (!name) throw Error("Wpisz nazwę profilu.");
  if (
    studio.profiles?.includes(name) &&
    !confirm("Zastąpić istniejący profil bieżącymi ustawieniami?")
  )
    return;
  command("profile_save", { name });
});
function ownBallots() {
  return studio.ballots?.[auth.login];
}
function renderBallots() {
  const history = ownBallots();
  $("ballots-status").textContent = history
    ? `Wyniki #${history.channel} · odświeżono ${date(history.updated)}. Historia jest zapisana lokalnie.`
    : "Zaloguj konto nadawcy i odśwież wyniki. Historia zbiera do 20 ostatnich ankiet i predykcji zwróconych przez Twitcha.";
  for (const kind of ["poll", "prediction"]) {
    const list = $(kind === "poll" ? "polls-list" : "predictions-list"),
      rows = history?.[kind === "poll" ? "polls" : "predictions"] || [];
    list.replaceChildren();
    if (!rows.length) empty(list, "Brak zapisanych wyników.");
    rows.forEach((entry, index) => {
      const card = element("article", undefined, "entry");
      card.append(
        element("div", entry.status || "", "meta"),
        element("h3", entry.title),
      );
      if (entry.winner) card.append(element("p", `Zwycięzca: ${entry.winner}`));
      for (const choice of entry.choices || entry.outcomes || []) {
        const value =
          kind === "poll"
            ? `${choice.votes || 0} głosów · ${choice.percent || 0}%`
            : `${choice.users || 0} osób · ${choice.channel_points || 0} punktów`;
        card.append(element("p", `${choice.title} — ${value}`));
        if (kind === "poll") {
          const bar = element("div", undefined, "bar"),
            fill = element("i");
          fill.style.width = `${Math.min(100, Math.max(0, choice.percent || 0))}%`;
          bar.append(fill);
          card.append(bar);
        }
      }
      card.append(
        button("Pokaż na overlayu", () =>
          command("ballot_show", { kind, index }),
        ),
        button("Limbo, ogłoś wynik", () =>
          command("ballot_show", { kind, index, announce: true }),
        ),
      );
      list.append(card);
    });
  }
  const active = history?.predictions?.find((p) =>
    ["ACTIVE", "LOCKED"].includes(p.status_raw),
  );
  fillSelect(
    $("prediction-winner"),
    (active?.outcomes || []).map((o) => o.title),
  );
  $("poll-end").disabled = !history?.polls?.some(
    (p) => p.status_raw === "ACTIVE",
  );
  for (const id of [
    "prediction-lock",
    "prediction-cancel",
    "prediction-resolve",
  ])
    $(id).disabled = !active;
}
bind("ballots-refresh", () => command("ballots_refresh"));
bind("ballot-hide", () => command("ballot_show", { kind: "off", index: 0 }));
bind("ballot-create", () => {
  const kind = $("ballot-kind").value,
    title = $("ballot-title").value.trim(),
    choices = $("ballot-choices")
      .value.split(/\r?\n/)
      .map((s) => s.trim())
      .filter(Boolean),
    seconds = number("ballot-seconds", kind === "poll" ? 15 : 30, 1800);
  if (
    !title ||
    title.length > (kind === "poll" ? 60 : 45) ||
    choices.length < 2 ||
    choices.length > (kind === "poll" ? 5 : 10) ||
    choices.some((c) => Array.from(c).length > 25)
  )
    throw Error(
      "Sprawdź tytuł i liczbę odpowiedzi. Każda odpowiedź może mieć do 25 znaków.",
    );
  if (
    confirm(
      `Utworzyć ${kind === "poll" ? "ankietę" : "predykcję"} na kanale ${auth.login || "zalogowanego konta"}?`,
    )
  )
    command(kind + "_create", { title, choices, seconds });
});
bind("poll-end", () => {
  if (confirm("Zakończyć aktywną ankietę?")) command("poll_end");
});
for (const mode of ["lock", "cancel", "resolve"])
  bind("prediction-" + mode, () => {
    const winner = $("prediction-winner").value;
    if (
      confirm(
        mode === "resolve"
          ? `Rozstrzygnąć predykcję na korzyść „${winner}”?`
          : `${mode === "cancel" ? "Anulować predykcję i zwrócić punkty" : "Zablokować obstawianie"}?`,
      )
    )
      command("prediction_end", {
        mode,
        winner: mode === "resolve" ? winner : null,
      });
  });
setInterval(() => {
  if (
    tab === "polls" &&
    $("ballots-auto").checked &&
    ws?.readyState === WebSocket.OPEN &&
    !studio.busy?.includes("ballots")
  )
    command("ballots_refresh");
}, 15000);
function gameCard(node, data, boss) {
  node.replaceChildren();
  if (!data) {
    empty(
      node,
      boss
        ? "Arena czeka na przeciwnika."
        : "Teleturniej jeszcze się nie rozpoczął.",
    );
    return;
  }
  node.append(
    element(
      "div",
      boss ? "WSPÓLNA WALKA CZATU" : `PYTANIE ${data.index}/${data.total}`,
      "game-eyebrow",
    ),
  );
  if (boss) {
    node.append(
      element("span", "👾", "boss-icon"),
      element("h2", data.name),
      element("div", `${data.hp}/${data.max_hp} HP`, "hp"),
    );
    const bar = element("div", undefined, "bar"),
      fill = element("i");
    fill.style.width = `${(data.hp / data.max_hp) * 100}%`;
    bar.append(fill);
    node.append(bar);
  }
  const phase = data.phase;
  node.append(
    element(
      "div",
      phase === "victory"
        ? "ZWYCIĘSTWO!"
        : phase === "defeat"
          ? "Boss wygrywa tę rundę"
          : phase === "finished"
            ? "Finał teleturnieju"
            : data.question,
      "game-title",
    ),
  );
  if (["question", "battle"].includes(phase)) {
    const timer = element("div", "", "countdown");
    timer.dataset.deadline = data.deadline;
    node.append(timer);
  }
  if (!["finished", "victory", "defeat"].includes(phase))
    for (const [index, choice] of data.choices.entries())
      node.append(
        element(
          "div",
          `${String.fromCharCode(65 + index)} · ${choice}`,
          "choice" + (data.correct === index ? " correct" : ""),
        ),
      );
  for (const [index, row] of (data.scores || []).entries()) {
    const score = element("div", undefined, "score");
    score.append(
      element("span", `${index + 1}. ${row.name}`),
      element("b", `${row.points} pkt`),
    );
    node.append(score);
  }
  if (!boss)
    node.append(
      element(
        "p",
        `${data.answers || 0} odpowiedzi · pierwsza odpowiedź widza się liczy`,
        "hint",
      ),
    );
}
function renderGames() {
  gameCard($("quiz-live"), publicState.quiz, false);
  gameCard($("boss-live"), publicState.boss, true);
  $("question-count").textContent =
    `Zapisany zestaw: ${studio.questions?.length || 0} pytań.`;
  tick();
}
function tick() {
  for (const node of document.querySelectorAll("[data-deadline]"))
    node.textContent =
      Math.max(
        0,
        Math.ceil((Number(node.dataset.deadline) - Date.now()) / 1000),
      ) + " s";
}
setInterval(tick, 250);
bind("quiz-start", () =>
  command("quiz_start", {
    channel: $("quiz-channel").value,
    seconds: number("quiz-seconds", 10, 120),
    voice: $("quiz-voice").checked,
  }),
);
bind("quiz-next", () => command("quiz_next"));
bind("quiz-stop", () => command("quiz_stop"));
bind("boss-start", () => {
  const victory_scene = $("boss-victory-scene").value;
  if (
    victory_scene &&
    !confirm(
      `Zwycięstwo czatu przełączy OBS na scenę „${victory_scene}”. Rozpocząć?`,
    )
  )
    return;
  command("boss_start", {
    channel: $("boss-channel").value,
    name: $("boss-name").value,
    hp: number("boss-hp", 20, 1000),
    seconds: number("boss-seconds", 30, 600),
    voice: $("boss-voice").checked,
    victory_scene,
  });
});
bind("boss-stop", () => command("boss_stop"));
function renderQuestions() {
  const list = $("questions-list");
  list.replaceChildren();
  for (const [index, q] of bank.entries()) {
    const card = element("div", undefined, "entry");
    card.append(element("h3", `${index + 1}. ${q.text}`));
    q.choices.forEach((choice, i) =>
      card.append(
        element(
          "div",
          `${String.fromCharCode(65 + i)} · ${choice}${i === q.correct ? " ✓" : ""}`,
          "choice" + (i === q.correct ? " correct" : ""),
        ),
      ),
    );
    card.append(
      button("Usuń", () => {
        bank.splice(index, 1);
        bankDirty = true;
        renderQuestions();
      }),
    );
    list.append(card);
  }
  if (!bank.length) empty(list, "Dodaj pytania do zestawu.");
}
bind("question-add", () => {
  const text = $("question-text").value.trim(),
    choices = $("question-choices")
      .value.split(/\r?\n/)
      .map((s) => s.trim())
      .filter(Boolean),
    correct = Number($("question-correct").value);
  if (
    !text ||
    choices.length < 2 ||
    choices.length > 6 ||
    correct >= choices.length ||
    choices.some((c) => Array.from(c).length > 100) ||
    bank.length >= 50
  )
    throw Error(
      "Dodaj pytanie i 2–6 odpowiedzi (do 100 znaków). Wskaż poprawną odpowiedź. Limit zestawu: 50 pytań.",
    );
  bank.push({ text, choices, correct });
  bankDirty = true;
  renderQuestions();
  $("question-text").value = "";
  $("question-choices").value = "";
});
bind("questions-save", () => {
  if (command("questions_save", { questions: bank })) bankDirty = false;
});
bind("questions-revert", () => {
  bank = structuredClone(studio.questions || []);
  bankDirty = false;
  renderQuestions();
});
bind("vision-sources", () => command("vision_sources"));
bind("vision-settings-save", () => {
  hasFrame = false;
  command("vision_settings", {
    config: {
      enabled: $("vision-enabled").checked,
      source: $("vision-source").value,
      speak: $("vision-speak").checked,
    },
  });
});
bind("vision-capture", () => {
  if (
    $("vision-source").value !== studio.vision?.source ||
    $("vision-enabled").checked !== studio.vision?.enabled
  )
    throw Error("Najpierw zapisz wybór źródła i zgodę na analizę.");
  hasFrame = false;
  $("vision-preview").hidden = true;
  $("vision-empty").hidden = false;
  command("vision_capture");
  updateBusy();
});
bind("vision-analyze", () => {
  const question = $("vision-question").value.trim();
  if (!question) throw Error("Wpisz pytanie do kadru.");
  command("vision_analyze", { question });
});
function loadViewer() {
  const channel = $("viewer-channel").value,
    login = $("viewer-login").value.trim().replace(/^@/, "").toLowerCase();
  if (!channel || !login) return notify("Wskaż kanał i nick.", true);
  command("viewer", { channel, login });
}
bind("viewer-load", loadViewer);
function renderViewer(data) {
  viewer = data;
  $("viewer-content").hidden = false;
  $("viewer-notes").value = data.notes || "";
  const history = $("viewer-history"),
    messages = $("viewer-messages"),
    actions = $("viewer-actions");
  history.replaceChildren();
  messages.replaceChildren();
  actions.replaceChildren();
  for (const entry of data.moderation || []) {
    const row = element("div", undefined, "entry");
    row.append(
      element("div", date(entry.at), "meta"),
      element("h3", entry.action),
      element("p", entry.detail || ""),
    );
    history.append(row);
  }
  if (!history.children.length)
    empty(
      history,
      "Nie zarejestrowano jeszcze działań dla tej osoby na tym kanale.",
    );
  for (const line of [...(data.messages || [])].reverse()) {
    const row = element("div", undefined, "entry");
    row.append(
      element("div", date(line.at), "meta"),
      element("p", `${line.name}: ${line.text}`, "note-message"),
    );
    messages.append(row);
  }
  if (!messages.children.length)
    empty(messages, "Brak wiadomości w lokalnej historii.");
  const channel = channels.find((c) => c.login === data.channel),
    message = [...(channel?.messages || [])]
      .reverse()
      .find((m) => m.login.toLowerCase() === data.login.toLowerCase());
  if (channel?.can_moderate && message?.user_id) {
    for (const [label, seconds] of [
      ["30 s", 30],
      ["10 min", 600],
      ["24 h", 86400],
    ])
      actions.append(
        button("Timeout " + label, () =>
          send({
            c: "timeout",
            channel: data.channel,
            user_id: message.user_id,
            login: data.login,
            seconds,
          }),
        ),
      );
    actions.append(
      button(
        "Ban",
        () => {
          if (confirm(`Zbanować ${data.login} na #${data.channel}?`))
            send({
              c: "ban",
              channel: data.channel,
              user_id: message.user_id,
              login: data.login,
            });
        },
        "danger",
      ),
      button("Unban", () =>
        send({
          c: "unban",
          channel: data.channel,
          user_id: message.user_id,
          login: data.login,
        }),
      ),
    );
  } else
    actions.append(
      element(
        "p",
        "Szybkie akcje wymagają uprawnień moderatora oraz wiadomości widza w bieżącej historii czatu.",
        "hint",
      ),
    );
}
bind("viewer-notes-save", () => {
  if (viewer)
    command("viewer_note", {
      channel: viewer.channel,
      login: viewer.login,
      notes: $("viewer-notes").value,
    });
});
for (const view of ["quiz", "boss", "ballot"]) {
  const url = `http://${location.host}/arena?view=${view}`;
  $(
    view === "ballot" ? "poll-overlay-url" : view + "-overlay-url",
  ).textContent = url;
  document.querySelector(`[data-copy-url="${view}"]`).onclick = () => copy(url);
}
connect();

$("vision-enabled").addEventListener("change", () => {
  if (!$("vision-enabled").checked) {
    hasFrame = false;
    $("vision-preview").hidden = true;
    command("vision_settings", {
      config: { ...(studio.vision || {}), enabled: false },
    });
    updateBusy();
  }
});
$("vision-source").addEventListener("change", () => {
  hasFrame = false;
  $("vision-preview").hidden = true;
  $("vision-empty").hidden = false;
  updateBusy();
});
