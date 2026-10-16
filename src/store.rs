//! Face template store (persistent storage).
//!
//! The file is a JSON map from user to [`Template`]: `{"<user>": {"prints": [{"features": [512
//! floats], "generation": 0, "source": "enroll"}, ...]}}`. Stores from before multi-print
//! templates (one averaged `features` vector per user) are not read; re-enroll. With the `tpm` feature it can instead be sealed:
//! encrypted under a key that only this machine's TPM can unseal (see [`crate::tpm`]). A sealed
//! store stays sealed when it is saved again.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::debug;

use crate::matcher::ENROLL_ON_FAILURE_THRESHOLD;
#[cfg(feature = "tpm")]
use crate::tpm::{DataKey, SealedFile};
use crate::{Embedding, PrintSource, Template, MATCH_THRESHOLD};

/// Result of [`FaceStore::add_on_failure`]; the daemon sends it to the PAM module.
#[derive(Debug, Clone, PartialEq)]
pub enum OnFailure {
    Added {
        similarity: f32,
    },
    NotEnrolled,
    /// Below [`ENROLL_ON_FAILURE_THRESHOLD`]
    TooFar {
        similarity: f32,
    },
    /// Matches another enrolled user
    OtherUser {
        user: String,
        similarity: f32,
    },
}

impl std::fmt::Display for OnFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OnFailure::Added { similarity } => write!(f, "ADDED {similarity:.3}"),
            OnFailure::NotEnrolled => f.write_str("REFUSED not_enrolled"),
            OnFailure::TooFar { similarity } => write!(f, "REFUSED too_far {similarity:.3}"),
            OnFailure::OtherUser { user, similarity } => write!(f, "REFUSED other_user {user} {similarity:.3}"),
        }
    }
}

/// Persistent face template store
pub struct FaceStore {
    path: PathBuf,
    faces: HashMap<String, Template>,
    /// Set when the store is sealed.
    #[cfg(feature = "tpm")]
    key: Option<DataKey>,
}

