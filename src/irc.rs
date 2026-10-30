//! Połączenie z czatem Twitch przez IRC (crate `twitch-irc`).
//!
//! `twitch-irc` sam zarządza pulą połączeń, PING/PONG, automatycznym
//! reconnectem i ponownym dołączaniem do kanałów (także wielu naraz).
//! Nasza rola to tłumaczenie `ServerMessage` na zdarzenia aplikacji.
//!
//! Czat czytamy anonimowo (`justinfanXXXX`) — odczyt nie wymaga tokena,
//! a wysyłanie wiadomości i moderacja idą przez Helix.

use smallvec::SmallVec;
use tokio::sync::mpsc::UnboundedReceiver;
use twitch_irc::{
    ClientConfig, SecureTCPTransport, TwitchIRCClient,
    login::StaticLoginCredentials,
    message::{ClearChatAction, PrivmsgMessage, ServerMessage, UserNoticeEvent, UserNoticeMessage},
};

use crate::{
    emotes::{EmoteMap, EmoteScale, EmoteStore},
    events::{AlertKind, ChatAlert, UiEvent, sub_tier},
    parser::{self, Badge, ParsedMessage, TwitchEmoteRef},
};

pub type AnonymousClient = TwitchIRCClient<SecureTCPTransport, StaticLoginCredentials>;

/// Tworzy anonimowego klienta tylko do odczytu.
/// Połączenie TCP+TLS jest nawiązywane leniwie przy pierwszym `join`.
pub fn connect_anonymous() -> (UnboundedReceiver<ServerMessage>, AnonymousClient) {
    AnonymousClient::new(ClientConfig::default())
}

/// Ręczny test sieciowy: weryfikacja certyfikatu i połączenia TLS z Twitchem.
#[cfg(test)]
#[tokio::test]
#[ignore = "wymaga dostępu do irc.chat.twitch.tv:6697"]
async fn twitch_tls_handshake() {
    use twitch_irc::transport::tcp::{MakeConnection, TLS};
    tokio::time::timeout(std::time::Duration::from_secs(20), TLS::new_socket())
        .await
        .expect("przekroczono czas połączenia z Twitchem")
        .expect("połączenie TLS z Twitchem");
}

/// Wynik tłumaczenia wiadomości IRC.
#[derive(Debug)]
pub enum IrcEvent {
    /// Zdarzenie do przekazania wprost do UI.
    Ui(UiEvent),
    /// ROOMSTATE — znamy numeryczne ID kanału, więc backend może pobrać
    /// emotki 7TV. Zauważ: nie potrzebujemy do tego Helix ani tokena.
    RoomState { channel_login: String, channel_id: String },
}

pub fn translate(msg: ServerMessage, emotes: &EmoteStore, scale: EmoteScale) -> Option<IrcEvent> {
    let event = match msg {
        ServerMessage::Privmsg(m) => {
            let map = emotes.map_for(&m.channel_id);
            let bits = m.bits.filter(|b| *b > 0);
            let parsed = privmsg_to_parsed(m, &map, scale);
            match bits {
                // Wiadomość z bitsami = alert (z tekstem wiadomości w środku).
                Some(amount) => IrcEvent::Ui(UiEvent::Alert(Box::new(ChatAlert {
                    id: parsed.id.clone(),
                    channel_login: parsed.channel_login.clone(),
                    user_id: parsed.sender_id.clone(),
                    user_login: parsed.sender_login.clone(),
                    user_name: parsed.sender_name.clone(),
                    color: parsed.color,
                    kind: AlertKind::Bits { amount },
                    timestamp: parsed.timestamp,
                    message: Some(parsed),
                }))),
                None => IrcEvent::Ui(UiEvent::Message(Box::new(parsed))),
            }
        }
        ServerMessage::UserNotice(m) => {
            let map = emotes.map_for(&m.channel_id);
            return usernotice_to_event(m, &map, scale).map(IrcEvent::Ui);
        }
        ServerMessage::ClearMsg(m) => IrcEvent::Ui(UiEvent::MessageDeleted {
            channel: m.channel_login,
            message_id: m.message_id,
        }),
        ServerMessage::ClearChat(m) => {
            let channel = m.channel_login;
            IrcEvent::Ui(match m.action {
                ClearChatAction::ChatCleared => UiEvent::ChatCleared { channel },
                ClearChatAction::UserBanned { user_login, user_id } => {
                    UiEvent::UserCleared { channel, user_id, login: user_login, timeout_secs: None }
                }
                ClearChatAction::UserTimedOut { user_login, user_id, timeout_length } => UiEvent::UserCleared {
                    channel,
                    user_id,
                    login: user_login,
                    timeout_secs: Some(timeout_length.as_secs()),
                },
            })
        }
        ServerMessage::RoomState(m) => IrcEvent::RoomState {
            channel_login: m.channel_login,
            channel_id: m.channel_id,
        },
        ServerMessage::Notice(m) => {
            IrcEvent::Ui(UiEvent::Notice { channel: m.channel_login, text: m.message_text })
        }
        ServerMessage::Reconnect(_) => {
            // twitch-irc połączy się ponownie sam — tylko informujemy UI.
            IrcEvent::Ui(UiEvent::notice("Twitch zażądał ponownego połączenia…"))
        }
        // PING, JOIN, PART, USERSTATE, WHISPER itd. — ignorujemy.
        _ => return None,
    };
    Some(event)
}

