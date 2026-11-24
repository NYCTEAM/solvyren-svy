//! T1 start-up / spec-load binding spot checks for the isolated MX10 n=16 chain
//! (`pow-mx10-n16-local`, id `svy_mx10_n16_isolated_local`). Test-only, CPU only,
//! no GPU. Drives the real `WorkAlgorithm::from_spec` (called by
//! `service::new_partial` before any database is opened) and the real
//! `WorkAlgorithm::verify` / `gpu_pow_mx10::verify_pre_hash`. Nothing here
//! changes a chain identity, the MX10 computation, or any live difficulty.
use crate::chain_spec::{self, MX10_WORK_ALGORITHM, POW_MATRIX_BLOCK_REWARDS_LOCAL_ID, POW_MX10_LOCAL_ID};
use crate::gpu_pow_consensus::{WorkAlgorithm, EXPERIMENTAL_TARGET};
use crate::gpu_pow_mx10 as mx;
use sc_service::ChainSpec as _;
use sp_core::H256;

const V2_GENESIS: &str = "187cc1a37100ff8e2237bce04466ef11bafd1e692717a3be32aad72deee69054";
const V2_WORK_ALGORITHM: &str = "integer-matrix-2x2048x16";

fn h256(hex: &str) -> H256 {
    let bytes: Vec<u8> = (0..64).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
    H256::from_slice(&bytes)
}
fn hexs(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect() }
fn shape(n: u32) -> mx::EvalShape { mx::EvalShape::new(n, 2048).unwrap() }

fn json(spec: &chain_spec::ChainSpec) -> serde_json::Value {
    serde_json::from_str(&spec.as_json(true).unwrap()).unwrap()
}
fn reload(value: &serde_json::Value) -> chain_spec::ChainSpec {
    chain_spec::ChainSpec::from_json_bytes(serde_json::to_vec(value).unwrap()).unwrap()
}
fn from_spec_err(value: &serde_json::Value) -> String {
    match WorkAlgorithm::from_spec(&reload(value)) {
        Ok(w) => panic!("tampered spec accepted as {w:?}"),
        Err(e) => e,
    }
}

/// A certificate for (genesis, pre_hash) computed with `n`, whose ticket passes the
/// experimental target (so any rejection is not the cheap ticket gate).
fn cert_n(genesis: H256, pre: H256, n: u32) -> mx::Certificate {
    cert_ch(mx::challenge_from_pre_hash(genesis, pre), n)
}
fn cert_ch(challenge: [u8; 32], n: u32) -> mx::Certificate {
    (0..4096u64).map(|nonce| mx::compute(shape(n), challenge, nonce)).find(|c| mx::ticket(c) <= EXPERIMENTAL_TARGET).expect("nonce budget")
}

#[test]
fn t1_startup_from_spec_accepts_pinned_specs_and_constant_digest() {
    let v2 = crate::mx10_context::tests::existing_spec(false);
    let mx10 = crate::mx10_context::tests::existing_spec(true);
    assert_eq!(WorkAlgorithm::from_spec(&v2).unwrap(), WorkAlgorithm::V2);
    assert_eq!(WorkAlgorithm::from_spec(&mx10).unwrap(), crate::mx10_context::tests::dispatch());
    assert_eq!(mx10.id(), POW_MX10_LOCAL_ID);
    // The expected value is derived from code constants (WORKID domain, version 10,
    // map id, n=16, inner 2048), not read from the spec.
    assert_eq!(hexs(&mx::work_algorithm_digest(shape(16))), MX10_WORK_ALGORITHM);
    assert_eq!(MX10_WORK_ALGORITHM, "25cdf8f5acd57e409d8324c71c15b271314e4effd3ef2ca75ff2c897d2275d17");
    let j = json(&mx10);
    assert_eq!(j["properties"]["workAlgorithm"].as_str(), Some(MX10_WORK_ALGORITHM));
    let key = format!("0x{}", hexs(b"SVY-MX10-ISOLATED-WORKID\0"));
    assert_eq!(j["genesis"]["raw"]["top"][&key].as_str(), Some(format!("0x{MX10_WORK_ALGORITHM}").as_str()));
    assert_eq!(json(&v2)["properties"]["workAlgorithm"].as_str(), Some(V2_WORK_ALGORITHM));
}