impl FaceStore {
    /// Load store from file (or create empty)
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let mut store = Self {
            path: path.as_ref().to_path_buf(),
            faces: HashMap::new(),
            #[cfg(feature = "tpm")]
            key: None,
        };
        store.reload()?;
        Ok(store)
    }

    /// Re-read the file, so changes made by another process are seen. A sealed store whose key
    /// was already unsealed by this process does not go to the TPM again.
    pub fn reload(&mut self) -> Result<()> {
        if !self.path.exists() {
            self.faces.clear();
            #[cfg(feature = "tpm")]
            {
                self.key = None;
            }
            return Ok(());
        }
        let data = fs::read(&self.path)?;
        let value: serde_json::Value =
            serde_json::from_slice(&data).with_context(|| format!("parsing face store {}", self.path.display()))?;
        // A sealed file has a string `format` field; in a plain store every value is a template.
        if value.get("format").is_some_and(serde_json::Value::is_string) {
            self.load_sealed(value)?;
        } else {
            self.faces = serde_json::from_value(value).with_context(|| self.format_error())?;
            #[cfg(feature = "tpm")]
            {
                self.key = None;
            }
        }
        debug!(count = self.faces.len(), "loaded face store");
        Ok(())
    }

    #[cfg(feature = "tpm")]
    fn load_sealed(&mut self, value: serde_json::Value) -> Result<()> {
        let file: SealedFile = serde_json::from_value(value)
            .with_context(|| format!("parsing sealed face store {}", self.path.display()))?;
        let key = match self.key.take() {
            Some(key) if key.sealed() == file.key() => key,
            _ => DataKey::unseal(file.key())
                .with_context(|| format!("opening sealed face store {}", self.path.display()))?,
        };
        let json = key.decrypt(&file).with_context(|| format!("opening sealed face store {}", self.path.display()))?;
        self.faces = serde_json::from_slice(&json).with_context(|| self.format_error())?;
        self.key = Some(key);
        Ok(())
    }

    #[cfg(not(feature = "tpm"))]
    fn load_sealed(&mut self, _: serde_json::Value) -> Result<()> {
        anyhow::bail!(
            "face store {} is sealed with a TPM, but linux_hello was built without the `tpm` feature",
            self.path.display()
        )
    }

    fn format_error(&self) -> String {
        format!(
            "parsing face store {0} (a store from before multi-print templates: move it aside, e.g. \
             `mv {0} {0}.old`, and enroll again)",
            self.path.display()
        )
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Seal the store from the next [`save`](Self::save) on, under a new data key sealed by the
    /// TPM. A non-empty `pcrs` also binds the key to the current values of those SHA-256 PCRs.
    #[cfg(feature = "tpm")]
    pub fn seal(&mut self, pcrs: &[u8]) -> Result<()> {
        self.key = Some(DataKey::create(pcrs)?);
        Ok(())
    }

    /// Store plain JSON from the next [`save`](Self::save) on.
    #[cfg(feature = "tpm")]
    pub fn unseal(&mut self) {
        self.key = None;
    }

    /// The PCRs the sealed key is bound to, or `None` for a plain store.
    #[cfg(feature = "tpm")]
    pub fn sealed_pcrs(&self) -> Option<&[u8]> {
        self.key.as_ref().map(DataKey::pcrs)
    }

    /// Save atomically (write a temp file, then rename) with owner-only permissions:
    /// templates are biometric data, and the daemon may read the file at any time.
    pub fn save(&self) -> Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let contents = self.encode()?;
        let tmp = self.path.with_extension("json.tmp");
        let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        file.write_all(&contents)?;
        file.sync_all()?;
        fs::rename(&tmp, &self.path)?;
        debug!("saved face store");
        Ok(())
    }

    #[cfg(feature = "tpm")]
    fn encode(&self) -> Result<Vec<u8>> {
        let json = zeroize::Zeroizing::new(serde_json::to_vec_pretty(&self.faces)?);
        match &self.key {
            Some(key) => Ok(serde_json::to_vec_pretty(&key.encrypt(&json)?)?),
            None => Ok(json.to_vec()),
        }
    }

    #[cfg(not(feature = "tpm"))]
    fn encode(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec_pretty(&self.faces)?)
    }

    /// Set a user's template, replacing any earlier one
    pub fn add(&mut self, user_id: String, template: Template) {
        self.faces.insert(user_id, template);
    }

    /// Remove a face by user ID
    pub fn remove(&mut self, user_id: &str) -> bool {
        self.faces.remove(user_id).is_some()
    }

    /// Get a user's template
    pub fn get(&self, user_id: &str) -> Option<&Template> {
        self.faces.get(user_id)
    }

    pub fn get_mut(&mut self, user_id: &str) -> Option<&mut Template> {
        self.faces.get_mut(user_id)
    }

    /// The other enrolled user that one of `prints` matches (similarity above [`MATCH_THRESHOLD`]),
    /// with the similarity; the closest one if several do (`CheckForDuplicateEnrollment`).
    pub fn duplicate(&self, user: &str, prints: &[Embedding]) -> Option<(&str, f32)> {
        self.faces
            .iter()
            .filter(|(other, _)| *other != user)
            .filter_map(|(other, template)| {
                let best =
                    prints.iter().filter_map(|p| template.best_match(p)).map(|m| m.similarity).reduce(f32::max)?;
                Some((other.as_str(), best))
            })
            .filter(|(_, similarity)| *similarity > MATCH_THRESHOLD)
            .max_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// Enroll-on-failure (F31): add `print`, from a failed face attempt of `user` that the
    /// password then confirmed, if it is at least [`ENROLL_ON_FAILURE_THRESHOLD`] similar to the
    /// user's enrolled and augmented prints and matches no other user. Does not save.
    pub fn add_on_failure(&mut self, user: &str, print: &Embedding) -> OnFailure {
        let Some(template) = self.faces.get(user) else { return OnFailure::NotEnrolled };
        let similarity = template
            .best_match_where(print, |p| p.source != PrintSource::OnFailure)
            .map_or(f32::NEG_INFINITY, |m| m.similarity);
        if similarity < ENROLL_ON_FAILURE_THRESHOLD {
            return OnFailure::TooFar { similarity };
        }
        if let Some((other, similarity)) = self.duplicate(user, std::slice::from_ref(print)) {
            return OnFailure::OtherUser { user: other.to_string(), similarity };
        }
        self.faces.get_mut(user).expect("checked above").add_on_failure(print);
        OnFailure::Added { similarity }
    }

    /// List all user IDs
    pub fn list_users(&self) -> Vec<&str> {
        self.faces.keys().map(|s| s.as_str()).collect()
    }

    /// Number of enrolled faces
    pub fn len(&self) -> usize {
        self.faces.len()
    }

    /// Check if store is empty
    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_store_round_trip() {
        let dir = std::env::temp_dir().join(format!("linux_hello_store_{}", std::process::id()));
        let path = dir.join("faces.json");
        let mut store = FaceStore::load(&path).unwrap();
        store.add("alice".into(), Template::enrolled([crate::Embedding { features: vec![0.6, 0.8] }]));
        store.save().unwrap();
        let reloaded = FaceStore::load(&path).unwrap();
        assert_eq!(reloaded.get("alice").unwrap().prints[0].features, vec![0.6, 0.8]);

        // A store from before multi-print templates is refused, not silently misread.
        fs::write(&path, r#"{"alice": {"features": [0.6, 0.8]}}"#).unwrap();
        let err = FaceStore::load(&path).err().unwrap();
        assert!(format!("{err:#}").contains("re-enrolled"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn duplicate_names_the_closest_other_user() {
        let emb = |features: &[f32]| crate::Embedding { features: features.to_vec() };
        let mut store = FaceStore::load(std::env::temp_dir().join("linux_hello_store_unused.json")).unwrap();
        store.add("alice".into(), Template::enrolled([emb(&[1.0, 0.0, 0.0])]));
        store.add("bob".into(), Template::enrolled([emb(&[0.6, 0.8, 0.0])]));
        // Similarity 0.8 to alice, 0.96 to bob.
        let probe = [emb(&[0.0, 0.0, 1.0]), emb(&[0.8, 0.6, 0.0])];
        let rounded = |d: Option<(&str, f32)>| d.map(|(user, s)| (user.to_string(), (s * 100.0).round() as i32));
        assert_eq!(rounded(store.duplicate("carol", &probe)), Some(("bob".into(), 96)));
        // Re-enrolling bob only compares with the others.
        assert_eq!(rounded(store.duplicate("bob", &probe)), Some(("alice".into(), 80)));
        // At or below the threshold is not a duplicate.
        assert_eq!(store.duplicate("carol", &[emb(&[0.5, 0.0, 0.866])]), None);
    }

    #[test]
    fn on_failure_needs_the_relaxed_match_and_no_other_user() {
        let emb = |features: &[f32]| crate::Embedding { features: features.to_vec() };
        let mut store = FaceStore::load(std::env::temp_dir().join("linux_hello_store_unused.json")).unwrap();
        store.add("alice".into(), Template::enrolled([emb(&[1.0, 0.0, 0.0])]));
        store.add("bob".into(), Template::enrolled([emb(&[0.0, 1.0, 0.0])]));
        let prints = |store: &FaceStore| store.get("alice").unwrap().prints.len();

        assert_eq!(store.add_on_failure("carol", &emb(&[1.0, 0.0, 0.0])), OnFailure::NotEnrolled);
        // 0.30 to alice: too far.
        assert!(matches!(store.add_on_failure("alice", &emb(&[0.3, 0.0, 0.954])), OnFailure::TooFar { .. }));
        // 0.45 to alice but 0.6 to bob: matches another user.
        let near_bob = emb(&[0.45, 0.6, 0.66]);
        assert!(matches!(store.add_on_failure("alice", &near_bob), OnFailure::OtherUser { user, .. } if user == "bob"));
        assert_eq!(prints(&store), 1);
        // 0.45 to alice, 0.2 to bob: added.
        let relaxed = emb(&[0.45, 0.2, 0.87]);
        assert!(matches!(store.add_on_failure("alice", &relaxed), OnFailure::Added { .. }));
        assert_eq!(prints(&store), 2);
        // The added print does not count for the relaxed check: a probe 0.99 to it but 0.35 to the
        // enrolled print is refused, so prints cannot drift away from the enrolled face.
        assert!(matches!(store.add_on_failure("alice", &emb(&[0.35, 0.2, 0.915])), OnFailure::TooFar { .. }));
    }

    #[test]
    fn stores_without_on_failure_prints_still_load() {
        let dir = std::env::temp_dir().join(format!("linux_hello_store_compat_{}", std::process::id()));
        let path = dir.join("faces.json");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            &path,
            r#"{"alice": {"prints": [{"features": [0.6, 0.8], "generation": 0, "source": "enroll"},
            {"features": [0.8, 0.6], "generation": 1, "source": "augment"}]}}"#,
        )
        .unwrap();
        let mut store = FaceStore::load(&path).unwrap();
        assert_eq!(store.get("alice").unwrap().prints.len(), 2);
        store.get_mut("alice").unwrap().add_on_failure(&crate::Embedding { features: vec![1.0, 0.0] });
        store.save().unwrap();
        assert_eq!(FaceStore::load(&path).unwrap().get("alice").unwrap().prints[2].source, PrintSource::OnFailure);
        fs::remove_dir_all(dir).unwrap();
    }
}
