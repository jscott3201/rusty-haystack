use clap::Subcommand;

#[derive(Subcommand)]
pub enum Action {
    /// Quiesce source and publish an immutable canonical backup; repeat to recover an interrupted export
    Export {
        #[arg(long)]
        config: String,
        #[arg(long)]
        operation: String,
        #[arg(long)]
        source: String,
        #[arg(long)]
        target: String,
        #[arg(long)]
        output: String,
    },
    /// Validate and stage this operation's backup in its fresh target schema
    Import {
        #[arg(long)]
        config: String,
        #[arg(long)]
        operation: String,
        #[arg(long)]
        backup: String,
    },
    /// Quiesce current selection and stage an archived cut under a NEW operation ID
    Restore {
        #[arg(long)]
        config: String,
        #[arg(long)]
        operation: String,
        #[arg(long)]
        target: String,
        #[arg(long)]
        backup: String,
    },
    /// Atomically retire source, activate target and move the selector; retry reconciles a lost acknowledgement
    Activate {
        #[arg(long)]
        config: String,
        #[arg(long)]
        operation: String,
    },
    /// Cancel before selection and reactivate source; committed selections cannot be cancelled
    Cancel {
        #[arg(long)]
        config: String,
        #[arg(long)]
        operation: String,
    },
    /// Read committed selection and optional operation outcome
    Status {
        #[arg(long)]
        config: String,
        #[arg(long)]
        operation: Option<String>,
    },
    /// Reconcile an uncertain operation by reading its committed journal outcome
    Reconcile {
        #[arg(long)]
        config: String,
        #[arg(long)]
        operation: String,
    },
    /// Validate an archive offline and display its original cut and integrity digest
    Verify {
        #[arg(long)]
        backup: String,
    },
}
pub fn run(action: Action) {
    #[cfg(not(feature = "postgres"))]
    {
        let _ = action;
        eprintln!("PostgreSQL storage requires a build with --features postgres");
        std::process::exit(1);
    }
    #[cfg(feature = "postgres")]
    if let Err(error) = enabled::run(action) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
#[cfg(feature = "postgres")]
mod enabled {
    use super::Action;
    use haystack_app::postgres::{
        PostgresLimits,
        admin::{self, Backup, Command, MAX_BACKUP_BYTES, Outcome},
    };
    use haystack_core::xeto::catalog::{ActivatedCatalog, Catalog};
    use std::{
        io::{Read, Write},
        path::Path,
        sync::Arc,
    };
    fn read(path: &Path) -> Result<Vec<u8>, String> {
        let mut bytes = vec![];
        std::fs::File::open(path)
            .and_then(|f| f.take(MAX_BACKUP_BYTES as u64 + 1).read_to_end(&mut bytes))
            .map_err(|_| "unreadable backup")?;
        if bytes.len() > MAX_BACKUP_BYTES {
            return Err("backup size limit exceeded".into());
        };
        Ok(bytes)
    }
    pub(super) fn run(action: Action) -> Result<(), String> {
        let catalog = Catalog::load_http_pinned().map_err(|_| "cannot load pinned HTTP catalog")?;
        if let Action::Verify { backup } = action {
            let catalog = Arc::new(
                ActivatedCatalog::new(catalog, None)
                    .map_err(|_| "cannot activate pinned catalog")?,
            );
            let backup = Backup::parse(
                &read(Path::new(&backup))?,
                &catalog,
                &PostgresLimits::default(),
            )
            .map_err(|e| e.to_string())?;
            println!(
                "{}",
                serde_json::json!({"manifest":backup.manifest(),"sha256":backup.sha256()})
            );
            return Ok(());
        }
        let mut output = None;
        let (file, command) = match action {
            Action::Export {
                config,
                operation,
                source,
                target,
                output: path,
            } => {
                output = Some(path);
                (
                    config,
                    Command::Export {
                        operation,
                        source,
                        target,
                    },
                )
            }
            Action::Import {
                config,
                operation,
                backup,
            } => (
                config,
                Command::Import {
                    operation,
                    backup: read(Path::new(&backup))?,
                },
            ),
            Action::Restore {
                config,
                operation,
                target,
                backup,
            } => (
                config,
                Command::Restore {
                    operation,
                    target,
                    backup: read(Path::new(&backup))?,
                },
            ),
            Action::Activate { config, operation } => (config, Command::Activate { operation }),
            Action::Cancel { config, operation } => (config, Command::Cancel { operation }),
            Action::Status { config, operation } => (config, Command::Status { operation }),
            Action::Reconcile { config, operation } => (
                config,
                Command::Status {
                    operation: Some(operation),
                },
            ),
            Action::Verify { .. } => unreachable!(),
        };
        let loaded = crate::commands::postgres_config::load(Path::new(&file))?;
        if !loaded.selected {
            return Err("storage administration requires selector_schema configuration".into());
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|_| "cannot create storage runtime")?;
        match runtime
            .block_on(admin::execute(loaded.config, catalog, command))
            .map_err(|e| {
                format!("{e}; inspect storage reconcile before retrying an uncertain mutation")
            })? {
            Outcome::Status(status) => println!(
                "{}",
                serde_json::to_string(&status).map_err(|_| "cannot encode status")?
            ),
            Outcome::Backup(backup) => {
                publish(
                    Path::new(output.as_deref().ok_or("missing backup output")?),
                    backup.bytes(),
                )?;
                println!(
                    "{}",
                    serde_json::json!({"manifest":backup.manifest(),"sha256":backup.sha256(),"published":true})
                );
            }
        }
        Ok(())
    }
    /// Same-directory hard-link publication is atomic and refuses replacement.
    /// Sync data before publication and the parent after link and temp removal.
    /// An existing identical artifact is an idempotent retry, never an overwrite.
    fn publish(path: &Path, bytes: &[u8]) -> Result<(), String> {
        publish_checked(path, bytes, |_| Ok(()))
    }
    fn publish_checked(
        path: &Path,
        bytes: &[u8],
        mut checkpoint: impl FnMut(bool) -> Result<(), String>,
    ) -> Result<(), String> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let directory = std::fs::File::open(parent).map_err(|_| "unreadable backup directory")?;
        if let Ok(metadata) = std::fs::symlink_metadata(path) {
            if !metadata.file_type().is_file() || read(path)? != bytes {
                return Err("backup output already exists with different contents or type".into());
            };
            std::fs::File::open(path)
                .and_then(|f| f.sync_all())
                .map_err(|_| "backup file sync failed")?;
            directory
                .sync_all()
                .map_err(|_| "backup directory sync failed")?;
            return Ok(());
        }
        let temp = parent.join(format!(
            ".haystack-backup-{}-{}.tmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| "invalid clock")?
                .as_nanos()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temp)
            .map_err(|_| "cannot create private backup temporary file")?;
        let result = (|| {
            file.write_all(bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| "backup write or sync failed")?;
            checkpoint(false)?;
            std::fs::hard_link(&temp, path)
                .map_err(|_| "backup publication failed; output is never replaced")?;
            checkpoint(true)?;
            directory
                .sync_all()
                .map_err(|_| "backup directory sync failed")?;
            Ok::<(), String>(())
        })();
        drop(file);
        let removed = std::fs::remove_file(&temp)
            .and_then(|()| directory.sync_all())
            .map_err(|_| "backup temporary file cleanup or directory sync failed".to_string());
        result.and(removed)
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn publication_is_no_clobber_idempotent_and_removes_temporary_file() {
            let dir = tempfile::tempdir().unwrap();
            let output = dir.path().join("backup");
            publish(&output, b"canonical").unwrap();
            publish(&output, b"canonical").unwrap();
            assert!(publish(&output, b"different").is_err());
            assert_eq!(std::fs::read(&output).unwrap(), b"canonical");
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        }
        #[test]
        fn interrupted_publication_is_absent_or_exact_and_retryable() {
            for after_link in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let output = dir.path().join("backup");
                let result = publish_checked(&output, b"canonical", |published| {
                    if published == after_link {
                        Err("injected publication interruption".into())
                    } else {
                        Ok(())
                    }
                });
                assert!(result.is_err());
                assert_eq!(output.exists(), after_link);
                if after_link {
                    assert_eq!(std::fs::read(&output).unwrap(), b"canonical");
                }
                publish(&output, b"canonical").unwrap();
                assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
            }
        }
    }
}
