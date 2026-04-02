use sha2::{Digest, Sha256};

pub fn hash_content(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_deterministic() {
        let h1 = hash_content(b"hello world");
        let h2 = hash_content(b"hello world");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_hash_different() {
        let h1 = hash_content(b"hello");
        let h2 = hash_content(b"world");
        assert_ne!(h1, h2);
    }
}
