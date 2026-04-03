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

    #[test]
    fn test_hash_empty() {
        let h = hash_content(b"");
        assert!(!h.is_empty());
        // SHA-256 of empty string is well-known
        assert_eq!(h, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn test_hash_unicode() {
        let h = hash_content("你好世界".as_bytes());
        assert!(!h.is_empty());
        assert_eq!(h.len(), 64); // SHA-256 hex = 64 chars
    }

    #[test]
    fn test_hash_binary() {
        let h = hash_content(&[0x00, 0xFF, 0x80, 0x01]);
        assert_eq!(h.len(), 64);
    }

    #[test]
    fn test_hash_length() {
        // All hashes should be 64 hex chars (256 bits)
        assert_eq!(hash_content(b"test").len(), 64);
    }
}