fn privmsg_to_parsed(m: PrivmsgMessage, emotes: &EmoteMap, scale: EmoteScale) -> ParsedMessage {
    let twitch_emotes: SmallVec<[TwitchEmoteRef<'_>; 8]> = m
        .emotes
        .iter()
        .map(|e| TwitchEmoteRef { id: &e.id, code: &e.code })
        .collect();

    let segments = parser::tokenize(&m.message_text, &twitch_emotes, emotes, scale);
    drop(twitch_emotes);

    ParsedMessage {
        id: m.message_id,
        channel_login: m.channel_login,
        channel_id: m.channel_id,
        sender_id: m.sender.id,
        sender_login: m.sender.login,
        sender_name: m.sender.name,
        color: m.name_color.map(|c| [c.r, c.g, c.b]),
        badges: m
            .badges
            .into_iter()
            .map(|b| Badge { name: b.name, version: b.version })
            .collect(),
        is_action: m.is_action,
        timestamp: m.server_timestamp,
        segments,
        raw_text: m.message_text,
        deleted: false,
    }
}

/// USERNOTICE (sub, resub, gifty, raid, seria oglądania…) → alert.
/// Inne rodzaje z dołączonym tekstem (np. ogłoszenia) trafiają na czat jako zwykła wiadomość.
fn usernotice_to_event(m: UserNoticeMessage, emotes: &EmoteMap, scale: EmoteScale) -> Option<UiEvent> {
    match usernotice_kind(&m) {
        Some(kind) => usernotice_to_alert(m, kind, emotes, scale).map(|a| UiEvent::Alert(Box::new(a))),
        None => usernotice_message(&m, emotes, scale).map(|p| UiEvent::Message(Box::new(p))),
    }
}

fn usernotice_kind(m: &UserNoticeMessage) -> Option<AlertKind> {
    Some(match &m.event {
        UserNoticeEvent::SubOrResub { is_resub, cumulative_months, sub_plan, .. } => {
            AlertKind::Sub { tier: sub_tier(sub_plan), months: *cumulative_months, resub: *is_resub }
        }
        UserNoticeEvent::Raid { viewer_count, profile_image_url } => {
            AlertKind::Raid { viewers: *viewer_count, avatar: profile_image_url.clone() }
        }
        UserNoticeEvent::SubGift { is_sender_anonymous, recipient, sub_plan, .. } => AlertKind::GiftSub {
            tier: sub_tier(sub_plan),
            recipient: recipient.name.clone(),
            anonymous: *is_sender_anonymous,
        },
        UserNoticeEvent::SubMysteryGift { mass_gift_count, sender_total_gifts, sub_plan } => AlertKind::MassGift {
            tier: sub_tier(sub_plan),
            count: *mass_gift_count,
            total: Some(*sender_total_gifts),
            anonymous: false,
        },
        UserNoticeEvent::AnonSubMysteryGift { mass_gift_count, sub_plan } => {
            AlertKind::MassGift { tier: sub_tier(sub_plan), count: *mass_gift_count, total: None, anonymous: true }
        }
        UserNoticeEvent::GiftPaidUpgrade { gifter_name, .. } => AlertKind::GiftUpgrade { gifter: Some(gifter_name.clone()) },
        UserNoticeEvent::AnonGiftPaidUpgrade { .. } => AlertKind::GiftUpgrade { gifter: None },
        UserNoticeEvent::Ritual { ritual_name } if ritual_name == "new_chatter" => AlertKind::NewChatter,
        UserNoticeEvent::BitsBadgeTier { threshold } => AlertKind::BitsBadge { threshold: *threshold },
        _ if m.event_id == "viewermilestone" => {
            let tag = |k: &str| m.source.tags.0.get(k).and_then(|v| v.as_deref()).unwrap_or_default().to_owned();
            if tag("msg-param-category") != "watch-streak" {
                return None;
            }
            AlertKind::WatchStreak { streams: tag("msg-param-value").parse().unwrap_or(0) }
        }
        _ => return None,
    })
}

fn usernotice_message(m: &UserNoticeMessage, emotes: &EmoteMap, scale: EmoteScale) -> Option<ParsedMessage> {
    m.message_text.as_ref().filter(|t| !t.trim().is_empty()).map(|text| {
        let refs: SmallVec<[TwitchEmoteRef<'_>; 8]> = m.emotes.iter().map(|e| TwitchEmoteRef { id: &e.id, code: &e.code }).collect();
        ParsedMessage {
            id: m.message_id.clone(),
            channel_login: m.channel_login.clone(),
            channel_id: m.channel_id.clone(),
            sender_id: m.sender.id.clone(),
            sender_login: m.sender.login.clone(),
            sender_name: m.sender.name.clone(),
            color: m.name_color.map(|c| [c.r, c.g, c.b]),
            badges: m.badges.iter().map(|b| Badge { name: b.name.clone(), version: b.version.clone() }).collect(),
            is_action: false,
            timestamp: m.server_timestamp,
            segments: parser::tokenize(text, &refs, emotes, scale),
            raw_text: text.clone(),
            deleted: false,
        }
    })
}

fn usernotice_to_alert(m: UserNoticeMessage, kind: AlertKind, emotes: &EmoteMap, scale: EmoteScale) -> Option<ChatAlert> {
    let message = usernotice_message(&m, emotes, scale);
    Some(ChatAlert {
        id: m.message_id,
        channel_login: m.channel_login,
        user_id: m.sender.id,
        user_login: m.sender.login,
        user_name: m.sender.name,
        color: m.name_color.map(|c| [c.r, c.g, c.b]),
        kind,
        message,
        timestamp: m.server_timestamp,
    })
}

#[cfg(test)]
mod tests {
    use twitch_irc::message::IRCMessage;

    use super::*;

    fn translate_raw(raw: &str) -> Option<IrcEvent> {
        let msg = ServerMessage::try_from(IRCMessage::parse(raw).unwrap()).unwrap();
        translate(msg, &EmoteStore::default(), EmoteScale::X1)
    }

    fn alert(raw: &str) -> ChatAlert {
        match translate_raw(raw) {
            Some(IrcEvent::Ui(UiEvent::Alert(a))) => *a,
            other => panic!("oczekiwano alertu, jest {other:?}"),
        }
    }

    #[test]
    fn watch_streak_shows_with_message_and_announcements_reach_chat() {
        let streak = alert("@badge-info=;badges=;color=#8A2BE2;display-name=Widz;emotes=;flags=;id=w1;login=widz;mod=0;msg-id=viewermilestone;msg-param-category=watch-streak;msg-param-copoReward=450;msg-param-id=x;msg-param-value=40;room-id=1;subscriber=0;system-msg=Widz\\swatched\\s40\\sconsecutive\\sstreams;tmi-sent-ts=1700000000000;user-id=9;user-type= :tmi.twitch.tv USERNOTICE #kanal :40 dni z rzędu, pozdro!");
        assert_eq!(streak.kind, AlertKind::WatchStreak { streams: 40 });
        assert_eq!(streak.message.as_ref().map(|m| m.raw_text.as_str()), Some("40 dni z rzędu, pozdro!"));

        let raw = "@badge-info=;badges=moderator/1;color=;display-name=Mod;emotes=;flags=;id=a1;login=mod;mod=1;msg-id=announcement;msg-param-color=PRIMARY;room-id=1;subscriber=0;system-msg=;tmi-sent-ts=1700000000000;user-id=5;user-type=mod :tmi.twitch.tv USERNOTICE #kanal :Ważne ogłoszenie";
        match translate_raw(raw) {
            Some(IrcEvent::Ui(UiEvent::Message(m))) => assert_eq!(m.raw_text, "Ważne ogłoszenie"),
            other => panic!("ogłoszenie powinno trafić na czat, jest {other:?}"),
        }
    }

    #[test]
    fn cheer_becomes_bits_alert_with_message() {
        let a = alert("@badge-info=;badges=;bits=500;color=#FF0000;display-name=Gaduła;emotes=;id=abc-1;mod=0;room-id=1;subscriber=0;tmi-sent-ts=1700000000000;user-id=42;user-type= :gadula!gadula@gadula.tmi.twitch.tv PRIVMSG #kanal :Cheer500 trzymaj!");
        assert_eq!(a.kind, AlertKind::Bits { amount: 500 });
        assert_eq!(a.user_name, "Gaduła");
        assert_eq!(a.message.unwrap().raw_text, "Cheer500 trzymaj!");
    }

    #[test]
    fn resub_and_raid_and_mass_gift() {
        let resub = alert("@badge-info=subscriber/6;badges=subscriber/6;color=#FF0000;display-name=9966Qtips;emotes=;flags=;id=916c;login=9966qtips;mod=0;msg-id=resub;msg-param-cumulative-months=6;msg-param-months=0;msg-param-should-share-streak=0;msg-param-sub-plan-name=Channel\\sSubscription;msg-param-sub-plan=Prime;room-id=71092938;subscriber=1;system-msg=x;tmi-sent-ts=1575162201680;user-id=46977320;user-type= :tmi.twitch.tv USERNOTICE #xqcow :xqcJAM pół roku!");
        assert_eq!(resub.kind, AlertKind::Sub { tier: "Prime".into(), months: 6, resub: true });
        assert_eq!(resub.message.unwrap().raw_text, "xqcJAM pół roku!");

        let raid = alert("@badge-info=;badges=;color=#0000FF;display-name=Rajder;emotes=;flags=;id=r1;login=rajder;mod=0;msg-id=raid;msg-param-displayName=Rajder;msg-param-login=rajder;msg-param-profileImageURL=https://static-cdn.jtvnw.net/x-70x70.png;msg-param-viewerCount=42;room-id=1;subscriber=0;system-msg=x;tmi-sent-ts=1575162201680;user-id=7;user-type= :tmi.twitch.tv USERNOTICE #kanal");
        assert!(matches!(raid.kind, AlertKind::Raid { viewers: 42, .. }));
        assert!(raid.message.is_none());

        let gift = alert("@badge-info=;badges=;color=;display-name=Hojny;emotes=;flags=;id=g1;login=hojny;mod=0;msg-id=submysterygift;msg-param-mass-gift-count=5;msg-param-origin-id=x;msg-param-sender-count=20;msg-param-sub-plan=1000;room-id=1;subscriber=1;system-msg=x;tmi-sent-ts=1575162201680;user-id=8;user-type= :tmi.twitch.tv USERNOTICE #kanal");
        assert_eq!(gift.kind, AlertKind::MassGift { tier: "1".into(), count: 5, total: Some(20), anonymous: false });
    }

    #[test]
    fn plain_message_stays_message() {
        let e = translate_raw("@badge-info=;badges=;color=;display-name=Ktoś;emotes=;id=m1;mod=0;room-id=1;subscriber=0;tmi-sent-ts=1700000000000;user-id=1;user-type= :ktos!ktos@ktos.tmi.twitch.tv PRIVMSG #kanal :siema");
        assert!(matches!(e, Some(IrcEvent::Ui(UiEvent::Message(_)))));
    }
}
