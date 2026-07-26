// End-to-end tests for the `haystack` binary.
//
// These spawn the real binary via CARGO_BIN_EXE_haystack, so they cover the
// wiring that unit tests in the library crates cannot see — in particular
// whether a command attaches the ontology to the graph it builds. A graph
// without a namespace refuses every spec-match filter (`ph::Ahu`), and that
// wiring is invisible to `haystack-core`'s own tests because it lives entirely
// in the command functions here.

use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// A three-entity zinc grid: one site, one AHU, one point.
const GRID: &str = "ver:\"3.0\"\n\
id,dis,site,equip,ahu,point,siteRef\n\
@s1,\"Site\",M,N,N,N,N\n\
@e1,\"AHU-1\",N,M,M,N,@s1\n\
@p1,\"Temp\",N,N,N,M,@s1\n";

/// The same entities as `GRID`, in Trio, to exercise serve's extension dispatch.
const TRIO_GRID: &str = "id:@s1\n\
dis:\"Site\"\n\
site\n\
---\n\
id:@e1\n\
dis:\"AHU-1\"\n\
equip\n\
ahu\n\
siteRef:@s1\n\
---\n\
id:@p1\n\
dis:\"Temp\"\n\
point\n\
siteRef:@s1\n";

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

struct ServeChild {
    child: Child,
    port: u16,
}

impl ServeChild {
    /// Spawn a server on a kernel-assigned port and learn that port from the child.
    ///
    /// The obvious approach — bind :0 in the test, read the port, drop the listener,
    /// pass it to the child — has a race that cannot be closed from this side. The
    /// port is unowned between the drop and the child's bind, so another process can
    /// take it; the child then dies of address-in-use while the readiness check
    /// still succeeds, because the port genuinely is listening. It just belongs to
    /// someone else. Readiness could only ever prove that *someone* was listening.
    ///
    /// Passing `--port 0` and reading the address back from our own child's stdout
    /// makes the bind atomic and proves ownership: the line cannot appear unless
    /// this child bound that port. The server prints it after binding and before
    /// accepting, so it is a readiness signal too.
    fn spawn(extra_args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_haystack"))
            .arg("serve")
            .args(["--host", "127.0.0.1", "--port", "0"])
            .args(extra_args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn haystack serve");

        let stdout = child.stdout.take().expect("piped stdout");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("read the listening banner");
        let port = line
            .rsplit_once(':')
            .and_then(|(_, port)| port.trim().parse::<u16>().ok())
            .unwrap_or_else(|| panic!("could not parse a port from serve output: {line:?}"));

        Self { child, port }
    }

    fn read(&self, filter: &str) -> HttpResponse {
        self.post("/api/read", &format!("ver:\"3.0\"\nfilter\n\"{filter}\"\n"))
    }

    fn post(&self, path: &str, body: &str) -> HttpResponse {
        let request = format!(
            "POST {path} HTTP/1.1\r\n\
             Host: 127.0.0.1:{}\r\n\
             Content-Type: text/zinc\r\n\
             Accept: text/zinc\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n\
             {body}",
            self.port,
            body.len()
        );
        let mut stream =
            TcpStream::connect(("127.0.0.1", self.port)).expect("connect to haystack serve");
        // Without a deadline a server that accepts the connection and then wedges
        // blocks this read forever: the test never reaches an assertion, never drops
        // its guard, and the whole test binary hangs instead of failing.
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("set read timeout");
        stream
            .write_all(request.as_bytes())
            .expect("send read request");
        let mut bytes = Vec::new();
        let mut chunk = [0; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => bytes.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == ErrorKind::ConnectionReset && !bytes.is_empty() => {
                    break;
                }
                Err(error) => panic!("read HTTP response: {error}"),
            }
        }
        let response = String::from_utf8(bytes).expect("UTF-8 HTTP response");
        HttpResponse::parse(&response)
    }
}

impl Drop for ServeChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct HttpResponse {
    status: u16,
    body: String,
}

impl HttpResponse {
    fn parse(response: &str) -> Self {
        let (head, body) = response
            .split_once("\r\n\r\n")
            .unwrap_or_else(|| panic!("malformed HTTP response:\n{response}"));
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|status| status.parse().ok())
            .unwrap_or_else(|| panic!("missing HTTP status in:\n{response}"));
        Self {
            status,
            body: body.to_string(),
        }
    }
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

