/// Parsed response from upstream `cdp_find_elements`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CdpFindElementsResponse {
    #[serde(default)]
    pub page_url: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub matches: Vec<CdpFindElementMatch>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct CdpFindElementMatch {
    pub uid: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub parent_role: Option<String>,
    #[serde(default)]
    pub parent_name: Option<String>,
}

/// Pick a random port in the ephemeral range (49152-65535).
pub fn rand_ephemeral_port() -> u16 {
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let raw = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    let range = 65535 - 49152;
    49152 + (raw % range) as u16
}
