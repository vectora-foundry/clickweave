use crate::sanitize::sanitize_for_path;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ChromeProfile {
    pub id: String,
    pub name: String,
    pub google_email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProfileEntry {
    id: String,
    name: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ProfilesFile {
    profiles: Vec<ProfileEntry>,
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

    fn preferences_path(&self, id: &str) -> PathBuf {
        self.profile_path(id).join("Default/Preferences")
    }

    pub fn is_configured(&self, id: &str) -> bool {
        self.preferences_path(id).exists()
    }

    fn load_entries(&self) -> Vec<ProfileEntry> {
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

    pub fn load_profiles(&self) -> Vec<ChromeProfile> {
        self.load_entries()
            .into_iter()
            .map(|e| ChromeProfile {
                google_email: self.read_google_email(&e.id),
                id: e.id,
                name: e.name,
            })
            .collect()
    }

    fn save_entries(&self, entries: &[ProfileEntry]) -> std::io::Result<()> {
        let file = ProfilesFile {
            profiles: entries.to_vec(),
        };
        crate::storage::write_json_atomic(&self.base_dir.join("profiles.json"), &file)
    }

    pub fn create_profile(&self, name: &str) -> std::io::Result<ChromeProfile> {
        let id = sanitize_for_path(name);
        let mut entries = self.load_entries();
        if entries.iter().any(|e| e.id == id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("Profile '{}' already exists", id),
            ));
        }
        entries.push(ProfileEntry {
            id: id.clone(),
            name: name.to_string(),
        });
        self.save_entries(&entries)?;
        Ok(ChromeProfile {
            id,
            name: name.to_string(),
            google_email: None,
        })
    }

    fn read_google_email(&self, id: &str) -> Option<String> {
        let contents = std::fs::read_to_string(self.preferences_path(id)).ok()?;
        let value: serde_json::Value = serde_json::from_str(&contents).ok()?;
        value
            .get("account_info")?
            .as_array()?
            .first()?
            .get("email")?
            .as_str()
            .map(String::from)
    }

    /// Resolve a profile display name to its filesystem path (case-insensitive).
    /// Strips trailing " (email)" suffixes so the LLM can include the email without breaking resolution.
    /// Returns `None` if no profile matches.
    pub fn resolve_profile_path_by_name(&self, name: &str) -> Option<PathBuf> {
        let stripped = name.find(" (").map(|i| &name[..i]).unwrap_or(name);
        let lower = stripped.to_lowercase();
        self.load_profiles()
            .into_iter()
            .find(|p| p.name.to_lowercase() == lower)
            .map(|p| self.profile_path(&p.id))
    }

    /// Ensure at least one profile exists. Returns all profiles.
    pub fn ensure_profiles(&self) -> std::io::Result<Vec<ChromeProfile>> {
        if self.load_entries().is_empty() {
            self.create_profile("Profile 1")?;
        }
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

    #[test]
    fn read_google_email_extracts_email_from_preferences() {
        let base = TempDir::new().unwrap();
        let store = ChromeProfileStore::new(base.path().to_path_buf());
        let prefs_dir = base.path().join("test-profile/Default");
        std::fs::create_dir_all(&prefs_dir).unwrap();
        std::fs::write(
            prefs_dir.join("Preferences"),
            r#"{"account_info": [{"email": "user@gmail.com", "full_name": "Test User"}]}"#,
        )
        .unwrap();
        assert_eq!(
            store.read_google_email("test-profile"),
            Some("user@gmail.com".to_string()),
        );
    }

    #[test]
    fn read_google_email_returns_none_when_no_account() {
        let base = TempDir::new().unwrap();
        let store = ChromeProfileStore::new(base.path().to_path_buf());
        let prefs_dir = base.path().join("test-profile/Default");
        std::fs::create_dir_all(&prefs_dir).unwrap();
        std::fs::write(prefs_dir.join("Preferences"), r#"{"account_info": []}"#).unwrap();
        assert_eq!(store.read_google_email("test-profile"), None);
    }

    #[test]
    fn read_google_email_returns_none_when_no_file() {
        let base = TempDir::new().unwrap();
        let store = ChromeProfileStore::new(base.path().to_path_buf());
        assert_eq!(store.read_google_email("nonexistent"), None);
    }

    #[test]
    fn resolve_profile_path_by_name_case_insensitive() {
        let base = TempDir::new().unwrap();
        let store = ChromeProfileStore::new(base.path().to_path_buf());
        store.create_profile("Work Account").unwrap();

        // Exact match
        let result = store.resolve_profile_path_by_name("Work Account");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), store.profile_path("work-account"));

        // Case-insensitive
        let result = store.resolve_profile_path_by_name("work account");
        assert!(result.is_some());

        // No match
        let result = store.resolve_profile_path_by_name("Personal");
        assert!(result.is_none());
    }

    #[test]
    fn resolve_profile_path_by_name_strips_email_suffix() {
        let base = TempDir::new().unwrap();
        let store = ChromeProfileStore::new(base.path().to_path_buf());
        store.create_profile("Your Chrome").unwrap();

        // LLM may include email suffix copied from the prompt display
        let result = store.resolve_profile_path_by_name("Your Chrome (ves.lisica@gmail.com)");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), store.profile_path("your-chrome"));
    }

    #[test]
    fn load_profiles_populates_google_email() {
        let base = TempDir::new().unwrap();
        let store = ChromeProfileStore::new(base.path().to_path_buf());
        store.create_profile("Work").unwrap();

        // Set up a Preferences file with a signed-in account
        let prefs_dir = base.path().join("work/Default");
        std::fs::create_dir_all(&prefs_dir).unwrap();
        std::fs::write(
            prefs_dir.join("Preferences"),
            r#"{"account_info": [{"email": "work@company.com"}]}"#,
        )
        .unwrap();

        let profiles = store.load_profiles();
        assert_eq!(profiles.len(), 1);
        assert_eq!(
            profiles[0].google_email,
            Some("work@company.com".to_string())
        );
    }
}
