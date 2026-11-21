use std::process::Command;

#[test]
fn mcp_help_explains_the_explicit_stdio_config() {
    let output = Command::new(env!("CARGO_BIN_EXE_haystack"))
        .args(["mcp", "stdio", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("--config <FILE>"));
    assert!(help.contains("Unix pipe"));
}

#[cfg(not(feature = "mcp"))]
#[test]
fn mcp_disabled_build_fails_before_configuration_access() {
    let output = Command::new(env!("CARGO_BIN_EXE_haystack"))
        .args(["mcp", "stdio", "--config", "/not-an-existing-mcp-config"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("--features mcp")
    );
}

#[cfg(feature = "mcp")]
#[test]
fn mcp_rejects_unknown_config_without_echoing_parser_input() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mcp.toml");
    std::fs::write(&path, "version = 77\ncredential = 'do-not-echo-this'\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_haystack"))
        .args(["mcp", "stdio", "--config"])
        .arg(path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostic.contains("invalid MCP configuration"));
    assert!(!diagnostic.contains("do-not-echo-this"));
}

#[cfg(any(feature = "mcp", feature = "mcp-http"))]
#[test]
fn discovery_response_floor_rejects_rich_limits_before_dataset_access() {
    for (command, scope, diagnostic) in [
        #[cfg(feature = "mcp")]
        (
            "stdio",
            "principal = 'fixture'\nentity_ids = ['a']\n",
            "invalid MCP configuration",
        ),
        #[cfg(feature = "mcp-http")]
        (
            "http",
            "listen = '127.0.0.1:0'\nissuer = 'https://issuer.example.test'\nrequired_scope = 'haystack:read'\n[[keys]]\nkid = 'fixture'\nalgorithm = 'RS256'\npublic_key_der_base64 = ''\n[[subjects]]\nsubject = 'fixture'\nentity_ids = ['a']\n",
            "invalid MCP HTTP configuration",
        ),
    ] {
        for max_response_bytes in [8192, 16 * 1024 - 1] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("mcp.toml");
            std::fs::write(&path, format!("version = 1\ndataset_file = 'absent-private-dataset'\ndataset_format = 'trio'\noutput_profile = 'haystack-jeto-v1'\nmax_response_bytes = {max_response_bytes}\n{scope}")).unwrap();
            let output = Command::new(env!("CARGO_BIN_EXE_haystack"))
                .args(["mcp", command, "--config"])
                .arg(path)
                .output()
                .unwrap();
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            let error = String::from_utf8(output.stderr).unwrap();
            assert!(
                error.contains(diagnostic),
                "response floor did not fail before dataset access: {error}"
            );
            assert!(!error.contains("absent-private-dataset"));
            assert!(!error.contains("MCP HTTP ready:"));
        }
    }
}