#[test]
fn serve_demo_discriminates_spec_match_filters() {
    let server = ServeChild::spawn(&["--demo"]);

    let ahus = server.read("ph::Ahu");
    assert_eq!(ahus.status, 200, "AHU response:\n{}", ahus.body);
    assert!(
        ahus.body.contains("@demo-ahu-1"),
        "expected an AHU:\n{}",
        ahus.body
    );
    assert!(
        !ahus.body.contains("@demo-vav-1-01-zat"),
        "a point must not match ph::Ahu:\n{}",
        ahus.body
    );

    let points = server.read("ph::Point");
    assert_eq!(points.status, 200, "Point response:\n{}", points.body);
    assert!(
        points.body.contains("@demo-vav-1-01-zat"),
        "expected a point:\n{}",
        points.body
    );
    assert!(
        !points.body.contains("@demo-ahu-1"),
        "an AHU must not match ph::Point:\n{}",
        points.body
    );

    let bogus = server.read("ph::Bogus");
    assert_eq!(
        bogus.status, 400,
        "unknown spec response had an unexpected status:\n{}",
        bogus.body
    );
    assert!(
        bogus.body.contains("ph::Bogus"),
        "error must name the unknown spec:\n{}",
        bogus.body
    );
}

#[test]
fn serve_trio_file_discriminates_spec_match_filters() {
    let mut file = tempfile::Builder::new()
        .suffix(".trio")
        .tempfile()
        .expect("create Trio fixture");
    file.write_all(TRIO_GRID.as_bytes())
        .expect("write Trio fixture");
    let path = file.path().to_str().expect("UTF-8 fixture path");
    let server = ServeChild::spawn(&["--file", path]);

    let ahus = server.read("ph::Ahu");
    assert_eq!(ahus.status, 200, "AHU response:\n{}", ahus.body);
    assert!(
        ahus.body.contains("@e1"),
        "expected the AHU:\n{}",
        ahus.body
    );
    assert!(
        !ahus.body.contains("@p1"),
        "the point must not match ph::Ahu:\n{}",
        ahus.body
    );

    let points = server.read("ph::Point");
    assert_eq!(points.status, 200, "Point response:\n{}", points.body);
    assert!(
        points.body.contains("@p1"),
        "expected the point:\n{}",
        points.body
    );
    assert!(
        !points.body.contains("@e1"),
        "the AHU must not match ph::Point:\n{}",
        points.body
    );
}

#[test]
fn serve_empty_graph_accepts_a_spec_match_filter() {
    let server = ServeChild::spawn(&[]);

    let response = server.read("ph::Ahu");
    assert_eq!(
        response.status, 200,
        "an empty namespaced graph must accept ph::Ahu:\n{}",
        response.body
    );
    assert!(
        !response.body.contains("err"),
        "an accepted filter must not return an error grid:\n{}",
        response.body
    );
    assert!(
        !response.body.contains("@"),
        "an empty graph must not return entities:\n{}",
        response.body
    );
}

/// Loading a library must make its specs filterable, not just visible to the
/// schema endpoints (issue #23).
///
/// The server holds its own mutable namespace while every graph holds an `Arc`
/// snapshot, so before the fix `loadLib` mutated one and left the other alone:
/// `/api/specs` listed `myLib::Widget` and `/api/read` rejected it as undefined,
/// one minute apart on the same server. This is an end-to-end test because that is
/// the only level at which the disagreement is visible — each endpoint was
/// individually correct about the namespace it could see.
#[test]
fn loading_a_lib_makes_its_specs_filterable() {
    let server = ServeChild::spawn(&["--demo"]);

    // Before: the spec does not exist anywhere, so a filter naming it is refused.
    let before = server.read("myLib::Widget");
    assert_eq!(
        before.status, 400,
        "an unknown spec must be refused before the lib is loaded:\n{}",
        before.body
    );

    let loaded = server.post(
        "/api/loadLib",
        "ver:\"3.0\"\nname,source\n\"myLib\",\"Widget : Dict { widget: Marker }\"\n",
    );
    assert_eq!(loaded.status, 200, "loadLib failed:\n{}", loaded.body);
    assert!(
        loaded.body.contains("myLib::Widget"),
        "loadLib must report the spec it created:\n{}",
        loaded.body
    );

    // The schema side has always agreed. Assert it so a regression that breaks
    // this instead of the read side is still caught.
    let specs = server.post("/api/specs", "ver:\"3.0\"\nlib\n\"myLib\"\n");
    assert_eq!(specs.status, 200, "specs failed:\n{}", specs.body);
    assert!(
        specs.body.contains("myLib::Widget"),
        "specs must list the loaded spec:\n{}",
        specs.body
    );

    // The bug: this returned 400 "filter names a spec that this graph's namespace
    // does not define", contradicting the two calls above.
    let after = server.read("myLib::Widget");
    assert_eq!(
        after.status, 200,
        "a loaded spec must be filterable; the graph's namespace did not get it:\n{}",
        after.body
    );
    assert!(
        !after.body.contains("does not define"),
        "the graph still rejects the loaded spec:\n{}",
        after.body
    );
}

