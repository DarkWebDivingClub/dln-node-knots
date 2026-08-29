use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

// RateLimitRule represents a rate limit rule with rate per micro second and max capacity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimitRule {
    #[serde(default)]
    pub rate_per_micro: u64,
    #[serde(
        default = "default_max_capacity",
        deserialize_with = "deserialize_max_capacity"
    )]
    pub max_capacity: i64,
}

fn default_max_capacity() -> i64 {
    i64::MAX
}

fn deserialize_max_capacity<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = i64::deserialize(deserializer)?;
    if value < 0 {
        return Err(de::Error::custom(format!(
            "max_capacity must be between 0 and {}",
            i64::MAX
        )));
    }
    Ok(value)
}
