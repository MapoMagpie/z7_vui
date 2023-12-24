use std::{process::Stdio, time::Duration};

use log::info;
use ropey::Rope;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{ChildStdin, ChildStdout, Command},
    time::sleep,
    try_join,
};

pub struct Z7 {
    document: Rope,
}

impl Z7 {
    pub fn new() -> Self {
        let rope = Rope::new();
        Self { document: rope }
    }
    pub async fn execute_list(&self, filename: &str) -> tokio::io::Result<()> {
        // let fi = fs::File::create("in")?;
        let mut child = Command::new("7z")
            .args(["l", filename])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;

        async fn read_to_string(mut pipe: Option<ChildStdout>) -> tokio::io::Result<String> {
            let mut str = String::new();
            if let Some(pipe) = pipe.as_mut() {
                pipe.read_to_string(&mut str).await.unwrap();
            }
            Ok(str)
        }
        async fn write_password(mut pipe: Option<ChildStdin>) -> tokio::io::Result<()> {
            sleep(Duration::from_secs(5)).await;
            if let Some(pipe) = pipe.as_mut() {
                info!("writing password");
                pipe.write_all(b"test").await?;
                info!("writed password");
            }
            Ok(())
        }
        match try_join!(
            read_to_string(child.stdout.take()),
            write_password(child.stdin.take())
        ) {
            Ok((output, _)) => {
                info!("output: {}", output);
            }
            Err(e) => {
                info!("error: {}", e);
            }
        }
        child.wait().await?;
        Ok(())
    }
}
