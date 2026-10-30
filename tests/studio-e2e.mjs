import http from "node:http";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import os from "node:os";
import { fileURLToPath } from "node:url";

const project = path.resolve(
    path.dirname(fileURLToPath(import.meta.url)),
    "..",
);
const work =
    process.env.LIMBOCHAT_TEST_DIR ||
    fs.mkdtempSync(path.join(os.tmpdir(), "limbo-studio-e2e-"));
try {
    const health = await fetch("http://127.0.0.1:47250/control/health");
    if (health.ok) throw Error("PORT_IN_USE");
} catch (error) {
    if (error.message === "PORT_IN_USE")
        throw Error("Close LiMBOCHAT before running its isolated E2E test.");
}
const profile = path.join(work, "studio-preview-profile");
const engineDir = path.join(work, "studio-preview-engine");
fs.mkdirSync(profile, { recursive: true });
fs.mkdirSync(engineDir, { recursive: true });
const log = [];
const aiBodies = [];
let sourceSettings = { local_file: "C:\\old.mp4", looping: true };
let sourceVisible = false;
const screenshot =
    "data:image/png;base64," +
    fs
        .readFileSync(path.join(project, "assets/demo-kappa.png"))
        .toString("base64");
const replayPath = "C:\\Clips\\studio-fixture.mkv";
function frame(text, opcode = 1) {
    const bytes = Buffer.from(text);
    let head;
    if (bytes.length < 126) {
        head = Buffer.from([0x80 | opcode, bytes.length]);
    } else if (bytes.length < 65536) {
        head = Buffer.alloc(4);
        head[0] = 0x80 | opcode;
        head[1] = 126;
        head.writeUInt16BE(bytes.length, 2);
    } else {
        head = Buffer.alloc(10);
        head[0] = 0x80 | opcode;
        head[1] = 127;
        head.writeBigUInt64BE(BigInt(bytes.length), 2);
    }
    return Buffer.concat([head, bytes]);
}
const obsServer = http.createServer();
obsServer.on("upgrade", (req, socket) => {
    const accept = crypto
        .createHash("sha1")
        .update(
            req.headers["sec-websocket-key"] +
                "258EAFA5-E914-47DA-95CA-C5AB0DC85B11",
        )
        .digest("base64");
    socket.write(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: " +
            accept +
            "\r\n\r\n",
    );
    const send = (data) => socket.write(frame(JSON.stringify(data)));
    send({ op: 0, d: { obsWebSocketVersion: "5.6.0-fixture", rpcVersion: 1 } });
    let pending = Buffer.alloc(0);
    socket.on("data", (chunk) => {
        pending = Buffer.concat([pending, chunk]);
        while (pending.length >= 2) {
            const opcode = pending[0] & 15,
                masked = !!(pending[1] & 128);
            let size = pending[1] & 127,
                offset = 2;
            if (size === 126) {
                if (pending.length < 4) return;
                size = pending.readUInt16BE(2);
                offset = 4;
            } else if (size === 127) {
                if (pending.length < 10) return;
                size = Number(pending.readBigUInt64BE(2));
                offset = 10;
            }
            if (size > 4_000_000) return socket.destroy();
            const maskAt = offset;
            if (masked) offset += 4;
            if (pending.length < offset + size) return;
            const bytes = Buffer.from(pending.subarray(offset, offset + size));
            if (masked)
                for (let i = 0; i < bytes.length; i++)
                    bytes[i] ^= pending[maskAt + (i % 4)];
            pending = pending.subarray(offset + size);
            if (opcode === 8) return socket.end();
            if (opcode === 9) {
                socket.write(frame(bytes, 10));
                continue;
            }
            if (opcode !== 1) continue;
            const msg = JSON.parse(bytes.toString());
            if (msg.op === 1) {
                send({ op: 2, d: { negotiatedRpcVersion: 1 } });
                continue;
            }
            if (msg.op !== 6) continue;
            const {
                requestType: kind,
                requestId,
                requestData: data = {},
            } = msg.d;
            log.push({ kind, data });
            let response = {};
            switch (kind) {
                case "GetSceneList":
                    response = {
                        scenes: [
                            { sceneName: "Gra", sceneIndex: 0 },
                            { sceneName: "Zwycięstwo", sceneIndex: 1 },
                        ],
                        currentProgramSceneName: "Gra",
                    };
                    break;
                case "GetStreamStatus":
                    response = { outputActive: false };
                    break;
                case "GetRecordStatus":
                    response = { outputActive: false };
                    break;
                case "GetReplayBufferStatus":
                    response = { outputActive: true };
                    break;
                case "GetInputList":
                    response = {
                        inputs: [
                            { inputName: "Gra" },
                            { inputName: "Powtórka" },
                        ],
                    };
                    break;
                case "GetSourceScreenshot":
                    assert.equal(data.sourceName, "Gra");
                    response = { imageData: screenshot };
                    break;
                case "GetInputSettings":
                    response = {
                        inputKind: "ffmpeg_source",
                        inputSettings: sourceSettings,
                    };
                    break;
                case "SetInputSettings":
                    sourceSettings = data.overlay
                        ? { ...sourceSettings, ...data.inputSettings }
                        : data.inputSettings;
                    break;
                case "GetSceneItemId":
                    response = { sceneItemId: 7 };
                    break;
                case "GetSceneItemEnabled":
                    response = { sceneItemEnabled: sourceVisible };
                    break;
                case "SetSceneItemEnabled":
                    sourceVisible = data.sceneItemEnabled;
                    break;
                case "GetMediaInputStatus":
                    response = { mediaState: "OBS_MEDIA_STATE_ENDED" };
                    break;
            }
            send({
                op: 7,
                d: {
                    requestId,
                    requestType: kind,
                    requestStatus: { result: true, code: 100 },
                    responseData: response,
                },
            });
            if (kind === "SaveReplayBuffer")
                setTimeout(
                    () =>
                        send({
                            op: 5,
                            d: {
                                eventType: "ReplayBufferSaved",
                                eventData: { savedReplayPath: replayPath },
                            },
                        }),
                    40,
                );
        }
    });
    socket.on("error", () => {});
});
await new Promise((resolve) => obsServer.listen(0, "127.0.0.1", resolve));
const ai = http.createServer(async (req, res) => {
    let body = "";
    for await (const chunk of req) body += chunk;
    assert.equal(req.url, "/v1/chat/completions");
    assert.equal(req.headers.authorization, "Bearer fixture-not-real");
    const data = JSON.parse(body);
    aiBodies.push(data);
    assert.equal(data.model, "vision-fixture");
    assert(!data.tools);
    assert(
        data.messages[1].content[1].image_url.url.startsWith(
            "data:image/png;base64,",
        ),
    );
    res.writeHead(200, { "content-type": "application/json" });
    res.end(
        JSON.stringify({
            choices: [
                {
                    message: {
                        role: "assistant",
                        content:
                            "Na testowym kadrze widzę obrazek demonstracyjny. To odpowiedź lokalnej atrapy AI.",
                    },
                },
            ],
        }),
    );
});
await new Promise((resolve) => ai.listen(0, "127.0.0.1", resolve));
fs.writeFileSync(
    path.join(profile, "obs-dock.json"),
    JSON.stringify({
        jarvis: {
            enabled: false,
            voice: false,
            brain: "custom",
            llm_preset: "custom",
            llm_url: "http://127.0.0.1:" + ai.address().port + "/v1",
            llm_model: "vision-fixture",
        },
        obs: { enabled: true },
        memory: { enabled: false, learn: false },
        summary: { enabled: false },
        alerts: {rhymes:false,rhymes_voice:false,rhymes_chat:false},
        greetings: {creator:false,voice:false,overlay:false},
    }),
);
const out = fs.openSync(path.join(work, "studio-preview.log"), "w");
const child = spawn(
    path.join(
        project,
        "target/debug/limbochat-engine" +
            (process.platform === "win32" ? ".exe" : ""),
    ),
    [],
    {
        windowsHide: true,
        env: {
            ...process.env,
            LIMBOCHAT_DEMO: "1",
            LIMBOCHAT_DATA_DIR: profile,
            LIMBOCHAT_ENGINE_DIR: engineDir,
            LIMBOCHAT_OBS_URL: "ws://127.0.0.1:" + obsServer.address().port,
            LIMBOCHAT_OBS_PASSWORD: "fixture-not-real",
            LIMBOCHAT_LLM_KEY: "fixture-not-real",
            LIMBOCHAT_GEMINI_KEY: "fixture-not-real",
            LIMBOCHAT_GROQ_KEY: "fixture-not-real",
        },
        stdio: ["ignore", out, out],
    },
);
fs.writeFileSync(path.join(work, "studio-preview.pid"), String(child.pid));
class Client {
    constructor(url) {
        this.messages = [];
        this.ws = new WebSocket(url);
        this.ws.addEventListener("message", (e) =>
            this.messages.push(JSON.parse(e.data)),
        );
    }
    async opened() {
        await new Promise((resolve, reject) => {
            this.ws.addEventListener("open", resolve, { once: true });
            this.ws.addEventListener("error", reject, { once: true });
        });
        return this;
    }
    send(data) {
        this.ws.send(JSON.stringify(data));
    }
    async wait(predicate, after = 0) {
        const start = Date.now();
        while (Date.now() - start < 8000) {
            const match = this.messages.slice(after).find(predicate);
            if (match) return match;
            await new Promise((r) => setTimeout(r, 15));
        }
        throw Error(
            "No expected event. Last events: " +
                JSON.stringify(this.messages.slice(-4)).slice(0, 300),
        );
    }
    async studio(action, data = {}, predicate) {
        const after = this.messages.length;
        this.send({ c: "studio", command: { action, ...data } });
        return this.wait(
            predicate || ((m) => m.t === "studio" && m.event === "state"),
            after,
        );
    }
}
let panel, arena;
try {
    for (let i = 0; i < 80; i++) {
        try {
            const health = await fetch("http://127.0.0.1:47250/control/health");
            if (health.ok) break;
        } catch {}
        await new Promise((r) => setTimeout(r, 100));
    }
    panel = await new Client("ws://127.0.0.1:47250/ws").opened();
    const snapshot = await panel.wait((m) => m.t === "snapshot");
    assert(snapshot.demo);
    assert.equal(snapshot.studio.questions.length, 6);
    assert(!snapshot.studio.vision.enabled);
    await panel.wait(
        (m) =>
            (m.t === "obs" && m.connected) ||
            (m.t === "snapshot" && m.obs.connected),
    );
    const html = await (await fetch("http://127.0.0.1:47250/studio")).text();
    assert(html.includes("Limbo widzi grę"));
    assert(html.includes("Teleturniej"));
    arena = await new Client("ws://127.0.0.1:47250/arena/ws").opened();
    await arena.wait((m) => m.t === "studio" && m.event === "public");
    await panel.studio(
        "viewer_note",
        {
            channel: "demo",
            login: "testviewer",
            notes: "PRYWATNA_NOTATKA_FIXTURE",
        },
        (m) =>
            m.t === "studio" &&
            m.event === "viewer" &&
            m.data.notes === "PRYWATNA_NOTATKA_FIXTURE",
    );
    const other = await panel.studio(
        "viewer",
        { channel: "drugi_kanal", login: "testviewer" },
        (m) =>
            m.t === "studio" &&
            m.event === "viewer" &&
            m.data.channel === "drugi_kanal",
    );
    assert.equal(other.data.notes, null);
    await panel.studio(
        "profile_save",
        { name: "Gra testowa" },
        (m) =>
            m.t === "studio" &&
            m.event === "state" &&
            m.data.profiles.includes("Gra testowa"),
    );
    panel.send({ c: "theme", accent: "#ff0000" });
    await panel.wait((m) => m.t === "settings" && m.accent === "#ff0000");
    const marker = panel.messages.length;
    panel.send({
        c: "studio",
        command: { action: "profile_apply", name: "Gra testowa" },
    });
    await panel.wait(
        (m) => m.t === "settings" && m.accent === "#8b6cf0",
        marker,
    );
    const moment = await panel.studio(
        "moment",
        { label: "Testowy moment", replay: false },
        (m) =>
            m.t === "studio" &&
            m.event === "state" &&
            m.data.moments[0]?.label === "Testowy moment",
    );
    assert(moment.data.moments[0].elapsed > 0);
    assert(moment.data.moments[0].chat.length <= 20);
    const saved = await panel.studio(
        "moment",
        { label: "Testowa powtórka", replay: true },
        (m) =>
            m.t === "studio" &&
            m.event === "state" &&
            m.data.moments[0]?.replay === replayPath,
    );
    const replayId = saved.data.moments[0].id;
    await panel.studio("replay_source", { source: "Powtórka" });
    await panel.studio(
        "replay_show",
        { id: replayId },
        (m) =>
            m.t === "studio" &&
            m.event === "state" &&
            !m.data.busy.includes("playback"),
    );
    assert.equal(sourceSettings.local_file, "C:\\old.mp4");
    assert.equal(sourceVisible, false);
    await panel.studio(
        "ballots_refresh",
        {},
        (m) =>
            m.t === "studio" &&
            m.event === "state" &&
            m.data.ballots.ty?.polls.length > 0,
    );
    await panel.studio(
        "poll_create",
        { title: "Test głosowania", choices: ["Tak", "Nie"], seconds: 60 },
        (m) =>
            m.t === "studio" &&
            m.event === "state" &&
            m.data.ballots.ty?.polls[0]?.title === "Test głosowania",
    );
    await panel.studio(
        "poll_end",
        {},
        (m) =>
            m.t === "studio" &&
            m.event === "state" &&
            m.data.ballots.ty?.polls[0]?.status_raw === "TERMINATED",
    );
    await panel.studio(
        "ballot_show",
        { kind: "poll", index: 0 },
        (m) =>
            m.t === "studio" &&
            m.event === "public" &&
            m.data.ballot?.entry?.title === "Test głosowania",
    );
    await panel.studio(
        "quiz_start",
        { channel: "demo", seconds: 30, voice: false },
        (m) =>
            m.t === "studio" &&
            m.event === "public" &&
            m.data.quiz?.phase === "question",
    );
    let before = panel.messages.length;
    panel.send({ c: "send", channel: "demo", text: "A" });
    const score = await panel.wait(
        (m) =>
            m.t === "studio" &&
            m.event === "public" &&
            m.data.quiz?.answers === 1,
        before,
    );
    assert(score.data.quiz.scores[0].points >= 100);
    await panel.studio(
        "quiz_next",
        {},
        (m) =>
            m.t === "studio" &&
            m.event === "public" &&
            m.data.quiz?.phase === "reveal",
    );
    await panel.studio(
        "quiz_stop",
        {},
        (m) => m.t === "studio" && m.event === "public" && m.data.quiz === null,
    );
    await panel.studio(
        "boss_start",
        {
            channel: "demo",
            name: "Testowy boss",
            hp: 20,
            seconds: 60,
            voice: false,
            victory_scene: "",
        },
        (m) =>
            m.t === "studio" &&
            m.event === "public" &&
            m.data.boss?.phase === "battle",
    );
    before = panel.messages.length;
    panel.send({ c: "send", channel: "demo", text: "!atak A" });
    await panel.wait(
        (m) =>
            m.t === "studio" && m.event === "public" && m.data.boss?.hp === 10,
        before,
    );
    before = panel.messages.length;
    panel.send({ c: "send", channel: "demo", text: "!atak B" });
    await panel.wait(
        (m) =>
            m.t === "studio" &&
            m.event === "public" &&
            m.data.boss?.phase === "victory",
        before,
    );
    await panel.studio(
        "boss_stop",
        {},
        (m) => m.t === "studio" && m.event === "public" && m.data.boss === null,
    );
    await panel.studio(
        "vision_sources",
        {},
        (m) =>
            m.t === "studio" && m.event === "sources" && m.data.includes("Gra"),
    );
    assert.equal(log.filter((x) => x.kind === "GetSourceScreenshot").length, 0);
    assert.equal(aiBodies.length, 0);
    await panel.studio(
        "vision_settings",
        { config: { enabled: true, source: "Gra", speak: false } },
        (m) => m.t === "studio" && m.event === "state" && m.data.vision.enabled,
    );
    await panel.studio(
        "vision_capture",
        {},
        (m) =>
            m.t === "studio" &&
            m.event === "frame" &&
            m.data.image.startsWith("data:image/png;base64,"),
    );
    assert.equal(aiBodies.length, 0);
    await panel.studio(
        "vision_analyze",
        { question: "Co widać w teście?" },
        (m) =>
            m.t === "studio" &&
            m.event === "vision" &&
            m.data.text.includes("atrapy AI"),
    );
    assert.equal(aiBodies.length, 1);
    await panel.studio("vision_settings", {
        config: { enabled: false, source: "Gra", speak: false },
    });
    await panel.studio(
        "vision_analyze",
        { question: "Nie wysyłaj" },
        (m) => m.t === "notice" && m.error && m.text.includes("wyłączona"),
    );
    assert.equal(aiBodies.length, 1);
    assert(
        !JSON.stringify(arena.messages).includes("PRYWATNA_NOTATKA_FIXTURE"),
    );
    assert(!JSON.stringify(arena.messages).includes("imageData"));
    assert(
        !arena.messages.some((m) => m.t === "studio" && m.event !== "public"),
    );
    assert(!arena.messages.some((m) => m.t === "snapshot"));
    fs.writeFileSync(
        path.join(work, "studio-e2e-result.json"),
        JSON.stringify(
            {
                ok: true,
                obsRequests: log.map((x) => x.kind),
                aiRequests: aiBodies.length,
                tested: [
                    "viewer channel scope",
                    "profiles",
                    "moments",
                    "OBS replay saved event",
                    "replay source restore",
                    "polls",
                    "quiz scoring",
                    "boss victory",
                    "vision preview and explicit AI submit",
                    "vision opt out",
                    "overlay privacy",
                ],
            },
            null,
            2,
        ),
    );
    console.log(
        "OK: Studio E2E — viewer, profiles, moments, replay capture/playback, polls, quiz, boss, image analysis and overlay privacy.",
    );
} catch (error) {
    console.error(error);
    process.exitCode = 1;
} finally {
    panel?.ws.close();
    arena?.ws.close();
    if (process.argv.includes("--preview") && !process.exitCode) {
        console.log("Preview running at http://127.0.0.1:47250/studio");
    } else {
        child.kill();
        obsServer.close();
        ai.close();
        setTimeout(() => process.exit(process.exitCode || 0), 500).unref();
    }
}