#[cfg(any(feature = "mcp", feature = "mcp-http"))]
#[test]
fn selected_history_launch_rejects_invalid_authority_floors_and_fixtures() {
    use serde_json::json;
    for (command, scope, diagnostic) in [
        #[cfg(feature = "mcp")]
        (
            "stdio",
            "principal = 'fixture'\nentity_ids = ['a']\nhistory_read = true\n",
            "invalid MCP configuration",
        ),
        #[cfg(feature = "mcp-http")]
        (
            "http",
            "listen = '127.0.0.1:0'\nissuer = 'https://issuer.example.test'\nrequired_scope = 'haystack:read'\n[[keys]]\nkid = 'fixture'\nalgorithm = 'RS256'\npublic_key_der_base64 = ''\n[[subjects]]\nsubject = 'fixture'\nentity_ids = ['a']\nhistory_read = true\n",
            "invalid MCP HTTP configuration",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mcp.toml");
        let invoke = || {
            let output = Command::new(env!("CARGO_BIN_EXE_haystack"))
                .args(["mcp", command, "--config"])
                .arg(&path)
                .output()
                .unwrap();
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            let error = String::from_utf8(output.stderr).unwrap();
            assert!(!error.contains("MCP HTTP ready:"));
            assert!(!error.contains("private-fixture"));
            error
        };
        // Both failures precede attempts to open either configured file.
        for selection in [
            "",
            "history_file = 'private-fixture.json'\nmax_response_bytes = 2097151\n",
        ] {
            std::fs::write(&path, format!("version = 1\ndataset_file = 'private-fixture.trio'\ndataset_format = 'trio'\n{selection}{scope}")).unwrap();
            assert!(invoke().contains(diagnostic));
        }
        std::fs::write(
            directory.path().join("entities.trio"),
            "id: @a\nhis\nkind: \"Bool\"\ntz: \"UTC\"\n",
        )
        .unwrap();
        std::fs::write(&path, format!("version = 1\ndataset_file = 'entities.trio'\ndataset_format = 'trio'\nhistory_file = 'private-fixture.json'\n{scope}")).unwrap();
        let row = json!({"id":{"kind":"ref","value":"a","display":null},"ts":{"kind":"dateTime","seconds":"1717200000","nanos":0,"offset":0,"timezone":"UTC"},"val":{"kind":"bool","value":true}});
        let sample = json!({"version":1,"value":{"kind":"grid","meta":{},"cols":[{"name":"id","meta":{}},{"name":"ts","meta":{}},{"name":"val","meta":{}}],"rows":[row.clone()]}});
        for change in [
            "duplicate",
            "descending",
            "missing",
            "display",
            "null",
            "int",
            "timezone",
            "columns",
            "extra",
            "meta",
        ] {
            let mut invalid = sample.clone();
            match change {
                "duplicate" => invalid["value"]["rows"]
                    .as_array_mut()
                    .unwrap()
                    .push(row.clone()),
                "descending" => {
                    let mut earlier = row.clone();
                    earlier["ts"]["seconds"] = json!("1717199999");
                    invalid["value"]["rows"]
                        .as_array_mut()
                        .unwrap()
                        .push(earlier);
                }
                "missing" => invalid["value"]["rows"][0]["id"]["value"] = json!("private-fixture"),
                "display" => {
                    invalid["value"]["rows"][0]["id"]["display"] = json!("private-fixture")
                }
                "null" => invalid["value"]["rows"][0]["val"] = json!({"kind":"null"}),
                "int" => invalid["value"]["rows"][0]["val"] = json!({"kind":"int","value":"1"}),
                "timezone" => invalid["value"]["rows"][0]["ts"]["offset"] = json!(3600),
                "columns" => {
                    invalid["value"]["cols"].as_array_mut().unwrap().swap(0, 1);
                }
                "extra" => {
                    invalid["value"]["rows"][0]["extra"] = json!({"kind":"bool","value":true})
                }
                "meta" => {
                    invalid["value"]["meta"]["private"] =
                        json!({"kind":"str","value":"private-fixture"})
                }
                _ => unreachable!(),
            }
            std::fs::write(
                directory.path().join("private-fixture.json"),
                invalid.to_string(),
            )
            .unwrap();
            let error = invoke();
            assert!(error.contains("history"), "{command}/{change}: {error}");
        }
    }
}

#[cfg(all(feature = "mcp", unix))]
#[test]
fn rich_launch_accepts_explicit_typed_payload_and_admitted_nominal_scope() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("entities.json"), r#"{"version":1,"value":{"kind":"grid","meta":{},"cols":[],"rows":[{"id":{"kind":"ref","value":"a","display":null},"exact":{"kind":"int","value":"9007199254740993"},"version":{"kind":"nominal","spec":"sys::Version","catalog":"https://github.com/Project-Haystack/xeto","revision":"873b922451d3ef4c0c9c08ef3daa542f352d69f3","value":"1.2.3"}}]}}"#).unwrap();
    let config = directory.path().join("mcp.toml");
    std::fs::write(&config, "version = 1\ndataset_file = 'entities.json'\ndataset_format = 'typed-payload-v1'\noutput_profile = 'haystack-jeto-v1'\nnominal_specs = ['sys::Version']\nprincipal = 'fixture'\nentity_ids = ['a']\n").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_haystack"))
        .args(["mcp", "stdio", "--config"])
        .arg(config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdin.take());
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(5) {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("rich MCP launch did not finish after EOF");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("all admitted work released"));
}

