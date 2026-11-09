use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub token: String,
    pub db_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
#[error("BOT_TOKEN is required (set it in .env)")]
pub struct MissingToken;

/// Empty values count as unset, like Python's `if not token` and `get(..) or default`.
pub fn load_config(get: impl Fn(&str) -> Option<String>) -> Result<Config, MissingToken> {
    let token = get("BOT_TOKEN").filter(|t| !t.is_empty()).ok_or(MissingToken)?;
    let db_path = get("DB_PATH").filter(|p| !p.is_empty()).unwrap_or_else(|| "data/bot.db".into());
    Ok(Config { token, db_path: PathBuf::from(db_path) })
}

pub fn load_config_from_env() -> Result<Config, MissingToken> {
    load_config(|k| std::env::var(k).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> =
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn load_config_defaults() {
        let cfg = load_config(env(&[("BOT_TOKEN", "tok")])).unwrap();
        assert_eq!(cfg.token, "tok");
        assert_eq!(cfg.db_path, PathBuf::from("data/bot.db"));
    }

    #[test]
    fn load_config_db_path() {
        let cfg = load_config(env(&[("BOT_TOKEN", "t"), ("DB_PATH", "x/y.db")])).unwrap();
        assert_eq!(cfg.db_path, PathBuf::from("x/y.db"));
    }

    #[test]
    fn load_config_requires_token() {
        let err = load_config(env(&[])).unwrap_err();
        assert_eq!(err.to_string(), "BOT_TOKEN is required (set it in .env)");
    }

    #[test]
    fn empty_values_count_as_unset() {
        assert!(load_config(env(&[("BOT_TOKEN", "")])).is_err());
        let cfg = load_config(env(&[("BOT_TOKEN", "t"), ("DB_PATH", "")])).unwrap();
        assert_eq!(cfg.db_path, PathBuf::from("data/bot.db"));
    }
}
