//! M4 storage properties: CDC boundary stability and checkpoint dedup.

use std::collections::HashSet;
use std::path::Path;

use pollard_objects::Store;
use proptest::prelude::*;

fn rand_bytes(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// Store `data` as a file and return its chunk list.
fn chunk(store: &Store, dir: &Path, data: &[u8]) -> Vec<(String, u64)> {
    let p = dir.join("f.bin");
    std::fs::write(&p, data).unwrap();
    let (h, _) = store.put_file(&p).unwrap();
    store.chunks_of(&h).unwrap().expect("large file is chunked")
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, failure_persistence: None, ..ProptestConfig::default() })]

    /// Inserting bytes at offset k only changes chunks around k: every chunk that ends
    /// before k survives, and the chunk stream resynchronises within 1 MB after k
    /// (a run of max-size 128 KB chunks can delay the resync by a few chunks).
    #[test]
    fn boundaries_stable_under_insertion(
        seed in any::<u64>(),
        len in (2usize << 20)..(3usize << 20),
        at_frac in 0.0f64..1.0,
        insert in prop::collection::vec(any::<u8>(), 1..5000),
    ) {
        let t = tempfile::tempdir().unwrap();
        let store = Store::new(t.path().join(".pollard"));
        let data = rand_bytes(len, seed);
        let k = (len as f64 * at_frac) as usize;
        let mut edited = data[..k].to_vec();
        edited.extend_from_slice(&insert);
        edited.extend_from_slice(&data[k..]);

        let a = chunk(&store, t.path(), &data);
        let b: HashSet<_> = chunk(&store, t.path(), &edited).into_iter().map(|c| c.0).collect();

        let mut end = 0u64;
        for (h, n) in &a {
            let start = end;
            end += n;
            if !b.contains(h) {
                prop_assert!(end as usize >= k, "chunk ending at {end} before insert at {k} changed");
                prop_assert!((start as usize) < k + (1 << 20), "chunk at {start} changed, > 1 MB after insert at {k}");
            }
        }
    }
}

/// §9 M4 (v3): two 50 MB checkpoints differing in one contiguous in-place region of 1 %
/// of the bytes share >= 95 % of chunks by count and by bytes.
#[test]
fn one_percent_change_shares_95_percent() {
    let t = tempfile::tempdir().unwrap();
    let store = Store::new(t.path().join(".pollard"));
    let len = 50 << 20;
    let a = rand_bytes(len, 42);
    let mut b = a.clone();
    let start = len / 3;
    for (j, byte) in b[start..start + len / 100].iter_mut().enumerate() {
        *byte ^= (j as u8) | 1;
    }
    let ca = chunk(&store, t.path(), &a);
    let cb = chunk(&store, t.path(), &b);
    let set_a: HashSet<_> = ca.iter().map(|c| &c.0).collect();
    let shared: Vec<_> = cb.iter().filter(|c| set_a.contains(&c.0)).collect();
    let by_count = shared.len() as f64 / cb.len() as f64;
    let by_bytes = shared.iter().map(|c| c.1).sum::<u64>() as f64 / len as f64;
    assert!(by_count >= 0.95 && by_bytes >= 0.95, "shared: {by_count:.4} by count, {by_bytes:.4} by bytes");
}
