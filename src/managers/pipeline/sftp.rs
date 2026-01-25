use crate::errors::ToolError;
use crate::managers::ssh::ensure_remote_dir;
use bytes::Bytes;
use serde_json::Value;
use ssh2::{OpenFlags, OpenType};
use std::io::{Read, Write};
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

pub(super) struct OpenedSftpStream {
    pub(super) reader: DuplexStream,
    pub(super) completion: tokio::task::JoinHandle<Result<(), ToolError>>,
}

impl super::PipelineManager {
    pub(super) async fn open_sftp_stream(
        &self,
        sftp_args: &Value,
    ) -> Result<OpenedSftpStream, ToolError> {
        if !sftp_args.is_object() {
            return Err(ToolError::invalid_params("sftp config is required"));
        }
        let remote_path = self.validation.ensure_string(
            sftp_args.get("remote_path").unwrap_or(&Value::Null),
            "remote_path",
            true,
        )?;

        let args = sftp_args.clone();
        let ssh_manager = self.ssh_manager.clone();
        let (mut writer, reader) = tokio::io::duplex(64 * 1024);

        let completion = tokio::spawn(async move {
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Bytes>(8);
            let remote_clone = remote_path.clone();
            let read_task = tokio::spawn(async move {
                ssh_manager
                    .with_sftp(&args, move |sftp| {
                        let mut file = sftp
                            .open(Path::new(&remote_clone))
                            .map_err(|err| ToolError::internal(err.to_string()))?;
                        let mut buf = [0u8; 64 * 1024];
                        loop {
                            let n = file
                                .read(&mut buf)
                                .map_err(|err| ToolError::internal(err.to_string()))?;
                            if n == 0 {
                                break;
                            }
                            if tx.blocking_send(Bytes::copy_from_slice(&buf[..n])).is_err() {
                                break;
                            }
                        }
                        Ok(())
                    })
                    .await
            });

            while let Some(chunk) = rx.recv().await {
                if writer.write_all(&chunk).await.is_err() {
                    break;
                }
            }
            let _ = writer.shutdown().await;

            read_task
                .await
                .map_err(|_| ToolError::internal("SFTP read task failed"))??;
            Ok(())
        });

        Ok(OpenedSftpStream { reader, completion })
    }

    pub(super) async fn upload_stream_to_sftp(
        &self,
        reader: &mut DuplexStream,
        sftp_args: &Value,
    ) -> Result<Value, ToolError> {
        if !sftp_args.is_object() {
            return Err(ToolError::invalid_params("sftp config is required"));
        }

        let remote_path = self.validation.ensure_string(
            sftp_args.get("remote_path").unwrap_or(&Value::Null),
            "remote_path",
            true,
        )?;
        let overwrite = sftp_args
            .get("overwrite")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mkdirs = sftp_args
            .get("mkdirs")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let args = sftp_args.clone();
        let ssh_manager = self.ssh_manager.clone();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Bytes>(8);
        let remote_clone = remote_path.clone();

        let write_task = tokio::spawn(async move {
            ssh_manager
                .with_sftp(&args, move |sftp| {
                    if !overwrite && sftp.stat(Path::new(&remote_clone)).is_ok() {
                        return Err(ToolError::conflict(format!(
                            "Remote path already exists: {}",
                            remote_clone
                        ))
                        .with_hint("Set overwrite=true to replace it."));
                    }
                    if mkdirs {
                        ensure_remote_dir(sftp, &remote_clone)?;
                    }

                    let mut remote_file = sftp
                        .open_mode(
                            Path::new(&remote_clone),
                            OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
                            0o600,
                            OpenType::File,
                        )
                        .map_err(|err| ToolError::internal(err.to_string()))?;

                    while let Some(chunk) = rx.blocking_recv() {
                        remote_file
                            .write_all(&chunk)
                            .map_err(|err| ToolError::internal(err.to_string()))?;
                    }
                    Ok(())
                })
                .await
        });

        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = reader.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            if tx.send(Bytes::copy_from_slice(&buf[..n])).await.is_err() {
                break;
            }
        }
        drop(tx);

        write_task
            .await
            .map_err(|_| ToolError::internal("SFTP upload task failed"))??;

        Ok(serde_json::json!({"success": true, "remote_path": remote_path}))
    }
}
