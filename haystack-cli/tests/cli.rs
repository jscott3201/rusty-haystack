// End-to-end tests for the `haystack` binary.
//
// These spawn the real binary via CARGO_BIN_EXE_haystack, so they cover the
// wiring that unit tests in the library crates cannot see — in particular
// whether a command attaches the ontology to the graph it builds. A graph
// without a namespace refuses every spec-match filter (`ph::Ahu`), and that
// wiring is invisible to `haystack-core`'s own tests because it lives entirely
// in the command functions here.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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
    fn spawn(extra_args: &[&str]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("reserve a test port");
        let port = listener.local_addr().expect("reserved port address").port();
        drop(listener);

        let child = Command::new(env!("CARGO_BIN_EXE_haystack"))
            .arg("serve")
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .args(extra_args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn haystack serve");
        let mut server = Self { child, port };
        server.wait_until_ready();
        server
    }

    fn wait_until_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
                return;
            }
            if let Some(status) = self.child.try_wait().expect("poll serve process") {
                panic!(
                    "haystack serve exited with {status} before binding port {}",
                    self.port
                );
            }
            assert!(
                Instant::now() < deadline,
                "haystack serve did not become ready on port {} within 5 seconds",
                self.port
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn read(&self, filter: &str) -> HttpResponse {
        let body = format!("ver:\"3.0\"\nfilter\n\"{filter}\"\n");
        let request = format!(
            "POST /api/read HTTP/1.1\r\n\
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
