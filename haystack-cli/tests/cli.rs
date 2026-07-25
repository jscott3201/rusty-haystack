// End-to-end tests for the `haystack` binary.
//
// These spawn the real binary via CARGO_BIN_EXE_haystack, so they cover the
// wiring that unit tests in the library crates cannot see — in particular
// whether a command attaches the ontology to the graph it builds. A graph
// without a namespace refuses every spec-match filter (`ph::Ahu`), and that
// wiring is invisible to `haystack-core`'s own tests because it lives entirely
// in the command functions here.

use std::io::Write;
use std::process::{Command, Stdio};

/// A three-entity zinc grid: one site, one AHU, one point.
const GRID: &str = "ver:\"3.0\"\n\
id,dis,site,equip,ahu,point,siteRef\n\
@s1,\"Site\",M,N,N,N,N\n\
@e1,\"AHU-1\",N,M,M,N,@s1\n\
@p1,\"Temp\",N,N,N,M,@s1\n";

/// Run the binary with `GRID` on stdin. `export` reads from stdin, not a path.
fn haystack_with_stdin(args: &[&str]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_haystack"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn haystack");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(GRID.as_bytes())
        .expect("write grid");
    child.wait_with_output().expect("run haystack")
}

#[test]
fn export_applies_a_plain_filter() {
    let out = haystack_with_stdin(&["export", "--filter", "site"]);

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("@s1"), "expected the site row:\n{stdout}");
    assert!(
        !stdout.contains("@e1"),
        "AHU must be filtered out:\n{stdout}"
    );
}

#[test]
fn export_applies_a_spec_match_filter() {
    // `export` built its graph with no namespace, so a spec term was refused
    // and the command exited 1 with a message telling a CLI user to call
    // `EntityGraph::with_namespace` — a Rust constructor.
    let out = haystack_with_stdin(&["export", "--filter", "ph::Ahu"]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "spec filter must not abort the command; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("with_namespace"),
        "error must not name a Rust constructor: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("@e1"), "expected the AHU row:\n{stdout}");
    assert!(
        !stdout.contains("@p1"),
        "the point is not an AHU:\n{stdout}"
    );
}

#[test]
fn export_still_rejects_an_unknown_spec() {
    // The namespace is attached now, so an unresolvable name is a real error
    // rather than a missing-namespace one.
    let out = haystack_with_stdin(&["export", "--filter", "ph::Bogus"]);

    assert!(!out.status.success(), "an unknown spec must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ph::Bogus"),
        "error names the spec: {stderr}"
    );
    assert!(
        !stderr.contains("no namespace"),
        "the graph has a namespace; the name is what is wrong: {stderr}"
    );
}
