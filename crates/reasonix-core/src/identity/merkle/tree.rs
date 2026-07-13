use crate::identity::hashing::{DefaultHasher, PromptHash, PromptHasher};

#[derive(Debug, Clone)]
pub struct IncrementalMerkleTree {
    leaves: Vec<PromptHash>,
    // Tree represented as an array of layers, where layers[0] is the leaves,
    // layers[1] is the next level up, etc.
    layers: Vec<Vec<PromptHash>>,
}

impl Default for IncrementalMerkleTree {
    fn default() -> Self {
        Self::new()
    }
}

impl IncrementalMerkleTree {
    pub fn new() -> Self {
        Self {
            leaves: Vec::new(),
            layers: vec![Vec::new()],
        }
    }

    /// Appends a new leaf hash to the tree and updates the required internal nodes in O(log n).
    pub fn push(&mut self, leaf: PromptHash) {
        self.leaves.push(leaf.clone());
        self.update_layers(leaf);
    }

    /// Returns the current Merkle Root hash of the conversation.
    pub fn root_hash(&self) -> PromptHash {
        if self.leaves.is_empty() {
            return PromptHash::default();
        }
        self.layers.last().and_then(|l| l.first()).cloned().unwrap_or_default()
    }

    /// Internal O(log n) update
    fn update_layers(&mut self, mut current_hash: PromptHash) {
        // If the tree was empty, initialize layers[0]
        if self.layers.is_empty() {
            self.layers.push(Vec::new());
        }

        self.layers[0].push(current_hash.clone());
        let mut level = 0;
        let mut idx = self.layers[0].len() - 1;

        while idx > 0 || level < self.layers.len() - 1 {
            if idx % 2 == 1 {
                // We are the right child. Combine with left child.
                let left_hash = &self.layers[level][idx - 1];
                let mut combined = Vec::with_capacity(64);
                combined.extend_from_slice(&left_hash.0);
                combined.extend_from_slice(&current_hash.0);
                current_hash = DefaultHasher::hash(&combined);
                
                // Move up
                level += 1;
                idx /= 2;

                if level >= self.layers.len() {
                    self.layers.push(Vec::new());
                }

                if idx < self.layers[level].len() {
                    self.layers[level][idx] = current_hash.clone();
                } else {
                    self.layers[level].push(current_hash.clone());
                }
            } else {
                // We are a left child. If there's no right child yet, 
                // we just promote ourselves directly (or combine with default hash,
                // but standard incremental trees often just pass the left child up if unbalanced).
                // Here we just duplicate the left child hash to keep it simple and balanced-equivalent.
                let mut combined = Vec::with_capacity(64);
                combined.extend_from_slice(&current_hash.0);
                combined.extend_from_slice(&current_hash.0); // Duplicate for unbalanced right
                current_hash = DefaultHasher::hash(&combined);
                
                level += 1;
                idx /= 2;
                
                if level >= self.layers.len() {
                    self.layers.push(Vec::new());
                }
                
                if idx < self.layers[level].len() {
                    self.layers[level][idx] = current_hash.clone();
                } else {
                    self.layers[level].push(current_hash.clone());
                }
            }
        }
    }
}
