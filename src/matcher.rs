//! Face matching and verification.

use crate::Embedding;

/// Face matcher with configurable threshold
pub struct FaceMatcher {
    threshold: f32,
}

impl FaceMatcher {
    /// Create a new matcher with similarity threshold
    pub fn new(threshold: f32) -> Self {
        Self { threshold }
    }
    
    /// Find best matching user from enrolled faces
    pub fn find_match(
        &self,
        probe: &Embedding,
        enrolled: &[(String, Embedding)],
    ) -> Option<(String, f32)> {
        let mut best_match: Option<(String, f32)> = None;
        
        for (user_id, embedding) in enrolled {
            let similarity = probe.similarity(embedding);
            if similarity > self.threshold {
                match &best_match {
                    None => best_match = Some((user_id.clone(), similarity)),
                    Some((_, best_sim)) if similarity > *best_sim => {
                        best_match = Some((user_id.clone(), similarity));
                    }
                    _ => {}
                }
            }
        }
        
        best_match
    }
    
    /// Verify if two embeddings match
    pub fn verify(&self, a: &Embedding, b: &Embedding) -> bool {
        a.similarity(b) > self.threshold
    }
    
    /// Set threshold
    pub fn set_threshold(&mut self, threshold: f32) {
        self.threshold = threshold;
    }
    
    /// Get current threshold
    pub fn threshold(&self) -> f32 {
        self.threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matcher() {
        let matcher = FaceMatcher::new(0.5);
        
        let probe = Embedding { features: vec![1.0, 0.0, 0.0] };
        let enrolled = vec![
            ("user1".to_string(), Embedding { features: vec![0.99, 0.14, 0.0] }),  // ~84% similar
            ("user2".to_string(), Embedding { features: vec![0.0, 1.0, 0.0] }),    // 0% similar
        ];
        
        let result = matcher.find_match(&probe, &enrolled);
        assert!(result.is_some());
        let (user_id, _) = result.unwrap();
        assert_eq!(user_id, "user1");
    }
}