#[cfg(all(feature = "mcp", unix))]
#[test]
fn fifo_config_and_dataset_without_a_writer_fail_promptly() {
    use std::{
        process::Stdio,
        time::{Duration, Instant},
    };
    let mut blocked = Vec::new();
    for source in ["config", "dataset", "history"] {
        let directory = tempfile::tempdir().unwrap();
        let fifo = directory.path().join("do-not-echo-this.fifo");
        assert!(
            Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let config = if source == "config" {
            fifo.clone()
        } else {
            let config = directory.path().join("mcp.toml");
            let selection = if source == "history" {
                std::fs::write(
                    directory.path().join("entities.trio"),
                    "id: @a\nhis\nkind: \"Bool\"\ntz: \"UTC\"\n",
                )
                .unwrap();
                "dataset_file = 'entities.trio'\nhistory_file = 'do-not-echo-this.fifo'\nhistory_read = true\n"
            } else {
                "dataset_file = 'do-not-echo-this.fifo'\n"
            };
            std::fs::write(&config,format!("version = 1\n{selection}dataset_format = 'trio'\nprincipal = 'fixture'\nentity_ids = ['a']\n")).unwrap();
            config
        };
        let mut child = Command::new(env!("CARGO_BIN_EXE_haystack"))
            .args(["mcp", "stdio", "--config"])
            .arg(config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let started = Instant::now();
        let prompt = loop {
            if child.try_wait().unwrap().is_some() {
                break true;
            }
            if started.elapsed() > Duration::from_secs(2) {
                child.kill().unwrap();
                break false;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let output = child.wait_with_output().unwrap();
        if !prompt {
            blocked.push(source);
            continue;
        }
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let diagnostic = String::from_utf8(output.stderr).unwrap();
        assert!(diagnostic.contains("must be regular files"));
        assert!(!diagnostic.contains("do-not-echo-this"));
    }
    assert!(
        blocked.is_empty(),
        "FIFO launch blocked before regular-file validation: {blocked:?}"
    );
}

#[test]
fn mcp_http_help_describes_explicit_loopback_configuration() {
    let output = Command::new(env!("CARGO_BIN_EXE_haystack"))
        .args(["mcp", "http", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("--config <FILE>"));
    assert!(help.contains("loopback"));
}
#[cfg(not(feature = "mcp-http"))]
#[test]
fn mcp_http_disabled_fails_before_configuration_access() {
    let output = Command::new(env!("CARGO_BIN_EXE_haystack"))
        .args(["mcp", "http", "--config", "/not-an-existing-http-config"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("--features mcp-http")
    );
}
#[cfg(feature = "mcp-http")]
#[test]
fn mcp_http_invalid_configuration_is_sanitized() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("http.toml");
    std::fs::write(&path, "version = 99\ncredential = 'do-not-echo-this'\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_haystack"))
        .args(["mcp", "http", "--config"])
        .arg(path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("invalid MCP HTTP configuration"));
    assert!(!error.contains("do-not-echo-this"));
}

#[cfg(any(feature = "mcp", feature = "mcp-http"))]
#[test]
fn semantic_selection_and_grants_fail_before_dataset_or_listener_work() {
    for (command, prefix, scope, invalid_config) in [
        #[cfg(feature = "mcp")]
        (
            "stdio",
            "",
            "principal = 'fixture'\nentity_ids = ['a']\n",
            "invalid MCP configuration",
        ),
        #[cfg(feature = "mcp-http")]
        (
            "http",
            "listen = '127.0.0.1:0'\nissuer = 'https://issuer.example.test'\nrequired_scope = 'haystack:read'\n[[keys]]\nkid = 'fixture'\nalgorithm = 'RS256'\npublic_key_der_base64 = ''\n",
            "[[subjects]]\nsubject = 'fixture'\nentity_ids = ['a']\n",
            "invalid MCP HTTP configuration",
        ),
    ] {
        for (selection, grants, diagnostic) in [
            (
                "",
                "h4_libraries = ['phIoT']\n",
                "require semantic_resources",
            ),
            (
                "semantic_resources = 'haystack-h4-semantics-v1'\n",
                "h4_libraries = ['phIoT']\nh4_definitions = ['notReal']\n",
                "H4 grants must name",
            ),
            (
                "semantic_resources = 'haystack-h4-semantics-v1'\n",
                "h4_libraries = ['ph']\nh4_definitions = ['ahu']\n",
                "H4 grants must name",
            ),
            (
                "semantic_resources = 'haystack-h4-semantics-v1'\n",
                "h4_libraries = ['sys']\n",
                "H4 grants must name",
            ),
            (
                "semantic_resources = 'haystack-h4-semantics-v1'\n",
                "h4_libraries = ['phIoT', 'phIoT']\n",
                "H4 grants must name",
            ),
            (
                "semantic_resources = 'haystack-h4-semantics-v1'\nmax_response_bytes = 131071\n",
                "h4_libraries = ['phIoT']\n",
                invalid_config,
            ),
            (
                "semantic_resources = 'unselected-xeto'\n",
                "",
                invalid_config,
            ),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("semantics.toml");
            std::fs::write(&path, format!("version = 1\ndataset_file = 'private-absent-dataset'\ndataset_format = 'trio'\n{selection}{prefix}{scope}{grants}")).unwrap();
            let output = Command::new(env!("CARGO_BIN_EXE_haystack"))
                .args(["mcp", command, "--config"])
                .arg(path)
                .output()
                .unwrap();
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            let error = String::from_utf8(output.stderr).unwrap();
            assert!(error.contains(diagnostic), "{command}: {error}");
            assert!(
                !error.contains("private-absent-dataset") && !error.contains("MCP HTTP ready:")
            );
        }
    }
}