#[test]
fn t1_startup_from_spec_rejects_tampered_mx10_work_algorithm() {
    let base = json(&crate::mx10_context::tests::existing_spec(true));
    let n32 = hexs(&mx::work_algorithm_digest(shape(32)));
    let mut flipped = MX10_WORK_ALGORITHM.to_string();
    flipped.replace_range(63..64, if flipped.ends_with('7') { "6" } else { "7" });
    for (label, value) in [
        ("old_v2_identity", serde_json::Value::from(V2_WORK_ALGORITHM)),
        ("mx10_n32_digest", n32.clone().into()),
        ("one_nibble_flipped", flipped.into()),
        ("uppercase", MX10_WORK_ALGORITHM.to_uppercase().into()),
        ("0x_prefixed", format!("0x{MX10_WORK_ALGORITHM}").into()),
        ("not_a_string", serde_json::json!(16)),
    ] {
        let mut t = base.clone();
        t["properties"]["workAlgorithm"] = value;
        let err = from_spec_err(&t);
        assert!(err.contains("requires the pinned n=16 workAlgorithm"), "{label}: {err}");
    }
    let mut missing = base.clone();
    missing["properties"].as_object_mut().unwrap().remove("workAlgorithm");
    assert!(from_spec_err(&missing).contains("requires the pinned n=16 workAlgorithm"));
}

#[test]
fn t1_startup_from_spec_rejects_mx10_identity_on_other_chain_and_tampered_v2() {
    let v2 = json(&crate::mx10_context::tests::existing_spec(false));
    assert_eq!(v2["id"].as_str(), Some(POW_MATRIX_BLOCK_REWARDS_LOCAL_ID));
    let mut t = v2.clone();
    t["properties"]["workAlgorithm"] = MX10_WORK_ALGORITHM.into();
    assert!(from_spec_err(&t).contains("requires its new isolated chain identity"));
    let mut t = v2.clone();
    t["properties"]["workAlgorithm"] = "integer-matrix-2x2048x32".into();
    assert!(from_spec_err(&t).contains("V2 matrix chain requires its unchanged workAlgorithm"));
    // MX10 work identity on an arbitrary other id is refused too.
    let mut t = json(&crate::mx10_context::tests::existing_spec(true));
    t["id"] = "svy_some_other_chain".into();
    assert!(from_spec_err(&t).contains("requires its new isolated chain identity"));
}

#[test]
fn t1_validator_shape_comes_from_trusted_constants_not_certificate() {
    // The certificate carries no n / inner-dimension field: 1 + 32 + 8 + 32 + 32 bytes.
    assert_eq!(mx::CERTIFICATE_BYTES, 105);
    assert_eq!(mx::INNER_DIM, 2048);
    // The dispatch shape is a literal in WorkAlgorithm::verify, not decoded.
    let consensus = include_str!("gpu_pow_consensus.rs");
    assert!(consensus.contains("Self::Mx10N16(context)"));
    assert!(include_str!("mx10_context.rs").contains("EvalShape::new(16, 2048)"));
    assert!(consensus.contains("/// Trusted dispatch configuration, never decoded from certificate bytes."));
    let service = include_str!("service.rs");
    assert!(service.contains("let work_algorithm = crate::gpu_pow_consensus::WorkAlgorithm::from_spec(config.chain_spec.as_ref())"));
    let g = crate::mx10_context::tests::context().genesis();
    let p = H256::repeat_byte(0x42);
    let good = cert_n(g, p, 16);
    assert!(crate::mx10_context::tests::dispatch().verify(g, p, &good.encode_fixed(), EXPERIMENTAL_TARGET));
    // A certificate honestly computed with another n (same challenge, ticket passes)
    // cannot steer the validator onto that n: it recomputes with n=16 and rejects.
    for n in [32u32, 64, 128] {
        let other = cert_n(g, p, n);
        assert!(mx::ticket(&other) <= EXPERIMENTAL_TARGET);
        assert_eq!(mx::verify_pre_hash(shape(16), g, p, &other.encode_fixed(), EXPERIMENTAL_TARGET), Err(mx::Error::WorkMismatch), "n={n}");
        assert!(!crate::mx10_context::tests::dispatch().verify(g, p, &other.encode_fixed(), EXPERIMENTAL_TARGET), "n={n}");
    }
    // Only the trusted caller can choose a shape, and only n in {16,32,64,128} with K=2048.
    assert!(mx::EvalShape::new(16, 1024).is_err());
    assert!(mx::EvalShape::new(17, 2048).is_err());
}

