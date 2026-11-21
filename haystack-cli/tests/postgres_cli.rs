//! Product entry-point failures must stay actionable, nonzero and secret-safe.
use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_haystack"))
        .args(args)
        .env_remove("HAYSTACK_MISSING_TEST_PASSWORD")
        .output()
        .unwrap()
}

#[test]
fn postgres_selection_is_discoverable_and_conflicts_with_memory_seeding() {
    let help = run(&["serve", "--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--postgres-config"));
    for extra in [vec!["--demo"], vec!["--file", "entities.trio"]] {
        let mut args = vec!["serve", "--postgres-config", "unused", "--users", "unused"];
        args.extend(extra);
        let output = run(&args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
        assert!(output.stdout.is_empty());
    }
    let output = run(&["serve", "--postgres-config", "unused"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--users"));
}

#[cfg(not(feature = "postgres"))]
#[test]
fn disabled_postgres_is_a_clear_error_before_opening_files() {
    let output = run(&["serve", "--postgres-config", "absent", "--users", "absent"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--features postgres"));
    assert!(output.stdout.is_empty());
}

#[cfg(feature = "postgres")]
#[test]
fn postgres_rejects_invalid_configuration_and_users_without_echoing_secrets() {
    use haystack_server::auth::users::hash_password;
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("database with spaces.toml");
    let users = temp.path().join("users.toml");
    let valid_users = format!(
        "[users.reader]\npassword_hash = {:?}\npermissions = [\"read\"]\n",
        hash_password("only-local-test")
    );
    let valid_config = "version = 1\nhost = '127.0.0.1'\nport = 5432\ndatabase = 'local'\nusername = 'local'\nschema = 'standalone'\npassword_env = 'HAYSTACK_MISSING_TEST_PASSWORD'\n[transport]\nmode = 'loopback-plaintext'\n";
    for (source, credentials, expected) in [
        (
            "password = 'secret-sentinel'\ninvalid = [",
            valid_users.as_str(),
            "invalid PostgreSQL TOML",
        ),
        (
            "version = 1\nhost = 'secret-sentinel'\nunknown_key = true",
            valid_users.as_str(),
            "invalid PostgreSQL TOML",
        ),
        (
            valid_config,
            valid_users.as_str(),
            "password environment variable is missing",
        ),
        (valid_config, "[users]\n", "nonempty --users"),
        (
            valid_config,
            "[users.leak]\npassword_hash = 'secret-sentinel'\n",
            "invalid or unreadable users file",
        ),
    ] {
        std::fs::write(&config, source).unwrap();
        std::fs::write(&users, credentials).unwrap();
        let output = run(&[
            "serve",
            "--postgres-config",
            config.to_str().unwrap(),
            "--users",
            users.to_str().unwrap(),
        ]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains(expected),
            "expected bounded category: {expected}"
        );
        assert!(!error.contains("secret-sentinel"));
    }
}

#[cfg(feature = "postgres")]
#[test]
fn postgres_semantic_configuration_failures_do_not_connect_or_echo_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("database.toml");
    let users = temp.path().join("users.toml");
    std::fs::write(
        &users,
        format!(
            "[users.reader]\npassword_hash = {:?}\npermissions = [\"read\"]\n",
            haystack_server::auth::users::hash_password("only-local-test")
        ),
    )
    .unwrap();
    for (host, port, schema) in [
        ("127.0.0.1", 0, "standalone"),
        ("secret-sentinel.invalid", 5432, "standalone"),
        ("127.0.0.1", 5432, "public"),
    ] {
        std::fs::write(&config, format!("version = 1\nhost = {host:?}\nport = {port}\ndatabase = 'local'\nusername = 'local'\nschema = {schema:?}\npassword_env = 'HAYSTACK_TEST_DATABASE_PASSWORD'\n[transport]\nmode = 'loopback-plaintext'\n")).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_haystack"))
            .args([
                "serve",
                "--postgres-config",
                config.to_str().unwrap(),
                "--users",
                users.to_str().unwrap(),
            ])
            .env("HAYSTACK_TEST_DATABASE_PASSWORD", "secret-sentinel")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("invalid PostgreSQL configuration"));
        assert!(!error.contains("secret-sentinel"));
    }
}

#[test]
fn storage_administration_commands_are_discoverable() {
    let output = run(&["storage", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    for command in [
        "export",
        "import",
        "restore",
        "activate",
        "cancel",
        "status",
        "reconcile",
        "verify",
    ] {
        assert!(help.contains(command));
    }
    #[cfg(not(feature = "postgres"))]
    {
        let output = run(&["storage", "verify", "--backup", "absent"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("--features postgres"));
    }
}
#[cfg(feature = "postgres")]
#[test]
fn shared_configuration_requires_version_and_exactly_one_authority_selection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let common = "host='127.0.0.1'\nport=5432\ndatabase='fixture'\nusername='fixture'\npassword_env='HAYSTACK_MISSING_TEST_PASSWORD'\n";
    for (selection, expected) in [
        ("schema='source'\n", "invalid PostgreSQL TOML"),
        (
            "version=2\nschema='source'\n",
            "unsupported PostgreSQL configuration version",
        ),
        ("version=1\n", "choose exactly one"),
        (
            "version=1\nschema='source'\nselector_schema='control'\n",
            "choose exactly one",
        ),
        (
            "version=1\nselector_schema='control'\n",
            "password environment variable is missing",
        ),
    ] {
        std::fs::write(
            &path,
            format!("{common}{selection}[transport]\nmode='loopback-plaintext'\n"),
        )
        .unwrap();
        let output = run(&["storage", "status", "--config", path.to_str().unwrap()]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains(expected));
    }
}
