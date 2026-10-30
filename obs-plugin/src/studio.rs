//! Trwałe dane Studia. Pliki lokalne, bez tokenów i bez zrzutów ekranu.
use crate::{
    config::{AlertsConfig, OverlayConfig},
    jarvis::JarvisConfig,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
};

pub fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
pub fn uid() -> String {
    use rand_core::RngCore;
    format!("{}-{:016x}", now(), rand_core::OsRng.next_u64())
}
pub fn text(s: &str, max: usize) -> String {
    s.trim().chars().take(max).collect()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VisionConfig {
    pub enabled: bool,
    pub source: String,
    pub speak: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub accent: String,
    pub animations: bool,
    pub timestamps: bool,
    pub mod_buttons: bool,
    pub jarvis: JarvisConfig,
    pub alerts: AlertsConfig,
    pub overlay: OverlayConfig,
    pub media_previews: String,
    pub vision: VisionConfig,
}
impl Profile {
    pub fn capture(name: &str, config: &crate::config::Config, vision: &VisionConfig) -> Self {
        Self {
            name: text(name, 40),
            accent: config.accent.clone(),
            animations: config.animations,
            timestamps: config.timestamps,
            mod_buttons: config.mod_buttons,
            jarvis: config.jarvis.clone(),
            alerts: config.alerts.clone(),
            overlay: config.overlay.clone(),
            media_previews: config.media_previews.clone(),
            vision: vision.clone(),
        }
    }
    pub fn apply(&self, config: &mut crate::config::Config) {
        config.accent = self.accent.clone();
        config.animations = self.animations;
        config.timestamps = self.timestamps;
        config.mod_buttons = self.mod_buttons;
        config.jarvis = self.jarvis.clone();
        config.alerts = self.alerts.clone();
        config.overlay = self.overlay.clone();
        config.media_previews = self.media_previews.clone();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatLine {
    pub at: i64,
    pub login: String,
    pub name: String,
    pub text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModEntry {
    pub at: i64,
    pub channel: String,
    pub login: String,
    pub action: String,
    pub detail: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Moment {
    pub id: String,
    pub at: i64,
    pub channel: String,
    pub label: String,
    pub elapsed: Option<i64>,
    pub chat: Vec<ChatLine>,
    pub replay: Option<String>,
    pub replay_error: Option<String>,
    pub title: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Ballots {
    pub channel: String,
    pub polls: Vec<Value>,
    pub predictions: Vec<Value>,
    pub updated: i64,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Data {
    pub moments: Vec<Moment>,
    pub profiles: Vec<Profile>,
    pub active_profile: Option<String>,
    pub vision: VisionConfig,
    pub replay_source: String,
    pub ballots: HashMap<String, Ballots>,
    pub moderation: VecDeque<ModEntry>,
    pub notes: HashMap<String, String>,
    pub recent: HashMap<String, VecDeque<ChatLine>>,
    pub questions: Vec<crate::games::Question>,
}
pub struct Store {
    pub data: Data,
    path: Option<PathBuf>,
    pub dirty: bool,
}
impl Store {
    pub fn load(demo: bool) -> Self {
        let path = (!demo)
            .then(crate::config::data_dir)
            .flatten()
            .map(|dir| dir.join("studio.json"));
        let mut data: Data = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        if data.questions.is_empty() {
            data.questions = crate::games::starter_questions();
        }
        Self {
            data,
            path,
            dirty: false,
        }
    }
    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(path) = &self.path else { return };
        let result = (|| -> anyhow::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec(&self.data)?)?;
            std::fs::rename(tmp, path)?;
            Ok(())
        })();
        match result {
            Ok(()) => self.dirty = false,
            Err(error) => tracing::warn!(%error,"Nie udało się zapisać Studia — ponowię zapis"),
        }
    }
    pub fn chat(&mut self, channel: &str, line: ChatLine) {
        let key = viewer_key(channel, &line.login);
        if !self.data.recent.contains_key(&key) && self.data.recent.len() >= 1000 {
            if let Some(oldest) = self
                .data
                .recent
                .iter()
                .min_by_key(|(_, v)| v.back().map(|l| l.at).unwrap_or(0))
                .map(|(key, _)| key.clone())
            {
                self.data.recent.remove(&oldest);
            }
        }
        let recent = self.data.recent.entry(key).or_default();
        recent.push_back(line);
        while recent.len() > 30 {
            recent.pop_front();
        }
        self.dirty = true;
    }
    pub fn moderation(&mut self, channel: &str, login: &str, action: &str, detail: &str) {
        self.data.moderation.push_front(ModEntry {
            at: now(),
            channel: channel.into(),
            login: login.to_lowercase(),
            action: action.into(),
            detail: text(detail, 300),
        });
        self.data.moderation.truncate(2000);
        self.dirty = true;
    }
    pub fn viewer(&self, channel: &str, login: &str) -> Value {
        let key = viewer_key(channel, login);
        serde_json::json!({"channel":channel,"login":login,"notes":self.data.notes.get(&key),"messages":self.data.recent.get(&key),"moderation":self.data.moderation.iter().filter(|e|e.channel==channel&&e.login.eq_ignore_ascii_case(login)).take(50).collect::<Vec<_>>()})
    }
    pub fn public(&self) -> Value {
        serde_json::json!({"moments":self.data.moments,"profiles":self.data.profiles.iter().map(|p|&p.name).collect::<Vec<_>>(),"active_profile":self.data.active_profile,"vision":self.data.vision,"replay_source":self.data.replay_source,"ballots":self.data.ballots,"questions":self.data.questions})
    }
}
pub fn viewer_key(channel: &str, login: &str) -> String {
    format!(
        "{}:{}",
        channel.to_ascii_lowercase(),
        login.to_ascii_lowercase()
    )
}

/// Odtwarza tylko plik zapisany przez OBS; po końcu/przerwaniu przywraca źródło.
pub async fn play_replay(
    obs: crate::obs::ObsHandle,
    source: String,
    path: String,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {
    use serde_json::json;
    let original = obs
        .request("GetInputSettings", json!({"inputName":source}))
        .await?;
    if original["inputKind"] != "ffmpeg_source" {
        return Err("Do powtórek wybierz źródło OBS typu Źródło multimediów, nie przechwytywanie gry ani VLC.".into());
    }
    let scene = obs
        .status()
        .current_scene
        .ok_or("OBS nie podał aktywnej sceny.")?;
    let item = obs
        .request(
            "GetSceneItemId",
            json!({"sceneName":scene,"sourceName":source}),
        )
        .await?;
    let item_id = item["sceneItemId"]
        .as_i64()
        .ok_or("Brak źródła powtórek na aktualnej scenie.")?;
    let visible = obs
        .request(
            "GetSceneItemEnabled",
            json!({"sceneName":scene,"sceneItemId":item_id}),
        )
        .await?["sceneItemEnabled"]
        .as_bool()
        .unwrap_or(false);
    obs.request("SetInputSettings",json!({"inputName":source,"inputSettings":{"local_file":path,"is_local_file":true,"looping":false,"restart_on_activate":true},"overlay":true})).await?;
    let outcome=async{
        obs.request("SetSceneItemEnabled",json!({"sceneName":scene,"sceneItemId":item_id,"sceneItemEnabled":true})).await?;
        obs.request("TriggerMediaInputAction",json!({"inputName":source,"mediaAction":"OBS_WEBSOCKET_MEDIA_INPUT_ACTION_RESTART"})).await?;
        let started=std::time::Instant::now();
        loop{
            tokio::select!{_ = stop.changed()=>return Ok(()),_ = tokio::time::sleep(std::time::Duration::from_millis(500))=>{}}
            let media=obs.request("GetMediaInputStatus",json!({"inputName":source})).await?;
            if matches!(media["mediaState"].as_str(),Some("OBS_MEDIA_STATE_ENDED"|"OBS_MEDIA_STATE_STOPPED")){return Ok(());}
            if media["mediaState"]=="OBS_MEDIA_STATE_ERROR"{return Err("OBS nie może odtworzyć pliku powtórki.".into());}
            if started.elapsed()>std::time::Duration::from_secs(180){return Err("Powtórka zatrzymana po limicie 3 minut.".into());}
        }
    }.await;
    // Nie nadpisuj ręcznej zmiany pliku dokonanej w OBS podczas odtwarzania.
    let current = obs
        .request("GetInputSettings", json!({"inputName":source}))
        .await?;
    if current["inputSettings"]["local_file"].as_str() == Some(&path) {
        obs.request(
            "SetSceneItemEnabled",
            json!({"sceneName":scene,"sceneItemId":item_id,"sceneItemEnabled":visible}),
        )
        .await?;
        obs.request(
            "SetInputSettings",
            json!({"inputName":source,"inputSettings":original["inputSettings"],"overlay":false}),
        )
        .await?;
    }
    outcome
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Command {
    Get,
    Viewer {
        channel: String,
        login: String,
    },
    ViewerNote {
        channel: String,
        login: String,
        notes: String,
    },
    Moment {
        label: String,
        replay: bool,
    },
    MomentDelete {
        id: String,
    },
    MomentTitle {
        id: String,
        title: String,
    },
    ReplayShow {
        id: String,
    },
    ReplayStop,
    ReplaySource {
        source: String,
    },
    ProfileSave {
        name: String,
    },
    ProfileApply {
        name: String,
    },
    ProfileDelete {
        name: String,
    },
    BallotsRefresh,
    PollCreate {
        title: String,
        choices: Vec<String>,
        seconds: u32,
    },
    PollEnd,
    PredictionCreate {
        title: String,
        choices: Vec<String>,
        seconds: u32,
    },
    PredictionEnd {
        mode: String,
        winner: Option<String>,
    },
    BallotShow {
        kind: String,
        index: usize,
        #[serde(default)]
        announce: bool,
    },
    VisionSources,
    VisionSettings {
        config: VisionConfig,
    },
    VisionCapture,
    VisionAnalyze {
        question: String,
    },
    QuestionsSave {
        questions: Vec<crate::games::Question>,
    },
    QuizStart {
        channel: String,
        seconds: u32,
        voice: bool,
    },
    QuizNext,
    QuizStop,
    BossStart {
        channel: String,
        name: String,
        hp: u32,
        seconds: u32,
        voice: bool,
        victory_scene: String,
    },
    BossStop,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn studio_data_survives_restart() {
        let dir = std::env::temp_dir().join(format!("limbo-studio-{}", uid()));
        let path = dir.join("studio.json");
        let mut store = Store {
            data: Data::default(),
            path: Some(path.clone()),
            dirty: true,
        };
        store
            .data
            .notes
            .insert(viewer_key("a", "Nick"), "Notatka moderatora".into());
        store.data.vision = VisionConfig {
            enabled: false,
            source: "Gra".into(),
            speak: false,
        };
        store.save();
        assert!(!store.dirty);
        let data: Data = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(data.notes["a:nick"], "Notatka moderatora");
        assert_eq!(data.vision.source, "Gra");
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
    #[test]
    fn profiles_preserve_twitch_channels_obs_and_private_keys() {
        let mut config = crate::config::Config::default();
        config.channels = vec!["streamer".into()];
        config.obs.port = 9999;
        let profile = Profile::capture("Gra", &config, &VisionConfig::default());
        config.channels = vec!["inny".into()];
        config.obs.port = 4455;
        profile.apply(&mut config);
        assert_eq!(config.channels, ["inny"]);
        assert_eq!(config.obs.port, 4455);
        assert!(
            !serde_json::to_string(&profile)
                .unwrap()
                .contains("access_token")
        );
    }
    #[test]
    fn viewer_history_is_channel_scoped_and_bounded() {
        let mut store = Store::load(true);
        for i in 0..50 {
            store.chat(
                "a",
                ChatLine {
                    at: i,
                    login: "nick".into(),
                    name: "Nick".into(),
                    text: i.to_string(),
                },
            );
        }
        store.moderation("a", "nick", "timeout", "60 s");
        assert_eq!(
            store.viewer("a", "nick")["messages"]
                .as_array()
                .unwrap()
                .len(),
            30
        );
        assert!(
            store.viewer("b", "nick")["moderation"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            !store.public().to_string().contains("timeout"),
            "prywatne dane moderacji nie trafiają do stanu Studia"
        );
    }
}