#[test]
fn t1_negative_old_version_bytes_rejected() {
    let g = crate::mx10_context::tests::context().genesis();
    let p = H256::repeat_byte(0x52);
    let good = cert_n(g, p, 16).encode_fixed();
    assert_eq!(good[0], 10);
    for v in [0u8, 1, 2, 3, 9, 11, 255] {
        let mut b = good.clone();
        b[0] = v;
        assert_eq!(mx::verify_pre_hash(shape(16), g, p, &b, EXPERIMENTAL_TARGET), Err(mx::Error::UnsupportedVersion), "v={v}");
        assert!(!crate::mx10_context::tests::dispatch().verify(g, p, &b, EXPERIMENTAL_TARGET), "v={v}");
    }
}

#[test]
fn t1_negative_v2_domain_and_v2_certificate_rejected() {
    let g = crate::mx10_context::tests::context().genesis();
    let p = H256::repeat_byte(0x62);
    // MX10 work bound to the V2 challenge domain (SVY V2 challenge for the same g/p).
    let v2_ch = crate::gpu_pow::challenge_from_pre_hash(g, p);
    assert_ne!(v2_ch, mx::challenge_from_pre_hash(g, p));
    let wrong_domain = cert_ch(v2_ch, 16);
    assert_eq!(mx::verify_pre_hash(shape(16), g, p, &wrong_domain.encode_fixed(), EXPERIMENTAL_TARGET), Err(mx::Error::ChallengeMismatch));
    assert!(!crate::mx10_context::tests::dispatch().verify(g, p, &wrong_domain.encode_fixed(), EXPERIMENTAL_TARGET));
    // A genuine V2 certificate is rejected by the MX10 verifier, and vice versa.
    let v2 = (0..8192).map(|n| crate::gpu_pow::compute(v2_ch, n)).find(|c| crate::gpu_pow::ticket(c) <= EXPERIMENTAL_TARGET).unwrap().encode_fixed();
    assert!(WorkAlgorithm::V2.verify(g, p, &v2, EXPERIMENTAL_TARGET));
    assert!(mx::verify_pre_hash(shape(16), g, p, &v2, EXPERIMENTAL_TARGET).is_err());
    assert!(!crate::mx10_context::tests::dispatch().verify(g, p, &v2, EXPERIMENTAL_TARGET));
    let mx10 = cert_n(g, p, 16).encode_fixed();
    assert!(crate::mx10_context::tests::dispatch().verify(g, p, &mx10, EXPERIMENTAL_TARGET));
    assert!(!WorkAlgorithm::V2.verify(g, p, &mx10, EXPERIMENTAL_TARGET));
}

#[test]
fn t1_negative_old_protocol_identity_genesis_rejected() {
    // Work bound to the V2 chain's genesis (old protocol identity) is not valid on the
    // MX10 chain, even with an identical pre-hash and a passing ticket.
    let mx10_genesis = h256("6172a0f0094cba19bfafe0b389dac397189903a6f816deb21378c3dba5200100");
    let v2_genesis = h256(V2_GENESIS);
    let p = H256::repeat_byte(0x72);
    let old = cert_n(v2_genesis, p, 16);
    assert!(mx::verify_pre_hash(shape(16), v2_genesis, p, &old.encode_fixed(), EXPERIMENTAL_TARGET).is_ok());
    assert!(!crate::mx10_context::tests::dispatch().verify(v2_genesis, p, &old.encode_fixed(), EXPERIMENTAL_TARGET));
    assert_eq!(mx::verify_pre_hash(shape(16), mx10_genesis, p, &old.encode_fixed(), EXPERIMENTAL_TARGET), Err(mx::Error::ChallengeMismatch));
    assert!(!crate::mx10_context::tests::dispatch().verify(mx10_genesis, p, &old.encode_fixed(), EXPERIMENTAL_TARGET));
    let new = cert_n(mx10_genesis, p, 16);
    assert!(crate::mx10_context::tests::dispatch().verify(mx10_genesis, p, &new.encode_fixed(), EXPERIMENTAL_TARGET));
}
