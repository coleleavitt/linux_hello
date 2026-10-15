//! Face embedding store (persistent storage).

use anyhow::Result;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::debug;

use crate::Embedding;

/// Persistent face embedding store
pub struct FaceStore {
    path: PathBuf,
    faces: HashMap<String, Embedding>,
}

impl FaceStore {
    /// Load store from file (or create empty)
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let faces = if path.exists() {
            let data = fs::read_to_string(&path)?;
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            HashMap::new()
        };
        
        debug!(count = faces.len(), "loaded face store");
        Ok(Self { path, faces })
    }
    
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Save atomically (write a temp file, then rename) with owner-only permissions:
    /// templates are biometric data, and the daemon may read the file at any time.
    pub fn save(&self) -> Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        file.write_all(serde_json::to_string_pretty(&self.faces)?.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, &self.path)?;
        debug!("saved face store");
        Ok(())
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