/// Unloading a library must make its specs unfilterable again, including after the
/// spec has already been queried once.
///
/// This is the direction the first version of the fix got wrong. The query cache is
/// keyed on (filter, entity version), and swapping the namespace deliberately does
/// not bump the version — no entity changed, so waking watchers would be noise. But
/// the cache hit path returns before spec validation runs, so a successful
/// `myLib::Widget` query cached before the unload was still served afterwards.
#[test]
fn unloading_a_lib_makes_its_specs_unfilterable_again() {
    let server = ServeChild::spawn(&["--demo"]);

    let loaded = server.post(
        "/api/loadLib",
        "ver:\"3.0\"\nname,source\n\"myLib\",\"Widget : Dict { widget: Marker }\"\n",
    );
    assert_eq!(loaded.status, 200, "loadLib failed:\n{}", loaded.body);

    // Query it once so the result is cached. Without this the bug is invisible.
    let cached = server.read("myLib::Widget");
    assert_eq!(
        cached.status, 200,
        "the loaded spec must be filterable:\n{}",
        cached.body
    );

    let unloaded = server.post("/api/unloadLib", "ver:\"3.0\"\nname\n\"myLib\"\n");
    assert_eq!(unloaded.status, 200, "unloadLib failed:\n{}", unloaded.body);

    // No entity changed across the unload, so the entity version is identical and a
    // version-keyed cache would still hold the pre-unload answer.
    let after = server.read("myLib::Widget");
    assert_eq!(
        after.status, 400,
        "an unloaded spec must stop being filterable; a stale cached result was \
         served instead:\n{}",
        after.body
    );
    // Pin the cause, not just the status. Asserting 400 alone would be satisfied by
    // any unrelated bad request, so this test could keep passing while the stale
    // cache came back.
    assert!(
        after.body.contains("does not define"),
        "the 400 must be because the spec is undefined, not for some other reason:\n{}",
        after.body
    );
}

/// Successive lib mutations must leave the graph agreeing with the server, not
/// holding whichever snapshot happened to be published last.
///
/// Each mutation snapshots the namespace and then publishes it to the graph as a
/// separate step, because holding the namespace lock across the graph update fixes
/// a lock order a custom router could invert. That makes the ORDER of publishes
/// load-bearing, so this walks a sequence where a stale publish would be visible:
/// after unloading only `libA`, `libB` must survive and `libA` must not.
#[test]
fn successive_lib_mutations_leave_the_graph_in_step() {
    let server = ServeChild::spawn(&["--demo"]);

    for (name, spec) in [("libA", "Alpha"), ("libB", "Beta")] {
        let loaded = server.post(
            "/api/loadLib",
            &format!(
                "ver:\"3.0\"\nname,source\n\"{name}\",\"{spec} : Dict {{ {} : Marker }}\"\n",
                spec.to_lowercase()
            ),
        );
        assert_eq!(
            loaded.status, 200,
            "loading {name} failed:\n{}",
            loaded.body
        );
    }

    assert_eq!(
        server.read("libA::Alpha").status,
        200,
        "libA must be filterable"
    );
    assert_eq!(
        server.read("libB::Beta").status,
        200,
        "libB must be filterable"
    );

    let unloaded = server.post("/api/unloadLib", "ver:\"3.0\"\nname\n\"libA\"\n");
    assert_eq!(unloaded.status, 200, "unloadLib failed:\n{}", unloaded.body);

    let a = server.read("libA::Alpha");
    assert_eq!(
        a.status, 400,
        "libA was unloaded and must be gone:\n{}",
        a.body
    );

    // The one that would fail on a stale publish: unloading A must not roll the
    // graph back to a snapshot that predates B.
    let b = server.read("libB::Beta");
    assert_eq!(
        b.status, 200,
        "libB must survive unloading libA; the graph was rolled back:\n{}",
        b.body
    );
}
