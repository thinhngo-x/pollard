//! `.pollard/config.toml`.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Data roots: local paths or `s3://` URLs.
    pub data: Vec<String>,
    /// Off-tree patterns (gitignore syntax).
    pub offtree: Vec<String>,
    /// Script outputs; auto-ignored by the code manifest.
    pub output_dirs: Vec<String>,
    /// Config capture mode: `hydra`, `file:<path>`, `sdk`. None = `file:config.yaml` if present, else `sdk`.
    pub config_capture: Option<String>,
    pub primary_metric: Option<String>,
    pub seed_keys: Vec<String>,
    pub checkpoint_dir: String,
    pub remote: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            data: vec![],
            offtree: vec!["*.md".into(), "notes/".into()],
            output_dirs: vec!["outputs/".into()],
            config_capture: None,
            primary_metric: None,
            seed_keys: vec!["seed".into(), "random_seed".into()],
            checkpoint_dir: "ckpt/".into(),
            remote: None,
        }
    }
}

pub const DEFAULT_TOML: &str = r#"# pollard repo config
# data = ["./data"]
offtree = ["*.md", "notes/"]
output_dirs = ["outputs/"]
checkpoint_dir = "ckpt/"
seed_keys = ["seed", "random_seed"]
# config_capture = "file:config.yaml"   # or "hydra", "sdk"
# primary_metric = "val_loss"
# remote = "/path/to/remote"
"#;

impl Config {
    pub fn parse(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }
}
