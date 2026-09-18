//! Human-friendly node ids: `word-word-counter`, words picked by blake3(salt ‖ counter).

// D-4: curated in-repo lists, 1,000 each, append-only after v1 (reordering remaps ids).
const ADJ_TXT: &str = include_str!("words/adjectives.txt");
const NOUN_TXT: &str = include_str!("words/nouns.txt");

fn words(txt: &'static str) -> Vec<&'static str> {
    txt.split_whitespace().collect()
}

pub fn make(salt: &str, counter: u64) -> String {
    let mut h = blake3::Hasher::new();
    h.update(salt.as_bytes());
    h.update(&counter.to_le_bytes());
    let b = h.finalize();
    let b = b.as_bytes();
    let (adj, noun) = (words(ADJ_TXT), words(NOUN_TXT));
    let a = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize % adj.len();
    let n = u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize % noun.len();
    format!("{}-{}-{}", adj[a], noun[n], counter)
}

/// Per-clone 64-bit salt (16 hex chars): not cryptographic, just distinct across clones.
pub fn new_salt(root: &std::path::Path) -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let mut h = blake3::Hasher::new();
    h.update(&t.as_nanos().to_le_bytes());
    h.update(&std::process::id().to_le_bytes());
    h.update(root.as_os_str().as_encoded_bytes());
    h.finalize().to_hex()[..16].to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn shape_and_salt() {
        let a = super::make("s1", 7);
        assert!(a.ends_with("-7") && a.split('-').count() == 3);
        assert_eq!(a, super::make("s1", 7));
        let differ = (1..20)
            .filter(|c| super::make("s1", *c) != super::make("s2", *c))
            .count();
        assert!(differ > 15);
        assert_eq!(
            (
                super::words(super::ADJ_TXT).len(),
                super::words(super::NOUN_TXT).len()
            ),
            (1000, 1000)
        );
    }
}
