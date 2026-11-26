use std::path::Path;

use oa::profiler::parse_v8_cpuprofile;
use oa::sha256::sha256_hex;
use oa::stats::{compare_samples, mann_whitney_u_pvalue, summarize, BenchVerdict};

#[test]
fn sha256_nist_vectors() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
}

#[test]
fn stats_detects_clear_speedup() {
    let baseline = [
        100.0, 102.0, 99.0, 101.0, 103.0, 98.0, 100.5, 101.5, 99.5, 102.5,
    ];
    let candidate = [50.0, 51.0, 49.5, 50.5, 51.2, 49.0, 50.2, 50.8, 49.8, 51.0];
    let res = compare_samples(&baseline, &candidate, 1.03, 0.01);
    assert_eq!(res.verdict, BenchVerdict::Faster);
    assert!(res.speedup_median > 1.95 && res.speedup_median < 2.05);
    assert!(res.ci95_low > 1.9);
    assert!(res.mann_whitney_p < 0.001);
}

#[test]
fn stats_rejects_noise() {
    let baseline = [
        100.0, 101.0, 99.0, 100.5, 99.5, 100.2, 99.8, 100.4, 99.6, 100.1,
    ];
    let candidate = [
        99.8, 100.9, 99.2, 100.1, 99.7, 100.0, 99.9, 100.3, 99.5, 100.2,
    ];
    let res = compare_samples(&baseline, &candidate, 1.03, 0.01);
    assert_eq!(res.verdict, BenchVerdict::Noise);
    assert!(res.mann_whitney_p > 0.2);
}

#[test]
fn stats_detects_regression() {
    let baseline = [50.0, 50.5, 49.5, 50.2, 49.8, 50.1, 49.9, 50.4, 49.6, 50.3];
    let candidate = [80.0, 81.0, 79.0, 80.5, 79.5, 80.2, 79.8, 80.6, 79.4, 80.1];
    let res = compare_samples(&baseline, &candidate, 1.03, 0.01);
    assert_eq!(res.verdict, BenchVerdict::Slower);
    assert!(res.ci95_high < 0.7);
    assert!(res.mann_whitney_p < 0.001);
}

#[test]
fn mann_whitney_handles_all_ties() {
    let a = [10.0; 8];
    let b = [10.0; 8];
    assert_eq!(mann_whitney_u_pvalue(&a, &b), 1.0);
    let s = summarize(&a);
    assert_eq!(s.median_ms, 10.0);
    assert_eq!(s.mad_ms, 0.0);
}

#[test]
fn parses_v8_cpuprofile_hotspots() {
    let sample = r#"{
        "nodes": [
            {"id": 1, "callFrame": {"functionName": "(root)", "url": "", "lineNumber": -1}, "children": [2]},
            {"id": 2, "callFrame": {"functionName": "parsePayload", "url": "file:///app/parser.js", "lineNumber": 41}, "children": [3]},
            {"id": 3, "callFrame": {"functionName": "dedupeSlow", "url": "file:///app/parser.js", "lineNumber": 77}, "children": []}
        ],
        "samples": [3, 3, 3, 2],
        "timeDeltas": [20000, 20000, 20000, 10000]
    }"#;
    let rep = parse_v8_cpuprofile(sample, 70.0, Path::new("/app"), 5).expect("parse v8 profile");
    assert_eq!(rep.profiler, "node:v8-cpu-prof");
    assert_eq!(rep.hotspots.len(), 2);
    assert_eq!(rep.hotspots[0].function, "dedupeSlow");
    assert_eq!(rep.hotspots[0].line, 78);
    assert!(rep.hotspots[0].self_pct > 80.0);
    assert_eq!(rep.hotspots[1].function, "parsePayload");
    assert!(rep.hotspots[1].cum_pct > 99.0);
}
