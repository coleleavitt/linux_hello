//! Face embedding store (persistent storage).
//!
//! The file is a JSON map from user to template. With the `tpm` feature it can instead be sealed:
//! encrypted under a key that only this machine's TPM can unseal (see [`crate::tpm`]). A sealed
//! store stays sealed when it is saved again.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::debug;

#[cfg(feature = "tpm")]
use crate::tpm::{DataKey, SealedFile};
use crate::Embedding;

/// Persistent face embedding store
pub struct FaceStore {
    path: PathBuf,
    faces: HashMap<String, Embedding>,
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
            self.faces = serde_json::from_value(value)
                .with_context(|| format!("parsing face store {}", self.path.display()))?;
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
            _ => DataKey::unseal(file.key()).with_context(|| format!("opening sealed face store {}", self.path.display()))?,
        };
        let json = key.decrypt(&file).with_context(|| format!("opening sealed face store {}", self.path.display()))?;
        self.faces = serde_json::from_slice(&json)?;
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

    /// Add a face embedding
    pub fn add(&mut self, user_id: String, embedding: Embedding) {
        self.faces.insert(user_id, embedding);
    }
    
    /// Remove a face by user ID
    pub fn remove(&mut self, user_id: &str) -> bool {
        self.faces.remove(user_id).is_some()
    }
    
    /// Get embedding by user ID
    pub fn get(&self, user_id: &str) -> Option<&Embedding> {
        self.faces.get(user_id)
    }
    
    /// Get all enrolled faces
    pub fn get_all(&self) -> Vec<(String, Embedding)> {
        self.faces.iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
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
        store.add("alice".into(), Embedding { features: vec![0.6, 0.8] });
        store.save().unwrap();
        let reloaded = FaceStore::load(&path).unwrap();
        assert_eq!(reloaded.get("alice").unwrap().features, vec![0.6, 0.8]);
        fs::remove_dir_all(dir).unwrap();
    }
}
