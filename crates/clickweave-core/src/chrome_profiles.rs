use crate::sanitize::sanitize_for_path;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ChromeProfile {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ProfilesFile {
    profiles: Vec<ChromeProfile>,
}

pub struct ChromeProfileStore {
    base_dir: PathBuf,
}

impl ChromeProfileStore {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    pub fn profile_path(&self, id: &str) -> PathBuf {
        // Sanitize to prevent path traversal — only allow the sanitized form.
        self.base_dir.join(sanitize_for_path(id))
    }

    pub fn is_configured(&self, id: &str) -> bool {
        self.profile_path(id).join("Default/Preferences").exists()
    }

    pub fn load_profiles(&self) -> Vec<ChromeProfile> {
        let path = self.base_dir.join("profiles.json");
        match std::fs::read_to_string(&path) {
            Ok(contents) => {
                serde_json::from_str::<ProfilesFile>(&contents)
                    .unwrap_or_default()
                    .profiles
            }
            Err(_) => Vec::new(),
        }
    }

    fn save_profiles(&self, profiles: &[ChromeProfile]) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.base_dir)?;
        let file = ProfilesFile {
            profiles: profiles.to_vec(),
        };
        let json = serde_json::to_string_pretty(&file).map_err(std::io::Error::other)?;
        std::fs::write(self.base_dir.join("profiles.json"), json)
    }

    pub fn create_profile(&self, name: &str) -> std::io::Result<ChromeProfile> {
        let id = sanitize_for_path(name);
        let mut profiles = self.load_profiles();
        if profiles.iter().any(|p| p.id == id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("Profile '{}' already exists", id),
            ));
        }
        let profile = ChromeProfile {
            id,
            name: name.to_string(),
        };
        profiles.push(profile.clone());
        self.save_profiles(&profiles)?;
        Ok(profile)
    }

    /// Ensure at least one profile exists. Returns all profiles.
    pub fn ensure_profiles(&self) -> std::io::Result<Vec<ChromeProfile>> {
        let profiles = self.load_profiles();
        if !profiles.is_empty() {
            return Ok(profiles);
        }
        self.create_profile("Profile 1")?;
        Ok(self.load_profiles())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn profile_path_returns_correct_subdir() {
        let base = TempDir::new().unwrap();
        let store = ChromeProfileStore::new(base.path().to_path_buf());
        let path = store.profile_path("my-profile");
        assert_eq!(path, base.path().join("my-profile"));
    }

    #[test]
    fn is_configured_false_for_missing_dir() {
        let base = TempDir::new().unwrap();
        let store = ChromeProfileStore::new(base.path().to_path_buf());
        assert!(!store.is_configured("nonexistent"));
    }

    #[test]
    fn is_configured_true_when_preferences_exists() {
        let base = TempDir::new().unwrap();
        let store = ChromeProfileStore::new(base.path().to_path_buf());
        let prefs = base.path().join("test-profile/Default/Preferences");
        std::fs::create_dir_all(prefs.parent().unwrap()).unwrap();
        std::fs::write(&prefs, "{}").unwrap();
        assert!(store.is_configured("test-profile"));
    }

    #[test]
    fn create_profile_persists_to_disk() {
        let base = TempDir::new().unwrap();
        let store = ChromeProfileStore::new(base.path().to_path_buf());
        let profile = store.create_profile("Work Account").unwrap();
        assert_eq!(profile.id, "work-account");
        assert_eq!(profile.name, "Work Account");

        let loaded = store.load_profiles();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "work-account");
    }

    #[test]
    fn create_profile_rejects_duplicate_id() {
        let base = TempDir::new().unwrap();
        let store = ChromeProfileStore::new(base.path().to_path_buf());
        store.create_profile("Work").unwrap();
        let err = store.create_profile("Work").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn ensure_profiles_creates_default() {
        let base = TempDir::new().unwrap();
        let store = ChromeProfileStore::new(base.path().to_path_buf());
        let profiles = store.ensure_profiles().unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "Profile 1");

        // Second call returns existing, doesn't duplicate
        let again = store.ensure_profiles().unwrap();
        assert_eq!(again.len(), 1);
    }
}
