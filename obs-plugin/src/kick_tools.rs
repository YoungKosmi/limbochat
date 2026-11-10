use crate::{jarvis::tools::Context, kick::Client, platform};
use reqwest::Method;
use serde_json::{Value, json};
use std::sync::Arc;

pub async fn execute(
    kick: Arc<Client>,
    ctx: &Context,
    name: &str,
    args: &Value,
) -> Result<Value, String> {
    let chat = ctx
        .active
        .as_ref()
        .filter(|c| platform::is_kick(&c.login))
        .ok_or("Wybierz kanał Kicka.")?;
    let own = ctx.me.as_ref().filter(|(l, _)| platform::is_kick(l));
    let s = |key: &str| args[key].as_str().unwrap_or_default().trim().to_owned();
    match name {
        "get_recent_chat" => {
            return Ok(
                json!({"messages":ctx.recent.iter().rev().take(30).map(|(name,text)|json!({"name":name,"text":text})).collect::<Vec<_>>()}),
            );
        }
        "get_stream_info" => {
            let v = kick
                .request(
                    Method::GET,
                    &format!("v1/channels?slug={}", platform::slug(&chat.login)),
                    None,
                    true,
                )
                .await?;
            let c = &v["data"][0];
            return Ok(
                json!({"platform":"kick","title":c["stream_title"],"category":c["category"]["name"],"live":c["stream"]["is_live"],"viewers":c["stream"]["viewer_count"],"started_at":c["stream"]["start_time"]}),
            );
        }
        "send_chat_message" => {
            if args["announcement"].as_bool() == Some(true) {
                return Err(platform::unavailable("ogłoszenie"));
            }
            kick.send(&chat.id, &s("text")).await?;
        }
        "timeout_user" | "ban_user" | "unban_user" => {
            let wanted = s("user").trim_start_matches('@').to_lowercase();
            let found: Vec<_> = ctx
                .chatters
                .iter()
                .filter(|c| c.login == wanted || c.name.to_lowercase() == wanted)
                .collect();
            let user_id = if found.len() == 1 {
                found[0].id.clone()
            } else if found.is_empty() {
                let slug = platform::channel(&wanted, Some("kick"))?;
                let v = kick
                    .request(
                        Method::GET,
                        &format!("v1/channels?slug={}", platform::slug(&slug)),
                        None,
                        true,
                    )
                    .await?;
                v["data"][0]["broadcaster_user_id"]
                    .as_u64()
                    .ok_or("Nie znaleziono użytkownika Kicka. Podaj dokładny nick.")?
                    .to_string()
            } else {
                return Err("Nick jest niejednoznaczny. Podaj dokładny login Kicka.".into());
            };
            if name == "unban_user" {
                kick.unban(&chat.id, &user_id).await?
            } else {
                let seconds = if name == "timeout_user" {
                    Some(
                        args["seconds"]
                            .as_u64()
                            .unwrap_or(600)
                            .try_into()
                            .map_err(|_| "Zbyt długi timeout.")?,
                    )
                } else {
                    None
                };
                kick.ban(&chat.id, &user_id, seconds, args["reason"].as_str())
                    .await?;
            }
        }
        "set_title" => {
            own.ok_or("Połącz konto Kick w ustawieniach.")?;
            let title = s("title");
            if title.is_empty() || title.chars().count() > 140 {
                return Err("Tytuł musi mieć 1–140 znaków.".into());
            }
            kick.request(
                Method::PATCH,
                "v1/channels",
                Some(json!({"stream_title":title})),
                false,
            )
            .await?;
        }
        "set_category" => {
            own.ok_or("Połącz konto Kick w ustawieniach.")?;
            let wanted = s("name");
            let mut url = reqwest::Url::parse("https://api.kick.com/public/v2/categories").unwrap();
            url.query_pairs_mut().append_pair("name", &wanted);
            let v = kick
                .request(
                    Method::GET,
                    &format!("v2/categories?{}", url.query().unwrap()),
                    None,
                    true,
                )
                .await?;
            let categories = v["data"].as_array().ok_or("Kick nie zwrócił kategorii.")?;
            let found = categories
                .iter()
                .find(|v| {
                    v["name"]
                        .as_str()
                        .is_some_and(|n| n.eq_ignore_ascii_case(&wanted))
                })
                .ok_or("Nie znaleziono dokładnej kategorii Kicka. Podaj pełną nazwę.")?;
            let category = found["id"].as_u64().ok_or("Brak ID kategorii.")?;
            kick.request(
                Method::PATCH,
                "v1/channels",
                Some(json!({"category_id":category})),
                false,
            )
            .await?;
        }
        _ => return Err(platform::unavailable(name)),
    }
    Ok(json!({"ok":true,"platform":"kick"}))
}
