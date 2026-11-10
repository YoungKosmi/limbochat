//! Persisted Twitch keys stay unchanged; Kick keys have an explicit namespace.
pub fn is_kick(channel: &str) -> bool {
    channel.starts_with("kick:")
}
pub fn slug(channel: &str) -> &str {
    channel.strip_prefix("kick:").unwrap_or(channel)
}
pub fn viewer(channel: &str, login: &str) -> String {
    if is_kick(channel) {
        format!("kick:{login}")
    } else {
        login.into()
    }
}
pub fn channel(name: &str, platform: Option<&str>) -> Result<String, String> {
    let name = name.trim().trim_start_matches(['#', '@']).to_lowercase();
    let (kind, name) = if let Some(n) = name.strip_prefix("kick:") {
        ("kick", n)
    } else if let Some(n) = name.strip_prefix("twitch:") {
        ("twitch", n)
    } else {
        (platform.unwrap_or("twitch"), name.as_str())
    };
    if !matches!(kind, "kick" | "twitch") {
        return Err("Wybierz Twitch albo Kick.".into());
    }
    if !(1..=25).contains(&name.len())
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || (kind == "kick" && c == b'-'))
    {
        return Err("Niepoprawna nazwa kanału.".into());
    }
    Ok(if kind == "kick" {
        format!("kick:{name}")
    } else {
        name.into()
    })
}
pub fn unavailable(feature: &str) -> String {
    format!(
        "Kick: {feature} nie jest udostępnione przez publiczne API. Użyj panelu Kicka. Konta Twitch nie zmieniono."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn channel_keys_are_distinct_and_backward_compatible() {
        assert_eq!(channel(" @Same ", None).unwrap(), "same");
        assert_eq!(channel("Same", Some("kick")).unwrap(), "kick:same");
        assert_eq!(channel("kick:john-doe", None).unwrap(), "kick:john-doe");
        assert!(channel("john-doe", Some("twitch")).is_err());
        assert!(channel("evil/channel", Some("kick")).is_err());
        assert!(channel("same", Some("other")).is_err());
    }
}
